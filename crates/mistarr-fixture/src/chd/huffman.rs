//! Length-limited Huffman trees with MAME's canonical codes and RLE tree export, written from
//! `docs/CHD.md` "Map tree" independently of the decoder in mistarr-core.

use super::flac::BitWriter;

/// Symbols in the map tree.
pub(crate) const SYMBOLS: usize = 16;
/// Longest code allowed.
pub(crate) const MAX_BITS: u8 = 8;

/// Code lengths for `histo`, at most [`MAX_BITS`]; unused symbols get length 0 and a lone
/// symbol gets length 1.
pub(crate) fn lengths(histo: &[u32; SYMBOLS]) -> [u8; SYMBOLS] {
    let mut weights: Vec<u64> = histo.iter().map(|&w| u64::from(w)).collect();
    loop {
        let mut out = [0u8; SYMBOLS];
        let mut nodes: Vec<(u64, Vec<usize>)> = weights
            .iter()
            .enumerate()
            .filter(|(_, &w)| w > 0)
            .map(|(s, &w)| (w, vec![s]))
            .collect();
        if nodes.len() == 1 {
            out[nodes[0].1[0]] = 1;
            return out;
        }
        while nodes.len() > 1 {
            nodes.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
            let (wa, a) = nodes.pop().unwrap_or_default();
            let (wb, b) = nodes.pop().unwrap_or_default();
            for &s in a.iter().chain(&b) {
                out[s] += 1;
            }
            nodes.push((wa + wb, a.into_iter().chain(b).collect()));
        }
        if out.iter().all(|&l| l <= MAX_BITS) {
            return out;
        }
        for w in weights.iter_mut().filter(|w| **w > 0) {
            *w = w.div_ceil(2);
        }
    }
}

/// MAME's canonical codes: walk lengths from longest to shortest handing out ranges, so a
/// longer code has a numerically smaller value; then number symbols in order within a length.
pub(crate) fn codes(lengths: &[u8; SYMBOLS]) -> [u32; SYMBOLS] {
    let mut count = [0u32; 33];
    for &l in lengths {
        count[usize::from(l)] += 1;
    }
    let mut first = [0u32; 33];
    let mut start = 0u32;
    for len in (1..=32usize).rev() {
        first[len] = start;
        start = u32::midpoint(start, count[len]);
    }
    let mut out = [0u32; SYMBOLS];
    for (code, &l) in out.iter_mut().zip(lengths) {
        if l > 0 {
            *code = first[usize::from(l)];
            first[usize::from(l)] += 1;
        }
    }
    out
}

/// Writes `lengths` as MAME's `import_tree_rle` reads them: 4-bit fields, `1` escaping a
/// literal 1 or a run of 3 to 18 equal lengths.
pub(crate) fn export_rle(lengths: &[u8; SYMBOLS], w: &mut BitWriter) {
    let mut i = 0;
    while i < SYMBOLS {
        let v = lengths[i];
        let mut run = 1;
        while i + run < SYMBOLS && lengths[i + run] == v {
            run += 1;
        }
        let mut left = run;
        while left > 0 {
            if v == 1 {
                w.put(1, 4);
                w.put(1, 4);
                left -= 1;
            } else if left <= 2 {
                w.put(u64::from(v), 4);
                left -= 1;
            } else {
                let reps = (left - 3).min(15);
                w.put(1, 4);
                w.put(u64::from(v), 4);
                w.put(reps as u64, 4);
                left -= reps + 3;
            }
        }
        i += run;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pad(prefix: &[u8]) -> [u8; SYMBOLS] {
        let mut l = [0u8; SYMBOLS];
        l[..prefix.len()].copy_from_slice(prefix);
        l
    }

    #[test]
    fn codes_follow_the_docs_example() {
        let c = codes(&pad(&[1, 2, 3, 3]));
        assert_eq!(&c[..4], &[1, 1, 0, 1]);
        let uniform = codes(&[4; SYMBOLS]);
        assert!(uniform.iter().enumerate().all(|(s, &c)| c as usize == s));
    }

    #[test]
    fn lengths_are_limited_and_complete() {
        let mut skewed = [0u32; SYMBOLS];
        for (i, w) in skewed.iter_mut().enumerate() {
            *w = 1 << i;
        }
        let l = lengths(&skewed);
        assert!(l.iter().all(|&x| (1..=MAX_BITS).contains(&x)));
        let kraft: f64 = l.iter().map(|&x| 0.5f64.powi(i32::from(x))).sum();
        assert!((kraft - 1.0).abs() < 1e-9);
        let mut one = [0u32; SYMBOLS];
        one[3] = 9;
        assert_eq!(lengths(&one)[3], 1);
    }

    #[test]
    fn rle_export_of_a_uniform_tree_is_1_4_13() {
        let mut w = BitWriter::default();
        export_rle(&[4; SYMBOLS], &mut w);
        assert_eq!(w.bytes, [0x14, 0xd0]);
    }
}
