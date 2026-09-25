//! MAME's canonical Huffman decoder for the 16-symbol map tree; see `docs/CHD.md` "Map tree".

use super::bits::BitReader;
use super::{corrupt, ChdError};

/// Symbols in the map tree.
pub(crate) const SYMBOLS: usize = 16;
/// Longest code in the map tree.
pub(crate) const MAX_BITS: u32 = 8;

/// A decoder built from code lengths with MAME's canonical code assignment.
pub(crate) struct Huffman {
    /// Indexed by the next [`MAX_BITS`] bits: `(symbol, length)`, length 0 for no code.
    lookup: [(u8, u8); 1 << MAX_BITS],
}

/// MAME's canonical codes for `lengths`: longer codes take numerically smaller values.
///
/// Zero-length symbols get no code. Fails when the lengths do not form a complete prefix
/// code in MAME's sense, or a length exceeds [`MAX_BITS`].
pub(crate) fn canonical_codes(lengths: &[u8; SYMBOLS]) -> Result<[u32; SYMBOLS], ChdError> {
    let mut histo = [0u32; 33];
    for &len in lengths {
        if u32::from(len) > MAX_BITS {
            return Err(corrupt("a map code is longer than 8 bits"));
        }
        histo[usize::from(len)] += 1;
    }
    let mut start = 0u32;
    for len in (1..=32).rev() {
        let next = u32::midpoint(start, histo[len]);
        if len != 1 && next * 2 != start + histo[len] {
            return Err(corrupt("the map code lengths are inconsistent"));
        }
        histo[len] = start;
        start = next;
    }
    let mut codes = [0u32; SYMBOLS];
    for (code, &len) in codes.iter_mut().zip(lengths) {
        if len > 0 {
            *code = histo[usize::from(len)];
            histo[usize::from(len)] += 1;
        }
    }
    Ok(codes)
}

impl Huffman {
    /// Builds the decoder for `lengths`.
    pub(crate) fn from_lengths(lengths: &[u8; SYMBOLS]) -> Result<Self, ChdError> {
        let codes = canonical_codes(lengths)?;
        let mut lookup = [(0u8, 0u8); 1 << MAX_BITS];
        for (sym, (&code, &len)) in codes.iter().zip(lengths).enumerate() {
            if len == 0 {
                continue;
            }
            let shift = MAX_BITS - u32::from(len);
            let first = (code as usize) << shift;
            let end = ((code as usize) + 1) << shift;
            let slots = lookup
                .get_mut(first..end)
                .ok_or_else(|| corrupt("a map code falls outside the tree"))?;
            for slot in slots {
                *slot = (u8::try_from(sym).unwrap_or(0), len);
            }
        }
        Ok(Self { lookup })
    }

    /// Reads the tree as MAME's `import_tree_rle` writes it with 4-bit fields.
    pub(crate) fn import_rle(bits: &mut BitReader<'_>) -> Result<Self, ChdError> {
        let mut lengths = [0u8; SYMBOLS];
        let mut at = 0usize;
        while at < SYMBOLS {
            let v = bits.read(4);
            if v != 1 {
                lengths[at] = u8::try_from(v).unwrap_or(0);
                at += 1;
                continue;
            }
            let w = bits.read(4);
            if w == 1 {
                lengths[at] = 1;
                at += 1;
                continue;
            }
            let count = bits.read(4) as usize + 3;
            let run = lengths
                .get_mut(at..at + count)
                .ok_or_else(|| corrupt("a map tree run is too long"))?;
            run.fill(u8::try_from(w).unwrap_or(0));
            at += count;
        }
        if bits.overflowed() {
            return Err(corrupt("the map tree ends early"));
        }
        Self::from_lengths(&lengths)
    }

    /// Decodes one symbol; a bit pattern with no code is corrupt.
    pub(crate) fn decode(&self, bits: &mut BitReader<'_>) -> Result<u8, ChdError> {
        let (sym, len) = self.lookup[bits.peek(MAX_BITS) as usize];
        if len == 0 {
            return Err(corrupt("a map code is not in the tree"));
        }
        bits.skip(u32::from(len));
        Ok(sym)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn lengths(prefix: &[u8]) -> [u8; SYMBOLS] {
        let mut l = [0u8; SYMBOLS];
        l[..prefix.len()].copy_from_slice(prefix);
        l
    }

    /// The docs/CHD.md example: lengths 1, 2, 3, 3 give codes 1, 01, 000, 001.
    #[test]
    fn canonical_codes_give_longer_codes_smaller_values() {
        let codes = canonical_codes(&lengths(&[1, 2, 3, 3])).expect("valid");
        assert_eq!(&codes[..4], &[0b1, 0b01, 0b000, 0b001]);
        assert_ne!(
            &codes[..4],
            &[0b0, 0b10, 0b110, 0b111],
            "not DEFLATE's order"
        );
    }

    #[test]
    fn decodes_the_example_codes() {
        let h = Huffman::from_lengths(&lengths(&[1, 2, 3, 3])).expect("valid");
        // 1 | 01 | 000 | 001 | 1  => 1010 0000 11..
        let mut bits = BitReader::new(&[0b1010_0000, 0b1100_0000]);
        let got: Vec<u8> = (0..5).map(|_| h.decode(&mut bits).expect("code")).collect();
        assert_eq!(got, [0, 1, 2, 3, 0]);
    }

    #[test]
    fn inconsistent_lengths_fail() {
        assert!(canonical_codes(&lengths(&[2, 2, 2])).is_err());
        assert!(canonical_codes(&lengths(&[9])).is_err());
        assert!(Huffman::from_lengths(&lengths(&[1, 1, 1])).is_err());
    }

    #[test]
    fn a_pattern_outside_an_incomplete_tree_is_corrupt() {
        let h = Huffman::from_lengths(&lengths(&[1])).expect("single code");
        assert_eq!(h.decode(&mut BitReader::new(&[0x00])).expect("code 0"), 0);
        assert!(h.decode(&mut BitReader::new(&[0x80])).is_err());
    }

    #[test]
    fn imports_a_uniform_rle_tree() {
        // 1, 4, 13: escape, length 4, repeat 13 + 3 = 16 times.
        let mut bits = BitReader::new(&[0x14, 0xd0]);
        let h = Huffman::import_rle(&mut bits).expect("tree");
        for sym in 0u8..16 {
            let byte = [sym << 4];
            assert_eq!(h.decode(&mut BitReader::new(&byte)).expect("code"), sym);
        }
    }

    #[test]
    fn imports_a_mixed_run_tree() {
        // lengths: 1 (via 1,1), 2, 3, 3, then 12 zeros as 1,0,9.
        let nibbles = [1u8, 1, 2, 3, 3, 1, 0, 9];
        let bytes: Vec<u8> = nibbles.chunks(2).map(|p| (p[0] << 4) | p[1]).collect();
        let h = Huffman::import_rle(&mut BitReader::new(&bytes)).expect("tree");
        let mut bits = BitReader::new(&[0b1010_0000, 0b1000_0000]);
        let got: Vec<u8> = (0..4).map(|_| h.decode(&mut bits).expect("code")).collect();
        assert_eq!(got, [0, 1, 2, 3]);
    }

    #[test]
    fn an_overlong_run_or_a_short_tree_fails() {
        // 1, 4, 15 repeats 18 > 16.
        assert!(Huffman::import_rle(&mut BitReader::new(&[0x14, 0xf0])).is_err());
        assert!(Huffman::import_rle(&mut BitReader::new(&[0x22])).is_err());
    }

    proptest! {
        #[test]
        fn import_never_panics(data in proptest::collection::vec(any::<u8>(), 0..16)) {
            let mut bits = BitReader::new(&data);
            if let Ok(h) = Huffman::import_rle(&mut bits) {
                for _ in 0..8 {
                    let _ = h.decode(&mut bits);
                }
            }
        }
    }
}
