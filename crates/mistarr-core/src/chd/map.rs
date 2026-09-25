//! The v5 hunk map, compressed or plain; see `docs/CHD.md` "Map".

use std::io::{Read, Seek, SeekFrom};

use super::bits::BitReader;
use super::cdrom::crc16_update;
use super::codec::Codec;
use super::header::Header;
use super::huffman::Huffman;
use super::{corrupt, fail, ChdError, Unidentifiable, FRAME_BYTES};

/// Where one hunk's bytes come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MapEntry {
    /// `len` bytes at `offset`, compressed with codec slot `slot`.
    Codec {
        /// File offset.
        offset: u64,
        /// Compressed length.
        len: u32,
        /// CRC16 of the decoded hunk.
        crc: u16,
        /// Codec slot 0 to 3.
        slot: u8,
    },
    /// `hunk_bytes` stored at `offset`; `crc` is absent in an uncompressed image.
    Raw {
        /// File offset.
        offset: u64,
        /// CRC16 of the hunk.
        crc: Option<u16>,
    },
    /// The same bytes as an earlier hunk.
    Copy(u32),
    /// All zero bytes.
    Zero,
}

const RLE_SMALL: u8 = 7;
const RLE_LARGE: u8 = 8;
const NONE: u8 = 4;
const SELF: u8 = 5;
const PARENT: u8 = 6;
const SELF_0: u8 = 9;
const SELF_1: u8 = 10;
const PARENT_SELF: u8 = 11;
const PARENT_0: u8 = 12;
const PARENT_1: u8 = 13;

/// Reads the map of `h` from a file of `file_size` bytes and checks every entry against it.
pub(crate) fn read_map<R: Read + Seek>(
    r: &mut R,
    h: &Header,
    file_size: u64,
) -> Result<Vec<MapEntry>, ChdError> {
    r.seek(SeekFrom::Start(h.map_offset))?;
    if h.uncompressed() {
        read_plain(r, h, file_size)
    } else {
        read_compressed(r, h, file_size)
    }
}

fn read_plain<R: Read>(r: &mut R, h: &Header, file_size: u64) -> Result<Vec<MapEntry>, ChdError> {
    let hunks = usize::try_from(h.hunk_count()).map_err(|_| corrupt("hunk count"))?;
    let hunk = u64::from(h.hunk_bytes);
    let mut out = Vec::with_capacity(hunks);
    let mut chunk = vec![0u8; 4 * hunks.clamp(1, 4096)];
    while out.len() < hunks {
        let n = (hunks - out.len()).min(4096);
        r.read_exact(&mut chunk[..n * 4])?;
        for e in chunk[..n * 4].as_chunks::<4>().0 {
            let block = u64::from(u32::from_be_bytes([e[0], e[1], e[2], e[3]]));
            if block == 0 {
                out.push(MapEntry::Zero);
                continue;
            }
            let offset = block * hunk;
            if offset + hunk > file_size {
                return Err(corrupt(format!(
                    "hunk {} lies past the end of the file",
                    out.len()
                )));
            }
            out.push(MapEntry::Raw { offset, crc: None });
        }
    }
    Ok(out)
}

/// Points every copy at the stored hunk its chain ends on, in one pass: a copy's target
/// comes before it, so that target already points at a stored hunk.
pub(crate) fn flatten_copies(map: &mut [MapEntry]) {
    for i in 0..map.len() {
        if let MapEntry::Copy(t) = map[i] {
            if let Some(&MapEntry::Copy(end)) = map.get(t as usize) {
                map[i] = MapEntry::Copy(end);
            }
        }
    }
}

/// Bytes of compressed map accepted for `hunks` hunks: 56 bits a hunk plus the tree.
pub(crate) fn max_map_bytes(hunks: u64) -> u64 {
    hunks * 8 + 64
}

fn read_compressed<R: Read>(
    r: &mut R,
    h: &Header,
    file_size: u64,
) -> Result<Vec<MapEntry>, ChdError> {
    let hunks = h.hunk_count();
    let mut head = [0u8; 16];
    r.read_exact(&mut head)?;
    let map_bytes = u64::from(u32::from_be_bytes([head[0], head[1], head[2], head[3]]));
    let first = u64::from_be_bytes([0, 0, head[4], head[5], head[6], head[7], head[8], head[9]]);
    let map_crc = u16::from_be_bytes([head[10], head[11]]);
    let (length_bits, self_bits, parent_bits) = (head[12], head[13], head[14]);
    if map_bytes > max_map_bytes(hunks) {
        return Err(corrupt("the map is larger than its hunks need"));
    }
    if length_bits > 32 || self_bits > 32 || parent_bits > 32 {
        return Err(corrupt("a map field is wider than 32 bits"));
    }
    let mut raw = vec![0u8; usize::try_from(map_bytes).map_err(|_| corrupt("map size"))?];
    r.read_exact(&mut raw)?;
    let mut bits = BitReader::new(&raw);
    let tree = Huffman::import_rle(&mut bits)?;

    let count = usize::try_from(hunks).map_err(|_| corrupt("hunk count"))?;
    let types = read_types(&tree, &mut bits, count)?;

    let per_hunk = u64::from(h.hunk_bytes / FRAME_BYTES);
    let mut out = Vec::with_capacity(count);
    let mut problem: Option<ChdError> = None;
    let (mut cur, mut last_self, mut last_parent) = (first, 0u64, 0u64);
    let mut crc = 0xffff;
    for (i, &t) in types.iter().enumerate() {
        let index = i as u64;
        let (mut kind, mut len, mut offset, mut hcrc) = (t, 0u32, cur, 0u16);
        match t {
            0..=3 => {
                len = bits.read(u32::from(length_bits));
                hcrc = bits.read_u16();
                cur = cur.saturating_add(u64::from(len));
            }
            NONE => {
                len = h.hunk_bytes;
                hcrc = bits.read_u16();
                cur = cur.saturating_add(u64::from(len));
            }
            SELF => {
                last_self = u64::from(bits.read(u32::from(self_bits)));
                offset = last_self;
            }
            PARENT => {
                last_parent = u64::from(bits.read(u32::from(parent_bits)));
                offset = last_parent;
            }
            SELF_0 | SELF_1 => {
                if t == SELF_1 {
                    last_self += 1;
                }
                kind = SELF;
                offset = last_self;
            }
            PARENT_SELF => {
                kind = PARENT;
                last_parent = index * per_hunk;
                offset = last_parent;
            }
            PARENT_0 | PARENT_1 => {
                if t == PARENT_1 {
                    last_parent += per_hunk;
                }
                kind = PARENT;
                offset = last_parent;
            }
            _ => {}
        }
        let mut expanded = [0u8; 12];
        expanded[0] = kind;
        expanded[1..4].copy_from_slice(&len.to_be_bytes()[1..]);
        expanded[4..10].copy_from_slice(&offset.to_be_bytes()[2..]);
        expanded[10..12].copy_from_slice(&hcrc.to_be_bytes());
        crc = crc16_update(crc, &expanded);
        let entry = check(h, file_size, index, kind, offset, len, hcrc);
        match entry {
            Ok(e) => out.push(e),
            Err(e) => {
                problem.get_or_insert(e);
                out.push(MapEntry::Zero);
            }
        }
    }
    if bits.overflowed() {
        return Err(corrupt("the map ends early"));
    }
    if crc != map_crc {
        return Err(corrupt("the map checksum does not match"));
    }
    match problem {
        Some(e) => Err(e),
        None => Ok(out),
    }
}

/// One type code per hunk, with the RLE codes expanded.
fn read_types(tree: &Huffman, bits: &mut BitReader<'_>, count: usize) -> Result<Vec<u8>, ChdError> {
    let mut types = Vec::with_capacity(count);
    let (mut last, mut rep) = (0u8, 0u32);
    for _ in 0..count {
        if rep > 0 {
            types.push(last);
            rep -= 1;
            continue;
        }
        match tree.decode(bits)? {
            RLE_SMALL => {
                types.push(last);
                rep = 2 + u32::from(tree.decode(bits)?);
            }
            RLE_LARGE => {
                types.push(last);
                let hi = u32::from(tree.decode(bits)?);
                rep = 2 + 16 + (hi << 4) + u32::from(tree.decode(bits)?);
            }
            v => {
                types.push(v);
                last = v;
            }
        }
    }
    Ok(types)
}

/// The entry for hunk `index` of expanded type `kind`, checked against the header and file.
fn check(
    h: &Header,
    file_size: u64,
    index: u64,
    kind: u8,
    offset: u64,
    len: u32,
    crc: u16,
) -> Result<MapEntry, ChdError> {
    let fits = |bytes: u32| {
        offset
            .checked_add(u64::from(bytes))
            .is_some_and(|e| e <= file_size)
    };
    match kind {
        0..=3 => {
            let slot = h.compressors[usize::from(kind)];
            if slot.is_none_or(|f| Codec::from_fourcc(f).is_none()) {
                return Err(fail(
                    Unidentifiable::Codec,
                    format!("hunk {index} uses codec slot {kind}"),
                ));
            }
            if len == 0 || len > h.hunk_bytes || !fits(len) {
                return Err(corrupt(format!("hunk {index} has a bad length or offset")));
            }
            Ok(MapEntry::Codec {
                offset,
                len,
                crc,
                slot: kind,
            })
        }
        NONE => {
            if !fits(h.hunk_bytes) {
                return Err(corrupt(format!(
                    "hunk {index} lies past the end of the file"
                )));
            }
            Ok(MapEntry::Raw {
                offset,
                crc: Some(crc),
            })
        }
        SELF => {
            if offset >= index {
                return Err(corrupt(format!(
                    "hunk {index} copies a hunk at or after itself"
                )));
            }
            Ok(MapEntry::Copy(
                u32::try_from(offset).map_err(|_| corrupt("copy target"))?,
            ))
        }
        PARENT => Err(fail(
            Unidentifiable::Parent,
            format!("hunk {index} is in a parent"),
        )),
        _ => Err(corrupt(format!("hunk {index} has unknown type {kind}"))),
    }
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation)] // synthetic test bytes wrap on purpose
pub(crate) mod tests {
    use super::*;
    use crate::chd::cdrom::crc16;
    use crate::chd::header::tests::sample;
    use crate::chd::read_header;
    use std::io::Cursor;

    /// MSB-first bit writer for hand-built maps.
    #[derive(Default)]
    pub(crate) struct Bits {
        pub(crate) bytes: Vec<u8>,
        used: u32,
    }

    impl Bits {
        pub(crate) fn put(&mut self, value: u64, n: u32) {
            for i in (0..n).rev() {
                if self.used.is_multiple_of(8) {
                    self.bytes.push(0);
                }
                let bit = u8::from((value >> i) & 1 == 1);
                let last = self.bytes.len() - 1;
                self.bytes[last] |= bit << (7 - self.used % 8);
                self.used += 1;
            }
        }
    }

    /// An expanded entry as the map CRC covers it.
    fn expanded(kind: u8, len: u32, offset: u64, crc: u16) -> [u8; 12] {
        let mut e = [0u8; 12];
        e[0] = kind;
        e[1..4].copy_from_slice(&len.to_be_bytes()[1..]);
        e[4..10].copy_from_slice(&offset.to_be_bytes()[2..]);
        e[10..12].copy_from_slice(&crc.to_be_bytes());
        e
    }

    /// A bit stream that starts with the uniform tree `1, 4, 13`: every type is 4 bits.
    fn body() -> Bits {
        let mut b = Bits::default();
        b.put(0x14d, 12);
        b
    }

    /// A header of `hunks` one-frame hunks with a cdzl slot, and a map at 124 holding
    /// `bits` and the CRC of the expanded `entries`.
    fn image(hunks: u64, bits: &Bits, entries: &[[u8; 12]], lens: (u8, u8)) -> (Header, Vec<u8>) {
        let mut hb = sample(hunks, 1);
        hb[16..20].copy_from_slice(b"cdzl");
        hb[20..24].fill(0);
        hb[40..48].copy_from_slice(&124u64.to_be_bytes());
        let h = read_header(&hb[..]).expect("header");
        let crc_in: Vec<u8> = entries.iter().flatten().copied().collect();
        let mut file = hb.to_vec();
        file.extend_from_slice(&u32::try_from(bits.bytes.len()).expect("len").to_be_bytes());
        file.extend_from_slice(&200u64.to_be_bytes()[2..]);
        file.extend_from_slice(&crc16(&crc_in).to_be_bytes());
        file.extend_from_slice(&[lens.0, lens.1, 0, 0]);
        file.extend_from_slice(&bits.bytes);
        file.resize(file.len() + 100_000, 0);
        (h, file)
    }

    fn map_of(h: &Header, file: &[u8]) -> Result<Vec<MapEntry>, ChdError> {
        read_map(&mut Cursor::new(file), h, file.len() as u64)
    }

    #[test]
    fn literal_types_and_none_without_a_length() {
        let mut body = body();
        body.put(0, 4); // cdzl
        body.put(NONE.into(), 4);
        body.put(0, 4);
        body.put(100, 16); // len
        body.put(0xabcd, 16);
        body.put(0x1234, 16); // NONE: crc only
        body.put(60, 16);
        body.put(0x0f0f, 16);
        let entries = [
            expanded(0, 100, 200, 0xabcd),
            expanded(NONE, 2448, 300, 0x1234),
            expanded(0, 60, 2748, 0x0f0f),
        ];
        let (h, file) = image(3, &body, &entries, (16, 0));
        let map = map_of(&h, &file).expect("map");
        assert_eq!(
            map,
            [
                MapEntry::Codec {
                    offset: 200,
                    len: 100,
                    crc: 0xabcd,
                    slot: 0
                },
                MapEntry::Raw {
                    offset: 300,
                    crc: Some(0x1234)
                },
                MapEntry::Codec {
                    offset: 2748,
                    len: 60,
                    crc: 0x0f0f,
                    slot: 0
                },
            ]
        );
    }

    /// A NONE hunk followed by `run` copies of its type through RLE.
    fn rle_image(run: u64) -> (Header, Vec<u8>) {
        let mut body = body();
        body.put(NONE.into(), 4);
        let mut left = run - 1;
        while left > 0 {
            if left >= 19 {
                let n = left.min(274);
                body.put(RLE_LARGE.into(), 4);
                body.put((n - 19) >> 4, 4);
                body.put((n - 19) & 15, 4);
                left -= n;
            } else if left >= 3 {
                body.put(RLE_SMALL.into(), 4);
                body.put(left - 3, 4);
                left = 0;
            } else {
                body.put(NONE.into(), 4);
                left -= 1;
            }
        }
        let mut entries = Vec::new();
        for i in 0..run {
            body.put(i & 0xffff, 16);
            entries.push(expanded(NONE, 2448, 200 + i * 2448, (i & 0xffff) as u16));
        }
        image(run, &body, &entries, (0, 0))
    }

    #[test]
    fn rle_small_and_large_runs_repeat_the_last_type() {
        // One literal then RLE_SMALL(0): 1 + 3 hunks; RLE_LARGE(0,0): 1 + 19; (15,15): 1 + 274.
        for run in [4u64, 20, 275] {
            let (h, mut file) = rle_image(run);
            file.resize(200 + (run as usize) * 2448, 0);
            let map = map_of(&h, &file).expect("map");
            assert_eq!(map.len() as u64, run);
            assert!(map.iter().enumerate().all(|(i, e)| *e
                == MapEntry::Raw {
                    offset: 200 + i as u64 * 2448,
                    crc: Some(i as u16)
                }));
        }
    }

    #[test]
    fn rle_counts_match_the_docs() {
        let mut body = body();
        body.put(NONE.into(), 4);
        body.put(RLE_SMALL.into(), 4);
        body.put(0, 4);
        let mut entries = Vec::new();
        for i in 0..4u64 {
            body.put(0, 16);
            entries.push(expanded(NONE, 2448, 200 + i * 2448, 0));
        }
        let (h, file) = image(4, &body, &entries, (0, 0));
        assert_eq!(
            map_of(&h, &file).expect("map").len(),
            4,
            "RLE_SMALL 0 is a run of 3"
        );
        let (h, file) = rle_image(19 + 1);
        assert_eq!(
            map_of(&h, &file).expect("map").len(),
            20,
            "RLE_LARGE 0,0 is a run of 19"
        );
    }

    #[test]
    fn self_references_resolve_to_earlier_hunks() {
        let mut body = body();
        for t in [NONE, NONE, SELF, SELF_0, SELF_1, SELF_1] {
            body.put(t.into(), 4);
        }
        body.put(1, 16);
        body.put(2, 16);
        body.put(0, 3); // SELF target 0, selfbits 3
        let entries = [
            expanded(NONE, 2448, 200, 1),
            expanded(NONE, 2448, 2648, 2),
            expanded(SELF, 0, 0, 0),
            expanded(SELF, 0, 0, 0),
            expanded(SELF, 0, 1, 0),
            expanded(SELF, 0, 2, 0),
        ];
        let (h, file) = image(6, &body, &entries, (0, 3));
        let map = map_of(&h, &file).expect("map");
        assert_eq!(
            &map[2..],
            &[
                MapEntry::Copy(0),
                MapEntry::Copy(0),
                MapEntry::Copy(1),
                MapEntry::Copy(2)
            ]
        );
    }

    #[test]
    fn copy_chains_flatten_to_their_stored_hunk() {
        let raw = MapEntry::Raw {
            offset: 0,
            crc: None,
        };
        let mut map = vec![raw, MapEntry::Copy(0)];
        map.extend((1..6).map(MapEntry::Copy));
        map.push(raw);
        map.push(MapEntry::Copy(7));
        flatten_copies(&mut map);
        assert!(map[1..7].iter().all(|e| *e == MapEntry::Copy(0)));
        assert_eq!(map[8], MapEntry::Copy(7));
    }

    fn one_type(
        kind: u8,
        fields: &[(u64, u32)],
        entry: [u8; 12],
    ) -> Result<Vec<MapEntry>, ChdError> {
        let mut body = body();
        body.put(kind.into(), 4);
        for &(v, n) in fields {
            body.put(v, n);
        }
        let (h, file) = image(1, &body, &[entry], (16, 4));
        map_of(&h, &file)
    }

    fn reason_of(r: Result<Vec<MapEntry>, ChdError>) -> Option<Unidentifiable> {
        r.err().and_then(|e| e.reason())
    }

    #[test]
    fn bad_entries_have_their_reasons() {
        let copy_self = one_type(SELF, &[(0, 4)], expanded(SELF, 0, 0, 0));
        assert_eq!(reason_of(copy_self), Some(Unidentifiable::Corrupt));
        let parent = one_type(PARENT, &[], expanded(PARENT, 0, 0, 0));
        assert_eq!(reason_of(parent), Some(Unidentifiable::Parent));
        let slot1 = one_type(1, &[(10, 16), (0, 16)], expanded(1, 10, 200, 0));
        assert_eq!(reason_of(slot1), Some(Unidentifiable::Codec));
        let past = one_type(0, &[(60_000, 16), (0, 16)], expanded(0, 60_000, 200, 0));
        assert_eq!(reason_of(past), Some(Unidentifiable::Corrupt));
        let unknown = one_type(14, &[], expanded(14, 0, 200, 0));
        assert_eq!(reason_of(unknown), Some(Unidentifiable::Corrupt));
        let crc = one_type(0, &[(10, 16), (0, 16)], expanded(0, 10, 200, 1));
        assert_eq!(reason_of(crc), Some(Unidentifiable::Corrupt));
    }

    #[test]
    fn a_huff_slot_is_an_unsupported_codec() {
        let mut body = body();
        body.put(0, 4);
        body.put(10, 16);
        body.put(0, 16);
        let (mut h, file) = image(1, &body, &[expanded(0, 10, 200, 0)], (16, 0));
        h.compressors[0] = Some(crate::chd::FourCc(*b"huff"));
        assert_eq!(reason_of(map_of(&h, &file)), Some(Unidentifiable::Codec));
    }

    #[test]
    fn a_plain_map_uses_offset_units_of_one_hunk() {
        let mut hb = sample(3, 1);
        hb[16..24].fill(0);
        hb[40..48].copy_from_slice(&124u64.to_be_bytes());
        let h = read_header(&hb[..]).expect("header");
        let mut file = hb.to_vec();
        for e in [1u32, 0, 2] {
            file.extend_from_slice(&e.to_be_bytes());
        }
        file.resize(3 * 2448, 0);
        let map = map_of(&h, &file).expect("map");
        assert_eq!(
            map,
            [
                MapEntry::Raw {
                    offset: 2448,
                    crc: None
                },
                MapEntry::Zero,
                MapEntry::Raw {
                    offset: 4896,
                    crc: None
                },
            ]
        );
        file.truncate(3 * 2448 - 1);
        assert_eq!(reason_of(map_of(&h, &file)), Some(Unidentifiable::Corrupt));
    }

    #[test]
    fn map_entries_fit_in_16_bytes() {
        assert!(std::mem::size_of::<MapEntry>() <= 16);
    }
}
