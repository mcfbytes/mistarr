//! The streaming decoder: hunks in order, frames routed to per-track hashers; see
//! `docs/CHD.md` "Decoding".

use std::io::{Read, Seek, SeekFrom};

use sha1::Digest as _;

use super::codec::{self, Codec, Codecs};
use super::header::Header;
use super::layout::{Layout, TrackKind};
use super::map::{self, MapEntry};
use super::{corrupt, crc16, fail, ChdError, Unidentifiable, FRAME_BYTES, SECTOR_BYTES};
use crate::hash::Hashers;
use crate::HashSet;

const FRAME: usize = FRAME_BYTES as usize;
const SECTOR: usize = SECTOR_BYTES as usize;

/// Progress of a [`Decoder`] after a [`Decoder::step`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// `done` of `total` hunks are decoded.
    More {
        /// Hunks decoded so far.
        done: u64,
        /// Hunks in the image.
        total: u64,
    },
    /// Every hunk is decoded; call [`Decoder::finish`].
    Done,
}

/// Reads hunks from the file and decodes them, sharing one set of buffers.
struct HunkSource<R> {
    r: R,
    header: Header,
    codecs: [Option<Codec>; 4],
    state: Codecs,
    compressed: Vec<u8>,
}

impl<R: Read + Seek> HunkSource<R> {
    /// Decodes the stored hunk `index` described by `entry` into `out`.
    fn stored(&mut self, index: u64, entry: MapEntry, out: &mut [u8]) -> Result<(), ChdError> {
        match entry {
            MapEntry::Codec {
                offset,
                len,
                crc,
                slot,
            } => {
                let codec = self.codecs[usize::from(slot)]
                    .ok_or_else(|| fail(Unidentifiable::Codec, format!("hunk {index} codec")))?;
                let input = self
                    .compressed
                    .get_mut(..len as usize)
                    .ok_or_else(|| corrupt(format!("hunk {index} length")))?;
                self.r.seek(SeekFrom::Start(offset))?;
                self.r.read_exact(input)?;
                self.state
                    .decode(codec, input, out)
                    .map_err(|e| with_hunk(e, index))?;
                check_crc(index, out, crc)
            }
            MapEntry::Raw { offset, crc } => {
                self.r.seek(SeekFrom::Start(offset))?;
                self.r.read_exact(out)?;
                match crc {
                    Some(crc) => check_crc(index, out, crc),
                    None => Ok(()),
                }
            }
            MapEntry::Zero => {
                out.fill(0);
                Ok(())
            }
            MapEntry::Copy(_) => Err(corrupt(format!("hunk {index} copies a copy"))),
        }
    }
}

fn with_hunk(e: ChdError, index: u64) -> ChdError {
    match e {
        ChdError::Unidentifiable { reason, detail } => ChdError::Unidentifiable {
            reason,
            detail: format!("hunk {index}: {detail}"),
        },
        other @ ChdError::Io(_) => other,
    }
}

fn check_crc(index: u64, data: &[u8], crc: u16) -> Result<(), ChdError> {
    if crc16(data) == crc {
        Ok(())
    } else {
        Err(fail(
            Unidentifiable::Checksum,
            format!("hunk {index} CRC16"),
        ))
    }
}

/// Decodes a CD image hunk by hunk into per-track CRC32, MD5 and SHA1 of the rebuilt
/// `.bin` files, holding at most [`decode_budget`] bytes of heap.
pub struct Decoder<R> {
    src: HunkSource<R>,
    layout: Layout,
    map: Vec<MapEntry>,
    out: Vec<u8>,
    memo: Option<(u64, Vec<u8>)>,
    next: u64,
    raw: sha1::Sha1,
    track: usize,
    hashers: Option<Hashers>,
    done: Vec<HashSet>,
}

impl<R: Read + Seek> Decoder<R> {
    /// Reads and checks the map, and allocates every buffer for `header`.
    ///
    /// # Errors
    ///
    /// [`ChdError::Unidentifiable`] when the map is damaged or names an unsupported codec
    /// or a parent, [`ChdError::Io`] when the reader fails.
    pub fn new(mut r: R, header: Header, layout: Layout) -> Result<Self, ChdError> {
        let file_size = r.seek(SeekFrom::End(0))?;
        let mut map = map::read_map(&mut r, &header, file_size)?;
        map::flatten_copies(&mut map);
        let hunk = header.hunk_bytes as usize;
        let codecs = header.compressors.map(|c| c.and_then(Codec::from_fourcc));
        let frames = (header.hunk_bytes / FRAME_BYTES) as usize;
        Ok(Self {
            src: HunkSource {
                r,
                codecs,
                state: Codecs::new(frames),
                compressed: vec![0u8; hunk],
                header,
            },
            layout,
            map,
            out: vec![0u8; hunk],
            memo: None,
            next: 0,
            raw: sha1::Sha1::new(),
            track: 0,
            hashers: None,
            done: Vec::new(),
        })
    }

    /// Decodes up to `max_hunks` more hunks; returns at a hunk boundary.
    ///
    /// # Errors
    ///
    /// [`ChdError::Unidentifiable`] with `Corrupt`, `Checksum` or `Codec` for a bad hunk,
    /// [`ChdError::Io`] when the reader fails.
    pub fn step(&mut self, max_hunks: u32) -> Result<Step, ChdError> {
        let total = self.map.len() as u64;
        for _ in 0..max_hunks {
            if self.next >= total {
                break;
            }
            self.decode_hunk(self.next)?;
            self.route(self.next);
            self.next += 1;
        }
        Ok(if self.next >= total {
            Step::Done
        } else {
            Step::More {
                done: self.next,
                total,
            }
        })
    }

    /// Checks the whole image was decoded and matches its raw and combined SHA1, then
    /// returns one [`HashSet`] per track.
    ///
    /// # Errors
    ///
    /// `Corrupt` when hunks remain, `Checksum` when a header SHA1 does not match.
    pub fn finish(self) -> Result<Vec<HashSet>, ChdError> {
        if self.next < self.map.len() as u64 || self.done.len() != self.layout.tracks().len() {
            return Err(corrupt("the image was not decoded to its end"));
        }
        let raw: [u8; 20] = self.raw.finalize().into();
        if raw != self.src.header.raw_sha1.0 {
            return Err(fail(Unidentifiable::Checksum, "raw SHA1"));
        }
        let mut records = self.layout.checksums().to_vec();
        records.sort_unstable();
        let mut combined = sha1::Sha1::new();
        combined.update(raw);
        for r in &records {
            combined.update(r);
        }
        let combined: [u8; 20] = combined.finalize().into();
        if combined != self.src.header.sha1.0 {
            return Err(fail(Unidentifiable::Checksum, "combined SHA1"));
        }
        Ok(self.done)
    }

    /// Heap bytes the decoder holds now.
    #[must_use]
    pub fn heap_bytes(&self) -> usize {
        self.map.capacity() * std::mem::size_of::<MapEntry>()
            + self.out.capacity()
            + self.src.compressed.capacity()
            + self.memo.as_ref().map_or(0, |(_, m)| m.capacity())
            + self.src.state.heap_bytes()
    }

    fn decode_hunk(&mut self, index: u64) -> Result<(), ChdError> {
        let entry = self.entry(index)?;
        let MapEntry::Copy(mut target) = entry else {
            return self.src.stored(index, entry, &mut self.out);
        };
        // The map was flattened, so a copy's target is stored; the loop guards that.
        let mut target_entry = self.entry(u64::from(target))?;
        while let MapEntry::Copy(t) = target_entry {
            if t >= target {
                return Err(corrupt(format!("hunk {index} copies in a loop")));
            }
            target = t;
            target_entry = self.entry(u64::from(t))?;
        }
        let target = u64::from(target);
        if self.memo.as_ref().is_none_or(|(i, _)| *i != target) {
            let mut buf = match self.memo.take() {
                Some((_, buf)) => buf,
                None => vec![0u8; self.out.len()],
            };
            let result = self.src.stored(target, target_entry, &mut buf);
            self.memo = Some((u64::MAX, buf));
            result?;
            if let Some(m) = self.memo.as_mut() {
                m.0 = target;
            }
        }
        if let Some((_, buf)) = &self.memo {
            self.out.copy_from_slice(buf);
        }
        Ok(())
    }

    fn entry(&self, index: u64) -> Result<MapEntry, ChdError> {
        usize::try_from(index)
            .ok()
            .and_then(|i| self.map.get(i))
            .copied()
            .ok_or_else(|| corrupt(format!("hunk {index} is not in the map")))
    }

    /// Hashes hunk `index`: all of it into the raw SHA1, its track frames into their tracks.
    fn route(&mut self, index: u64) {
        let hunk = u64::from(self.src.header.hunk_bytes);
        let start = index * hunk;
        let valid = usize::try_from(
            self.src
                .header
                .logical_bytes
                .saturating_sub(start)
                .min(hunk),
        )
        .unwrap_or(0);
        self.raw.update(&self.out[..valid]);
        let first = start / u64::from(FRAME_BYTES);
        let mut swapped = [0u8; SECTOR];
        for (f, frame) in self.out[..valid].as_chunks::<FRAME>().0.iter().enumerate() {
            let global = first + f as u64;
            let Some(track) = self.layout.tracks().get(self.track) else {
                return;
            };
            if global < track.start_frame {
                continue;
            }
            let hashers = self.hashers.get_or_insert_with(Hashers::new);
            let sector = &frame[..SECTOR];
            if track.kind == TrackKind::Audio {
                for (o, pair) in swapped
                    .as_chunks_mut::<2>()
                    .0
                    .iter_mut()
                    .zip(sector.as_chunks::<2>().0)
                {
                    o[0] = pair[1];
                    o[1] = pair[0];
                }
                hashers.update(&swapped);
            } else {
                hashers.update(sector);
            }
            if global + 1 == track.start_frame + u64::from(track.frames) {
                if let Some(h) = self.hashers.take() {
                    self.done.push(h.finish());
                }
                self.track += 1;
            }
        }
    }
}

/// Upper bound on the decoder's heap (buffers, map, transient map read, codec state) for `h`.
///
/// ```
/// # use mistarr_core::chd::{decode_budget, read_header};
/// # let mut b = [0u8; 124];
/// # b[..8].copy_from_slice(b"MComprHD");
/// # b[8..12].copy_from_slice(&124u32.to_be_bytes());
/// # b[12..16].copy_from_slice(&5u32.to_be_bytes());
/// # b[16..20].copy_from_slice(b"cdlz");
/// # b[32..40].copy_from_slice(&(300_000u64 * 2448).to_be_bytes());
/// # b[56..60].copy_from_slice(&(8u32 * 2448).to_be_bytes());
/// # b[60..64].copy_from_slice(&2448u32.to_be_bytes());
/// let h = read_header(&b[..]).unwrap();
/// assert!(decode_budget(&h) < 16 << 20);
/// ```
#[must_use]
pub fn decode_budget(h: &Header) -> usize {
    let hunk = h.hunk_bytes as usize;
    let hunks = usize::try_from(h.hunk_count()).unwrap_or(usize::MAX / 64);
    let frames = hunk / FRAME;
    let buffers = 3 * hunk + frames * FRAME;
    let map = hunks * std::mem::size_of::<MapEntry>();
    let transient = if h.uncompressed() {
        4 * hunks.min(4096)
    } else {
        usize::try_from(map::max_map_bytes(h.hunk_count())).unwrap_or(usize::MAX / 64) + hunks
    };
    let lzma = hunk + frames * SECTOR + 2 * codec::LZMA_STATE;
    let zstd = codec::zstd_heap(hunk);
    let codecs = codec::INFLATE_STATE + lzma + zstd + codec::FLAC_BUFFER;
    buffers + map + transient + codecs
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation)] // synthetic test bytes wrap on purpose
mod tests {
    use super::*;
    use crate::chd::header::tests::sample;
    use crate::chd::layout::pad4;
    use crate::chd::layout::tests::with_meta;
    use crate::chd::{read_header, read_layout, MAX_FRAMES, MAX_HUNK_BYTES};
    use crate::hash::{hash_reader, HeaderRule};
    use proptest::prelude::*;
    use std::io::Cursor;

    struct Spec {
        kind: &'static str,
        frames: u32,
        pregap: u32,
    }

    /// Frame `i` of a track: distinct bytes, plus subcode.
    fn frame(track: usize, i: u32) -> [u8; FRAME] {
        let mut f = [0u8; FRAME];
        for (j, b) in f.iter_mut().enumerate() {
            *b = (j as u32)
                .wrapping_mul(31)
                .wrapping_add(i * 7 + track as u32 * 101) as u8;
        }
        f
    }

    /// An uncompressed image of `specs` in hunks of `fpb` frames, and each track's `.bin`.
    fn image(specs: &[Spec], fpb: u32) -> (Vec<u8>, Vec<Vec<u8>>) {
        let mut logical = Vec::new();
        let mut bins = Vec::new();
        let mut texts = Vec::new();
        for (t, s) in specs.iter().enumerate() {
            let mut bin = Vec::new();
            for i in 0..s.frames {
                let f = frame(t, i);
                logical.extend_from_slice(&f);
                if s.kind == "AUDIO" {
                    for p in f[..SECTOR].as_chunks::<2>().0 {
                        bin.extend_from_slice(&[p[1], p[0]]);
                    }
                } else {
                    bin.extend_from_slice(&f[..SECTOR]);
                }
            }
            logical.resize(logical.len() + pad4(s.frames) as usize * FRAME, 0);
            bins.push(bin);
            let pg = if s.pregap > 0 {
                format!("V{}", s.kind)
            } else {
                "MODE1".to_owned()
            };
            texts.push(format!(
                "TRACK:{} TYPE:{} SUBTYPE:NONE FRAMES:{} PREGAP:{} PGTYPE:{pg} PGSUB:RW POSTGAP:0\0",
                t + 1,
                s.kind,
                s.frames,
                s.pregap
            ));
        }
        let frames = (logical.len() / FRAME) as u64;
        let entries: Vec<(&[u8; 4], u8, &[u8])> =
            texts.iter().map(|t| (b"CHT2", 1u8, t.as_bytes())).collect();
        let (_, mut file) = with_meta(frames, &entries);
        let hunk = (fpb as usize) * FRAME;
        let hunks = logical.len().div_ceil(hunk);
        let map_at = file.len() as u64;
        let data_at = (map_at + 4 * hunks as u64).div_ceil(hunk as u64);
        for i in 0..hunks {
            file.extend_from_slice(
                &(u32::try_from(data_at).expect("u32") + i as u32).to_be_bytes(),
            );
        }
        file.resize(usize::try_from(data_at).expect("usize") * hunk, 0);
        let mut padded = logical.clone();
        padded.resize(hunks * hunk, 0);
        file.extend_from_slice(&padded);
        let raw: [u8; 20] = sha1::Sha1::digest(&logical).into();
        let mut records: Vec<[u8; 24]> = texts
            .iter()
            .map(|t| {
                let mut r = [0u8; 24];
                r[..4].copy_from_slice(b"CHT2");
                r[4..].copy_from_slice(&sha1::Sha1::digest(t.as_bytes()));
                r
            })
            .collect();
        records.sort_unstable();
        let mut all = sha1::Sha1::new();
        all.update(raw);
        for r in &records {
            all.update(r);
        }
        let combined: [u8; 20] = all.finalize().into();
        let mut head = sample(frames, fpb);
        head[16..32].fill(0);
        head[40..48].copy_from_slice(&map_at.to_be_bytes());
        head[64..84].copy_from_slice(&raw);
        head[84..104].copy_from_slice(&combined);
        file[..124].copy_from_slice(&head);
        (file, bins)
    }

    fn decode(file: &[u8], k: u32) -> Result<Vec<HashSet>, ChdError> {
        let h = read_header(file)?;
        let mut c = Cursor::new(file);
        let layout = read_layout(&mut c, &h)?;
        let mut d = Decoder::new(c, h, layout)?;
        while d.step(k)? != Step::Done {}
        d.finish()
    }

    fn specs() -> Vec<Spec> {
        vec![
            Spec {
                kind: "MODE1_RAW",
                frames: 7,
                pregap: 0,
            },
            Spec {
                kind: "AUDIO",
                frames: 9,
                pregap: 3,
            },
            Spec {
                kind: "MODE2_RAW",
                frames: 4,
                pregap: 0,
            },
            Spec {
                kind: "AUDIO",
                frames: 1,
                pregap: 0,
            },
        ]
    }

    #[test]
    fn an_uncompressed_image_rebuilds_every_track() {
        let (file, bins) = image(&specs(), 3);
        let got = decode(&file, 1).expect("decode");
        let want: Vec<HashSet> = bins
            .iter()
            .map(|b| hash_reader(&b[..], HeaderRule::None, None).expect("hash"))
            .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn a_flipped_byte_or_a_patched_raw_sha1_is_a_checksum_failure() {
        let (mut file, _) = image(&specs(), 3);
        let n = file.len();
        file[n - 5000] ^= 1;
        assert_eq!(
            decode(&file, 4).err().and_then(|e| e.reason()),
            Some(Unidentifiable::Checksum)
        );
        let (mut file, _) = image(&specs(), 3);
        file[70] ^= 1;
        assert_eq!(
            decode(&file, 4).err().and_then(|e| e.reason()),
            Some(Unidentifiable::Checksum)
        );
        let (mut file, _) = image(&specs(), 3);
        file[90] ^= 1;
        assert_eq!(
            decode(&file, 4).err().and_then(|e| e.reason()),
            Some(Unidentifiable::Checksum)
        );
    }

    #[test]
    fn finishing_early_is_corrupt() {
        let (file, _) = image(&specs(), 3);
        let h = read_header(&file[..]).expect("header");
        let mut c = Cursor::new(&file[..]);
        let layout = read_layout(&mut c, &h).expect("layout");
        let mut d = Decoder::new(c, h, layout).expect("decoder");
        assert!(matches!(
            d.step(1).expect("step"),
            Step::More { done: 1, .. }
        ));
        assert_eq!(
            d.finish().err().and_then(|e| e.reason()),
            Some(Unidentifiable::Corrupt)
        );
    }

    #[test]
    fn heap_stays_within_the_budget() {
        let (file, _) = image(&specs(), 5);
        let h = read_header(&file[..]).expect("header");
        let budget = decode_budget(&h);
        let mut c = Cursor::new(&file[..]);
        let layout = read_layout(&mut c, &h).expect("layout");
        let mut d = Decoder::new(c, h, layout).expect("decoder");
        while d.step(1).expect("step") != Step::Done {
            assert!(d.heap_bytes() <= budget);
        }
    }

    #[test]
    fn the_budget_holds_at_the_header_limits() {
        let limit = 24usize << 20;
        let one_frame = read_header(&sample(MAX_FRAMES, 1)[..]).expect("header");
        assert!(
            decode_budget(&one_frame) <= limit,
            "{}",
            decode_budget(&one_frame)
        );
        let big =
            read_header(&sample(MAX_FRAMES, MAX_HUNK_BYTES / FRAME_BYTES)[..]).expect("header");
        assert!(decode_budget(&big) <= limit, "{}", decode_budget(&big));
    }

    #[test]
    fn the_decoder_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<Decoder<std::fs::File>>();
    }

    proptest! {
        #[test]
        fn any_step_size_gives_the_same_hashes(k in 1u32..12, fpb in 1u32..6) {
            let (file, _) = image(&specs(), fpb);
            let once = decode(&file, u32::MAX).expect("decode");
            prop_assert_eq!(decode(&file, k).expect("decode"), once);
        }
    }
}
