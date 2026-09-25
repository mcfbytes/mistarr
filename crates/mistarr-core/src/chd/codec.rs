//! Hunk codecs over flate2, lzma-rs, ruzstd and claxon, with input guards; see `docs/CHD.md`
//! "Codecs".

use std::io::{Cursor, Read as _};

use flate2::{Decompress, FlushDecompress};
use lzma_rs::decompress::raw::{LzmaDecoder, LzmaParams, LzmaProperties};
use ruzstd::decoding::{BlockDecodingStrategy, FrameDecoder};

use super::header::FourCc;
use super::{corrupt, ecc, span, zstd, ChdError, FRAME_BYTES, SECTOR_BYTES};

/// A codec this decoder reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Codec {
    Zlib,
    Lzma,
    Zstd,
    Flac,
    CdZlib,
    CdLzma,
    CdZstd,
    CdFlac,
}

impl Codec {
    /// The codec a header slot names, `None` for one this decoder does not read.
    pub(crate) fn from_fourcc(f: FourCc) -> Option<Self> {
        Some(match &f.0 {
            b"zlib" => Self::Zlib,
            b"lzma" => Self::Lzma,
            b"zstd" => Self::Zstd,
            b"flac" => Self::Flac,
            b"cdzl" => Self::CdZlib,
            b"cdlz" => Self::CdLzma,
            b"cdzs" => Self::CdZstd,
            b"cdfl" => Self::CdFlac,
            _ => return None,
        })
    }
}

/// Subcode bytes in one frame.
const SUBCODE: usize = (FRAME_BYTES - SECTOR_BYTES) as usize;
const SECTOR: usize = SECTOR_BYTES as usize;
const FRAME: usize = FRAME_BYTES as usize;
const SYNC: [u8; 12] = [
    0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0,
];
/// Largest FLAC block claxon may allocate for: 65535 samples of up to 8 channels.
pub(crate) const FLAC_BUFFER: usize = 65_535 * 8 * 4;
/// Heap an idle inflater holds.
pub(crate) const INFLATE_STATE: usize = 64 << 10;
/// Heap ruzstd holds beyond its output buffer: block content, literals, up to 43,690
/// sequences of 12 bytes with room to grow, and its tables.
pub(crate) const ZSTD_STATE: usize = 1280 << 10;
/// Heap of one lzma-rs decoder's probability tables for lc 3.
pub(crate) const LZMA_STATE: usize = 16 << 10;

/// Reused codec state and scratch, sized for one image.
pub(crate) struct Codecs {
    inflate: Decompress,
    lzma: Vec<(usize, LzmaDecoder)>,
    zstd: FrameDecoder,
    /// Largest window or output length a zstd frame has used, `0` before the first.
    zstd_extent: usize,
    flac: Vec<i32>,
    /// Base sectors then subcode of one CD hunk.
    scratch: Vec<u8>,
}

impl Codecs {
    /// State for hunks of `frames` frames.
    pub(crate) fn new(frames: usize) -> Self {
        Self {
            inflate: Decompress::new(false),
            lzma: Vec::new(),
            zstd: FrameDecoder::new(),
            zstd_extent: 0,
            flac: Vec::new(),
            scratch: vec![0u8; frames * FRAME],
        }
    }

    /// Heap held by codec state and scratch now.
    pub(crate) fn heap_bytes(&self) -> usize {
        let lzma: usize = self.lzma.iter().map(|(dict, _)| dict + LZMA_STATE).sum();
        let zstd = if self.zstd_extent > 0 {
            zstd_heap(self.zstd_extent)
        } else {
            0
        };
        INFLATE_STATE + lzma + zstd + self.flac.capacity() * 4 + self.scratch.capacity()
    }

    /// Decodes `input` with `codec` into all of `out`, which is one hunk.
    pub(crate) fn decode(
        &mut self,
        codec: Codec,
        input: &[u8],
        out: &mut [u8],
    ) -> Result<(), ChdError> {
        match codec {
            Codec::Zlib => self.inflate(input, out),
            Codec::Lzma => self.lzma(input, out),
            Codec::Zstd => self.zstd(input, out),
            Codec::Flac => {
                let (&order, rest) = input
                    .split_first()
                    .ok_or_else(|| corrupt("empty FLAC hunk"))?;
                let big = match order {
                    b'B' => true,
                    b'L' => false,
                    _ => return Err(corrupt("FLAC byte order is neither L nor B")),
                };
                self.flac(rest, out, big).map(|_| ())
            }
            Codec::CdZlib | Codec::CdLzma | Codec::CdZstd => self.cd(codec, input, out),
            Codec::CdFlac => self.cd_flac(input, out),
        }
    }

    fn inflate(&mut self, input: &[u8], out: &mut [u8]) -> Result<(), ChdError> {
        self.inflate.reset(false);
        self.inflate
            .decompress(input, out, FlushDecompress::Finish)
            .map_err(|_| corrupt("deflate data are invalid"))?;
        if self.inflate.total_out() != out.len() as u64 {
            return Err(corrupt("deflate data have the wrong length"));
        }
        Ok(())
    }

    fn lzma(&mut self, input: &[u8], out: &mut [u8]) -> Result<(), ChdError> {
        let len = out.len();
        let at = if let Some(at) = self.lzma.iter().position(|(dict, _)| *dict == len) {
            at
        } else {
            let props = LzmaProperties {
                lc: 3,
                lp: 0,
                pb: 2,
            };
            let dict = u32::try_from(len).map_err(|_| corrupt("LZMA output length"))?;
            let params = LzmaParams::new(props, dict, Some(len as u64));
            let dec = LzmaDecoder::new(params, Some(len))
                .map_err(|_| corrupt("LZMA parameters are invalid"))?;
            self.lzma.push((len, dec));
            self.lzma.len() - 1
        };
        let dec = &mut self.lzma[at].1;
        dec.reset(Some(Some(len as u64)));
        let mut sink = &mut out[..];
        dec.decompress(&mut &input[..], &mut sink)
            .map_err(|_| corrupt("LZMA data are invalid"))?;
        if !sink.is_empty() {
            return Err(corrupt("LZMA data have the wrong length"));
        }
        Ok(())
    }

    fn zstd(&mut self, input: &[u8], out: &mut [u8]) -> Result<(), ChdError> {
        let window = zstd::check_frame(input, out.len())?;
        self.zstd_extent = self.zstd_extent.max(window).max(out.len());
        let mut src = input;
        self.zstd
            .init(&mut src)
            .map_err(|_| corrupt("zstd frame header is invalid"))?;
        // The frame was measured to decode to exactly `out.len()` bytes.
        self.zstd
            .decode_blocks(&mut src, BlockDecodingStrategy::All)
            .map_err(|_| corrupt("zstd data are invalid"))?;
        let mut filled = 0;
        while filled < out.len() {
            let n = self
                .zstd
                .read(&mut out[filled..])
                .map_err(|_| corrupt("zstd data are invalid"))?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        if filled != out.len() || self.zstd.can_collect() != 0 || !self.zstd.is_finished() {
            return Err(corrupt("zstd data have the wrong length"));
        }
        Ok(())
    }

    /// Decodes `len / 4` stereo samples into `out`; returns the bytes of `input` consumed.
    fn flac(&mut self, input: &[u8], out: &mut [u8], big_endian: bool) -> Result<usize, ChdError> {
        let total = out.len() / 4;
        let mut reader = claxon::frame::FrameReader::new(Cursor::new(input));
        let mut done = 0usize;
        while done < total {
            let buffer = std::mem::take(&mut self.flac);
            let block = reader
                .read_next_or_eof(buffer)
                .map_err(|_| corrupt("FLAC data are invalid"))?
                .ok_or_else(|| corrupt("FLAC data end early"))?;
            let samples = block.duration() as usize;
            if block.channels() != 2 || samples > total - done {
                return Err(corrupt("a FLAC block does not fit the hunk"));
            }
            for (i, (l, r)) in block.stereo_samples().enumerate() {
                let l = i16::try_from(l).map_err(|_| corrupt("a FLAC sample is over 16 bits"))?;
                let r = i16::try_from(r).map_err(|_| corrupt("a FLAC sample is over 16 bits"))?;
                let at = (done + i) * 4;
                let (lb, rb) = if big_endian {
                    (l.to_be_bytes(), r.to_be_bytes())
                } else {
                    (l.to_le_bytes(), r.to_le_bytes())
                };
                out[at..at + 2].copy_from_slice(&lb);
                out[at + 2..at + 4].copy_from_slice(&rb);
            }
            done += samples;
            self.flac = block.into_buffer();
        }
        usize::try_from(reader.into_inner().position()).map_err(|_| corrupt("FLAC position"))
    }

    /// `cdzl`, `cdlz` and `cdzs`: ECC flags, base length, base sectors, then subcode.
    fn cd(&mut self, codec: Codec, input: &[u8], out: &mut [u8]) -> Result<(), ChdError> {
        let frames = out.len() / FRAME;
        let ecc_bytes = frames.div_ceil(8);
        let len_bytes = if out.len() < 65_536 { 2 } else { 3 };
        let head = span(input, 0, ecc_bytes + len_bytes, "the CD hunk header")?;
        let base_len = head[ecc_bytes..]
            .iter()
            .fold(0usize, |v, &b| (v << 8) | usize::from(b));
        let base_in = span(input, ecc_bytes + len_bytes, base_len, "the CD base data")?;
        let sub_in = &input[ecc_bytes + len_bytes + base_len..];
        let mut scratch = std::mem::take(&mut self.scratch);
        let (base, sub) = scratch.split_at_mut(frames * SECTOR);
        let result = match codec {
            Codec::CdLzma => self
                .lzma(base_in, base)
                .and_then(|()| self.inflate(sub_in, sub)),
            Codec::CdZstd => self
                .zstd(base_in, base)
                .and_then(|()| self.zstd(sub_in, sub)),
            _ => self
                .inflate(base_in, base)
                .and_then(|()| self.inflate(sub_in, sub)),
        };
        if result.is_ok() {
            interleave(&scratch, frames, out);
            for f in 0..frames {
                if head[f / 8] & (1 << (f % 8)) != 0 {
                    let sector: &mut [u8; 2352] = (&mut out[f * FRAME..f * FRAME + SECTOR])
                        .try_into()
                        .map_err(|_| corrupt("sector"))?;
                    sector[..12].copy_from_slice(&SYNC);
                    ecc::generate(sector);
                }
            }
        }
        self.scratch = scratch;
        result
    }

    /// `cdfl`: big-endian FLAC sectors, then deflated subcode from where the FLAC data end.
    fn cd_flac(&mut self, input: &[u8], out: &mut [u8]) -> Result<(), ChdError> {
        let frames = out.len() / FRAME;
        let mut scratch = std::mem::take(&mut self.scratch);
        let (base, sub) = scratch.split_at_mut(frames * SECTOR);
        let result = self.flac(input, base, true).and_then(|used| {
            let rest = input.get(used..).ok_or_else(|| corrupt("FLAC position"))?;
            self.inflate(rest, sub)
        });
        if result.is_ok() {
            interleave(&scratch, frames, out);
        }
        self.scratch = scratch;
        result
    }
}

/// Rebuilds `frames` 2448-byte frames from base sectors followed by subcode.
fn interleave(scratch: &[u8], frames: usize, out: &mut [u8]) {
    let (base, sub) = scratch.split_at(frames * SECTOR);
    for f in 0..frames {
        out[f * FRAME..f * FRAME + SECTOR].copy_from_slice(&base[f * SECTOR..(f + 1) * SECTOR]);
        out[f * FRAME + SECTOR..(f + 1) * FRAME]
            .copy_from_slice(&sub[f * SUBCODE..(f + 1) * SUBCODE]);
    }
}

/// Heap ruzstd may hold for frames whose window and output are at most `extent`: its buffer
/// reserves the window, and while it grows the old and new buffers are both alive.
pub(crate) fn zstd_heap(extent: usize) -> usize {
    3 * extent.max(zstd::MIN_WINDOW_CAP).next_power_of_two() + ZSTD_STATE
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compress, Compression, FlushCompress};

    fn deflate(data: &[u8]) -> Vec<u8> {
        let mut c = Compress::new(Compression::default(), false);
        let mut out = Vec::with_capacity(data.len() + 64);
        c.compress_vec(data, &mut out, FlushCompress::Finish)
            .expect("deflate");
        out
    }

    #[test]
    fn raw_deflate_round_trips_and_checks_length() {
        let data: Vec<u8> = (0..5000u32).map(|i| (i % 7) as u8).collect();
        let packed = deflate(&data);
        let mut c = Codecs::new(1);
        let mut out = vec![0u8; data.len()];
        c.decode(Codec::Zlib, &packed, &mut out).expect("inflate");
        assert_eq!(out, data);
        let mut short = vec![0u8; data.len() + 1];
        assert!(c.decode(Codec::Zlib, &packed, &mut short).is_err());
        assert!(c.decode(Codec::Zlib, &[0xff, 0xff], &mut out).is_err());
    }

    /// A single-segment zstd frame holding `data` as one raw block.
    fn raw_zstd(data: &[u8], fcs: Option<u32>) -> Vec<u8> {
        let mut f = vec![0x28, 0xb5, 0x2f, 0xfd];
        if let Some(n) = fcs {
            f.push(0xa0); // FCS 4 bytes, single segment
            f.extend_from_slice(&n.to_le_bytes());
        } else {
            f.extend_from_slice(&[0x00, 0x08]); // window 2^11, no FCS
        }
        let header = (u32::try_from(data.len()).expect("len") << 3) | 1;
        f.extend_from_slice(&header.to_le_bytes()[..3]);
        f.extend_from_slice(data);
        f
    }

    #[test]
    fn zstd_accepts_both_header_forms_and_checks_the_size() {
        let data = vec![0x5a; 1500];
        let mut c = Codecs::new(1);
        let mut out = vec![0u8; 1500];
        c.decode(Codec::Zstd, &raw_zstd(&data, Some(1500)), &mut out)
            .expect("single segment");
        assert_eq!(out, data);
        out.fill(0);
        c.decode(Codec::Zstd, &raw_zstd(&data, None), &mut out)
            .expect("windowed, no FCS");
        assert_eq!(out, data);
        assert!(c
            .decode(Codec::Zstd, &raw_zstd(&data, Some(1499)), &mut out)
            .is_err());
        let mut long = vec![0u8; 1400];
        assert!(c
            .decode(Codec::Zstd, &raw_zstd(&data, None), &mut long)
            .is_err());
    }

    #[test]
    fn zstd_heap_covers_what_decoding_holds() {
        let data: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let frame = ruzstd::encoding::compress_to_vec(
            &data[..],
            ruzstd::encoding::CompressionLevel::Fastest,
        );
        let mut c = Codecs::new(1);
        let mut out = vec![0u8; data.len()];
        c.decode(Codec::Zstd, &frame, &mut out).expect("zstd");
        assert_eq!(out, data);
        assert!(c.heap_bytes() >= zstd_heap(data.len()));
        assert!(zstd_heap(523_872) < 4 << 20);
    }

    #[test]
    fn a_mismatched_lzma_length_is_corrupt() {
        let mut c = Codecs::new(1);
        let mut out = vec![0u8; 100];
        assert!(c
            .decode(Codec::Lzma, &[0, 0, 0, 0, 0, 0], &mut out)
            .is_err());
        assert!(c.decode(Codec::Lzma, &[], &mut out).is_err());
    }

    #[test]
    fn flac_needs_a_byte_order_and_valid_frames() {
        let mut c = Codecs::new(1);
        let mut out = vec![0u8; 16];
        assert!(c.decode(Codec::Flac, &[b'X', 0, 0], &mut out).is_err());
        assert!(c
            .decode(Codec::Flac, &[b'B', 0xff, 0xf8], &mut out)
            .is_err());
        assert!(c.decode(Codec::Flac, &[], &mut out).is_err());
    }

    #[test]
    fn a_cd_hunk_with_a_length_past_its_data_is_corrupt() {
        let mut c = Codecs::new(1);
        let mut out = vec![0u8; FRAME];
        // One ECC byte, then a base length of 0xffff with 2 bytes of data.
        assert!(c
            .decode(Codec::CdZlib, &[0, 0xff, 0xff, 1, 2], &mut out)
            .is_err());
        assert!(c.decode(Codec::CdZlib, &[0], &mut out).is_err());
    }

    #[test]
    fn a_cd_hunk_rebuilds_frames_sync_and_ecc() {
        let mut sector = [0u8; SECTOR];
        sector[..12].copy_from_slice(&SYNC);
        sector[15] = 1;
        sector[16..2064].fill(0x42);
        ecc::generate(&mut sector);
        let mut stripped = sector;
        stripped[..12].fill(0);
        stripped[0x81c..].fill(0);
        let sub = [0x77u8; SUBCODE];
        let base = deflate(&stripped);
        let mut hunk = vec![1u8];
        hunk.extend_from_slice(&u16::try_from(base.len()).expect("len").to_be_bytes());
        hunk.extend_from_slice(&base);
        hunk.extend_from_slice(&deflate(&sub));
        let mut c = Codecs::new(1);
        let mut out = vec![0u8; FRAME];
        c.decode(Codec::CdZlib, &hunk, &mut out).expect("cdzl");
        assert_eq!(&out[..SECTOR], &sector[..]);
        assert_eq!(&out[SECTOR..], &sub[..]);
        assert_eq!(&out[..4], [0, 0xff, 0xff, 0xff]);
    }

    #[test]
    fn fourccs_map_to_codecs() {
        assert_eq!(Codec::from_fourcc(FourCc(*b"cdfl")), Some(Codec::CdFlac));
        assert_eq!(Codec::from_fourcc(FourCc(*b"huff")), None);
        assert_eq!(Codec::from_fourcc(FourCc(*b"avhu")), None);
    }
}
