//! The v5 compressed map encoder: Huffman-coded hunk types with RLE, then per-hunk fields.

use mistarr_core::chd::crc16;

use super::flac::BitWriter;
use super::huffman::{self, SYMBOLS};
use super::Tree;

/// How one hunk is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Entry {
    /// `len` bytes compressed with codec slot `slot`; `crc` of the decoded hunk.
    Codec { slot: u8, len: u32, crc: u16 },
    /// Stored as is.
    None { crc: u16 },
    /// A copy of hunk `target`.
    Copy { target: u32 },
}

const NONE: u8 = 4;
const SELF: u8 = 5;
const RLE_SMALL: u8 = 7;
const RLE_LARGE: u8 = 8;
const SELF_0: u8 = 9;
const SELF_1: u8 = 10;

fn bits_needed(v: u64) -> u32 {
    64 - v.leading_zeros()
}

/// The map for `entries`: the 16-byte header and the bit stream, hunk data starting at `first`.
pub(crate) fn encode(
    entries: &[Entry],
    first: u64,
    hunk_bytes: u32,
    tree: Tree,
    rle: bool,
) -> Vec<u8> {
    let mut expanded = Vec::with_capacity(entries.len() * 12);
    let mut cur = first;
    for e in entries {
        let (kind, len, offset, crc) = match *e {
            Entry::Codec { slot, len, crc } => (slot, len, cur, crc),
            Entry::None { crc } => (NONE, hunk_bytes, cur, crc),
            Entry::Copy { target } => (SELF, 0, u64::from(target), 0),
        };
        if kind != SELF {
            cur += u64::from(len);
        }
        expanded.push(kind);
        expanded.extend_from_slice(&len.to_be_bytes()[1..]);
        expanded.extend_from_slice(&offset.to_be_bytes()[2..]);
        expanded.extend_from_slice(&crc.to_be_bytes());
    }

    let mut types = Vec::with_capacity(entries.len());
    let (mut last_self, mut max_self, mut max_len) = (0u64, 0u64, 0u64);
    for e in entries {
        types.push(match *e {
            Entry::Codec { slot, len, .. } => {
                max_len = max_len.max(u64::from(len));
                slot
            }
            Entry::None { .. } => NONE,
            Entry::Copy { target } => {
                let t = u64::from(target);
                let code = if t == last_self {
                    SELF_0
                } else if t == last_self + 1 {
                    SELF_1
                } else {
                    max_self = max_self.max(t);
                    SELF
                };
                last_self = t;
                code
            }
        });
    }

    let symbols = type_symbols(&types, rle);
    let mut histo = [0u32; SYMBOLS];
    for &s in &symbols {
        histo[usize::from(s)] += 1;
    }
    let lengths = match tree {
        Tree::Uniform => [4u8; SYMBOLS],
        Tree::Built => huffman::lengths(&histo),
    };
    let codes = huffman::codes(&lengths);
    let mut w = BitWriter::default();
    huffman::export_rle(&lengths, &mut w);
    for &s in &symbols {
        w.put(
            u64::from(codes[usize::from(s)]),
            u32::from(lengths[usize::from(s)]),
        );
    }
    let (length_bits, self_bits) = (bits_needed(max_len), bits_needed(max_self));
    for (e, &t) in entries.iter().zip(&types) {
        match *e {
            Entry::Codec { len, crc, .. } => {
                w.put(u64::from(len), length_bits);
                w.put(u64::from(crc), 16);
            }
            Entry::None { crc } => w.put(u64::from(crc), 16),
            Entry::Copy { target } if t == SELF => w.put(u64::from(target), self_bits),
            Entry::Copy { .. } => {}
        }
    }

    let mut out = Vec::with_capacity(16 + w.bytes.len());
    out.extend_from_slice(&u32::try_from(w.bytes.len()).unwrap_or(0).to_be_bytes());
    out.extend_from_slice(&first.to_be_bytes()[2..]);
    out.extend_from_slice(&crc16(&expanded).to_be_bytes());
    out.push(u8::try_from(length_bits).unwrap_or(0));
    out.push(u8::try_from(self_bits).unwrap_or(0));
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(&w.bytes);
    out
}

/// The Huffman symbols for a type sequence: literals, and with `rle` the MAME run codes.
fn type_symbols(types: &[u8], rle: bool) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < types.len() {
        let t = types[i];
        out.push(t);
        let mut run = 0;
        while i + 1 + run < types.len() && types[i + 1 + run] == t {
            run += 1;
        }
        i += 1;
        if !rle {
            continue;
        }
        let mut left = run;
        while left > 0 {
            if left < 3 {
                out.push(t);
                left -= 1;
            } else if left <= 18 {
                out.push(RLE_SMALL);
                out.push(u8::try_from(left - 3).unwrap_or(0));
                left = 0;
            } else {
                let n = left.min(274);
                out.push(RLE_LARGE);
                out.push(u8::try_from((n - 19) >> 4).unwrap_or(0));
                out.push(u8::try_from((n - 19) & 15).unwrap_or(0));
                left -= n;
            }
        }
        i += run;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_become_rle_codes() {
        let t = [0u8; 1 + 3];
        assert_eq!(type_symbols(&t, true), [0, RLE_SMALL, 0]);
        assert_eq!(type_symbols(&t, false), [0, 0, 0, 0]);
        assert_eq!(type_symbols(&[1, 1, 1], true), [1, 1, 1]);
        let long = [2u8; 1 + 19];
        assert_eq!(type_symbols(&long, true), [2, RLE_LARGE, 0, 0]);
        let longer = [2u8; 1 + 274 + 2];
        assert_eq!(type_symbols(&longer, true), [2, RLE_LARGE, 15, 15, 2, 2]);
    }

    #[test]
    fn bits_needed_counts_significant_bits() {
        assert_eq!(
            (
                bits_needed(0),
                bits_needed(1),
                bits_needed(255),
                bits_needed(256)
            ),
            (0, 1, 8, 9)
        );
    }
}
