//! Checks a zstd frame's decoded length before ruzstd runs, so hostile input cannot make it
//! allocate; see `docs/CHD.md` "Codecs".

use super::{corrupt, span, ChdError};

/// Largest block a zstd frame may hold, compressed or decoded.
pub(crate) const MAX_BLOCK: usize = 128 << 10;
/// Window accepted whatever the hunk length: the one ruzstd's encoder writes.
pub(crate) const MIN_WINDOW_CAP: usize = 128 << 10;

/// Literals length codes: baseline and extra bits.
const LL_CODES: [(u32, u8); 36] = [
    (0, 0),
    (1, 0),
    (2, 0),
    (3, 0),
    (4, 0),
    (5, 0),
    (6, 0),
    (7, 0),
    (8, 0),
    (9, 0),
    (10, 0),
    (11, 0),
    (12, 0),
    (13, 0),
    (14, 0),
    (15, 0),
    (16, 1),
    (18, 1),
    (20, 1),
    (22, 1),
    (24, 2),
    (28, 2),
    (32, 3),
    (40, 3),
    (48, 4),
    (64, 6),
    (128, 7),
    (256, 8),
    (512, 9),
    (1024, 10),
    (2048, 11),
    (4096, 12),
    (8192, 13),
    (16384, 14),
    (32768, 15),
    (65536, 16),
];

/// Match length codes 32 to 52 (codes 0 to 31 are `code + 3` with no extra bits).
const ML_HIGH: [(u32, u8); 21] = [
    (35, 1),
    (37, 1),
    (39, 1),
    (41, 1),
    (43, 2),
    (47, 2),
    (51, 3),
    (59, 3),
    (67, 4),
    (83, 4),
    (99, 5),
    (131, 7),
    (259, 8),
    (515, 9),
    (1027, 10),
    (2051, 11),
    (4099, 12),
    (8195, 13),
    (16387, 14),
    (32771, 15),
    (65539, 16),
];

const LL_DEFAULT: [i32; 36] = [
    4, 3, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 3, 2, 1, 1, 1, 1, 1,
    -1, -1, -1, -1,
];
const ML_DEFAULT: [i32; 53] = [
    1, 4, 3, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1, -1, -1,
];
const OF_DEFAULT: [i32; 29] = [
    1, 1, 1, 1, 1, 1, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1,
];

/// One of the three sequence symbol streams.
#[derive(Clone, Copy)]
struct Kind {
    max_log: u32,
    max_symbol: usize,
    default_log: u32,
    default: &'static [i32],
}

const LL: Kind = Kind {
    max_log: 9,
    max_symbol: 35,
    default_log: 6,
    default: &LL_DEFAULT,
};
const OF: Kind = Kind {
    max_log: 8,
    max_symbol: 31,
    default_log: 5,
    default: &OF_DEFAULT,
};
const ML: Kind = Kind {
    max_log: 9,
    max_symbol: 52,
    default_log: 6,
    default: &ML_DEFAULT,
};

/// A decoding table: one symbol, or FSE states of `(symbol, bits, baseline)`.
#[derive(Clone)]
enum Table {
    Rle(u8),
    Fse {
        log: u32,
        states: Vec<(u8, u32, u32)>,
    },
}

impl Table {
    fn init(&self, bits: &mut Reverse<'_>) -> Result<u32, ChdError> {
        match self {
            Self::Rle(_) => Ok(0),
            Self::Fse { log, .. } => bits.read(*log),
        }
    }

    fn symbol(&self, state: u32) -> u8 {
        match self {
            Self::Rle(s) => *s,
            Self::Fse { states, .. } => states.get(state as usize).map_or(0, |e| e.0),
        }
    }

    fn next(&self, state: u32, bits: &mut Reverse<'_>) -> Result<u32, ChdError> {
        match self {
            Self::Rle(_) => Ok(0),
            Self::Fse { states, .. } => {
                let &(_, n, base) = states
                    .get(state as usize)
                    .ok_or_else(|| corrupt("zstd FSE state"))?;
                Ok(base + bits.read(n)?)
            }
        }
    }
}

/// The tables in force, kept from block to block of one frame for repeat mode.
#[derive(Default)]
struct Tables {
    ll: Option<Table>,
    of: Option<Table>,
    ml: Option<Table>,
}

/// Checks the zstd frame at the start of `frame` against a hunk of `expect` bytes: its header,
/// a window of at most `expect` rounded up to a power of two or 128 KiB, its content size, and every block, whose decoded
/// lengths must add up to exactly `expect`. Returns the window size.
pub(crate) fn check_frame(frame: &[u8], expect: usize) -> Result<usize, ChdError> {
    let (window, mut at, checksum) = frame_header(frame, expect)?;
    let mut tables = Tables::default();
    let mut total = 0usize;
    loop {
        let h = span(frame, at, 3, "a zstd block header")?;
        let h = u32::from(h[0]) | u32::from(h[1]) << 8 | u32::from(h[2]) << 16;
        at += 3;
        let size = (h >> 3) as usize;
        if size > MAX_BLOCK {
            return Err(corrupt("a zstd block is over 128 KiB"));
        }
        let out = match (h >> 1) & 3 {
            0 => {
                span(frame, at, size, "a zstd raw block")?;
                at += size;
                size
            }
            1 => {
                span(frame, at, 1, "a zstd RLE block")?;
                at += 1;
                size
            }
            2 => {
                let body = span(frame, at, size, "a zstd compressed block")?;
                at += size;
                compressed_block(body, &mut tables)?
            }
            _ => return Err(corrupt("a zstd block has the reserved type")),
        };
        if out > MAX_BLOCK {
            return Err(corrupt("a zstd block decodes to over 128 KiB"));
        }
        total += out;
        if total > expect {
            return Err(corrupt("zstd data are longer than the hunk"));
        }
        if h & 1 != 0 {
            break;
        }
    }
    if checksum {
        span(frame, at, 4, "the zstd checksum")?;
    }
    if total != expect {
        return Err(corrupt("zstd data have the wrong length"));
    }
    Ok(window)
}

/// The window, the offset of the first block and whether a checksum follows the blocks.
fn frame_header(frame: &[u8], expect: usize) -> Result<(usize, usize, bool), ChdError> {
    let head = span(frame, 0, 5, "the zstd frame header")?;
    if head[..4] != [0x28, 0xb5, 0x2f, 0xfd] {
        return Err(corrupt("no zstd frame magic"));
    }
    let desc = head[4];
    let single = desc & 0x20 != 0;
    if desc & 0x08 != 0 || desc & 0x03 != 0 {
        return Err(corrupt("zstd frame uses a reserved bit or a dictionary"));
    }
    let mut at = 5;
    let mut window = 0u64;
    if !single {
        let wd = span(frame, at, 1, "the zstd window")?[0];
        let base = 1u64 << (10 + u32::from(wd >> 3));
        window = base + (base / 8) * u64::from(wd & 7);
        at += 1;
    }
    let fcs_len = match desc >> 6 {
        0 if single => 1,
        0 => 0,
        1 => 2,
        2 => 4,
        _ => 8,
    };
    let fcs = span(frame, at, fcs_len, "the zstd content size")?;
    at += fcs_len;
    let mut size = fcs.iter().rev().fold(0u64, |v, &b| (v << 8) | u64::from(b));
    if fcs_len == 2 {
        size += 256;
    }
    if fcs_len > 0 && size != expect as u64 {
        return Err(corrupt("zstd content size is not the hunk length"));
    }
    if single {
        window = size;
    }
    if window > expect.next_power_of_two().max(MIN_WINDOW_CAP) as u64 {
        return Err(corrupt("zstd window is larger than the hunk"));
    }
    let window = usize::try_from(window).map_err(|_| corrupt("zstd window"))?;
    Ok((window, at, desc & 0x04 != 0))
}

/// The decoded length of a compressed block: its literals plus every match.
fn compressed_block(body: &[u8], tables: &mut Tables) -> Result<usize, ChdError> {
    let (literals, used) = literals_section(body)?;
    if literals > MAX_BLOCK {
        return Err(corrupt("zstd literals are over 128 KiB"));
    }
    let seq = body
        .get(used..)
        .ok_or_else(|| corrupt("zstd literals run past the block"))?;
    let b0 = usize::from(*seq.first().ok_or_else(|| corrupt("no zstd sequences"))?);
    let byte = |i: usize| span(seq, i, 1, "the zstd sequence count").map(|b| usize::from(b[0]));
    let (count, mut at) = match b0 {
        0 => return Ok(literals),
        1..=127 => (b0, 1),
        128..=254 => (((b0 - 128) << 8) + byte(1)?, 2),
        _ => (byte(1)? + (byte(2)? << 8) + 0x7f00, 3),
    };
    // Every match is at least 3 bytes, so more sequences than this overflow the block.
    if count > MAX_BLOCK / 3 {
        return Err(corrupt("too many zstd sequences"));
    }
    let modes = byte(at)?;
    at += 1;
    if modes & 3 != 0 {
        return Err(corrupt("zstd sequence modes use reserved bits"));
    }
    let ll = table(LL, modes >> 6, seq, &mut at, &mut tables.ll)?;
    let of = table(OF, (modes >> 4) & 3, seq, &mut at, &mut tables.of)?;
    let ml = table(ML, (modes >> 2) & 3, seq, &mut at, &mut tables.ml)?;
    let mut bits = Reverse::new(seq.get(at..).unwrap_or_default())?;
    let (mut sl, mut so, mut sm) = (
        ll.init(&mut bits)?,
        of.init(&mut bits)?,
        ml.init(&mut bits)?,
    );
    let (mut lits, mut matches) = (0usize, 0usize);
    for i in 0..count {
        let (lc, oc, mc) = (ll.symbol(sl), of.symbol(so), ml.symbol(sm));
        if oc > 31 {
            return Err(corrupt("zstd offset code is over 31"));
        }
        bits.read(u32::from(oc))?;
        let (base, extra) = match mc {
            0..=31 => (u32::from(mc) + 3, 0),
            _ => *ML_HIGH
                .get(usize::from(mc) - 32)
                .ok_or_else(|| corrupt("zstd match length code"))?,
        };
        matches += (base + bits.read(u32::from(extra))?) as usize;
        let &(base, extra) = LL_CODES
            .get(usize::from(lc))
            .ok_or_else(|| corrupt("zstd literals length code"))?;
        lits += (base + bits.read(u32::from(extra))?) as usize;
        if lits > literals || literals + matches > MAX_BLOCK {
            return Err(corrupt("a zstd block decodes to over 128 KiB"));
        }
        if i + 1 < count {
            sl = ll.next(sl, &mut bits)?;
            sm = ml.next(sm, &mut bits)?;
            so = of.next(so, &mut bits)?;
        }
    }
    if !bits.is_empty() {
        return Err(corrupt("zstd sequences leave bits unread"));
    }
    Ok(literals + matches)
}

/// The regenerated size of a literals section and the bytes it takes.
fn literals_section(body: &[u8]) -> Result<(usize, usize), ChdError> {
    let b = |i: usize| span(body, i, 1, "the zstd literals header").map(|b| usize::from(b[0]));
    let b0 = b(0)?;
    let (regen, used) = if b0 & 3 < 2 {
        let (regen, head) = match (b0 >> 2) & 3 {
            0 | 2 => (b0 >> 3, 1),
            1 => ((b0 >> 4) + (b(1)? << 4), 2),
            _ => ((b0 >> 4) + (b(1)? << 4) + (b(2)? << 12), 3),
        };
        let raw = matches!(b0 & 3, 0);
        (regen, head + if raw { regen } else { 1 })
    } else {
        let (head, width) = match (b0 >> 2) & 3 {
            0 | 1 => (3, 10),
            2 => (4, 14),
            _ => (5, 18),
        };
        let v = span(body, 0, head, "the zstd literals header")?
            .iter()
            .rev()
            .fold(0u64, |v, &x| (v << 8) | u64::from(x));
        let mask = (1u64 << width) - 1;
        let regen = usize::try_from((v >> 4) & mask).unwrap_or(usize::MAX);
        let comp = usize::try_from((v >> (4 + width)) & mask).unwrap_or(usize::MAX);
        (regen, head + comp)
    };
    span(body, 0, used, "the zstd literals")?;
    Ok((regen, used))
}

/// Reads the table `mode` selects for `kind` at `seq[*at..]`, and keeps it in `slot`.
fn table<'a>(
    kind: Kind,
    mode: usize,
    seq: &[u8],
    at: &mut usize,
    slot: &'a mut Option<Table>,
) -> Result<&'a Table, ChdError> {
    let t = match mode {
        0 => build(kind.default_log, kind.default)?,
        1 => {
            let s = span(seq, *at, 1, "a zstd RLE symbol")?[0];
            *at += 1;
            if usize::from(s) > kind.max_symbol {
                return Err(corrupt("zstd RLE symbol is out of range"));
            }
            Table::Rle(s)
        }
        2 => {
            let rest = seq.get(*at..).unwrap_or_default();
            let (log, probs, used) = describe(rest, kind)?;
            *at += used;
            build(log, &probs)?
        }
        _ => slot
            .take()
            .ok_or_else(|| corrupt("zstd repeats a table it never had"))?,
    };
    Ok(slot.insert(t))
}

/// Reads an FSE table description: its accuracy log, probabilities and length in bytes.
fn describe(src: &[u8], kind: Kind) -> Result<(u32, Vec<i32>, usize), ChdError> {
    let mut bits = Forward { src, pos: 0 };
    let log = bits.take(4) + 5;
    if log > kind.max_log {
        return Err(corrupt("zstd FSE accuracy is too high"));
    }
    let mut remaining = (1i32 << log) + 1;
    let mut threshold = 1i32 << log;
    let mut width = log + 1;
    let mut probs = Vec::with_capacity(kind.max_symbol + 1);
    while remaining > 1 {
        if probs.len() > kind.max_symbol {
            return Err(corrupt("zstd FSE table has too many symbols"));
        }
        let max = 2 * threshold - 1 - remaining;
        #[allow(clippy::cast_possible_wrap)] // at most 10 bits
        let v = bits.peek(width) as i32;
        let mut count = if v & (threshold - 1) < max {
            bits.pos += width as usize - 1;
            v & (threshold - 1)
        } else {
            bits.pos += width as usize;
            let c = v & (2 * threshold - 1);
            if c >= threshold {
                c - max
            } else {
                c
            }
        };
        count -= 1;
        if count.abs() >= remaining {
            return Err(corrupt("zstd FSE probabilities overflow"));
        }
        remaining -= count.abs();
        probs.push(count);
        if count == 0 {
            loop {
                let run = bits.take(2);
                // Checked per run, so a hostile run of zeros never grows the vector.
                if probs.len() + run as usize > kind.max_symbol + 1 {
                    return Err(corrupt("zstd FSE table has too many symbols"));
                }
                probs.extend(std::iter::repeat_n(0, run as usize));
                if run != 3 {
                    break;
                }
            }
        }
        while remaining < threshold {
            width -= 1;
            threshold >>= 1;
        }
    }
    let used = bits.pos.div_ceil(8);
    if remaining != 1 || used > src.len() {
        return Err(corrupt("zstd FSE table description is invalid"));
    }
    Ok((log, probs, used))
}

/// Builds the decoding states of a distribution whose absolute values sum to `1 << log`.
fn build(log: u32, probs: &[i32]) -> Result<Table, ChdError> {
    let size = 1usize << log;
    let mut symbols = vec![0u8; size];
    let mut next = vec![0u32; probs.len()];
    let mut high = size;
    for (s, &p) in probs.iter().enumerate() {
        let s8 = u8::try_from(s).map_err(|_| corrupt("zstd FSE symbol"))?;
        if p == -1 {
            high = high
                .checked_sub(1)
                .ok_or_else(|| corrupt("zstd FSE table overflows"))?;
            symbols[high] = s8;
            next[s] = 1;
        } else {
            next[s] = u32::try_from(p).map_err(|_| corrupt("zstd FSE probability"))?;
        }
    }
    let step = (size >> 1) + (size >> 3) + 3;
    let mut pos = 0usize;
    for (s, &p) in probs.iter().enumerate() {
        for _ in 0..p.max(0) {
            if high == 0 {
                return Err(corrupt("zstd FSE table overflows"));
            }
            symbols[pos] = u8::try_from(s).map_err(|_| corrupt("zstd FSE symbol"))?;
            pos = (pos + step) & (size - 1);
            while pos >= high {
                pos = (pos + step) & (size - 1);
            }
        }
    }
    if pos != 0 {
        return Err(corrupt("zstd FSE probabilities do not fill the table"));
    }
    let mut states = Vec::with_capacity(size);
    for &s in &symbols {
        let x = next
            .get_mut(usize::from(s))
            .ok_or_else(|| corrupt("zstd FSE symbol"))?;
        if *x == 0 {
            return Err(corrupt("zstd FSE table is inconsistent"));
        }
        let n = log - (31 - x.leading_zeros());
        let base = (*x << n) - u32::try_from(size).map_err(|_| corrupt("zstd FSE size"))?;
        *x += 1;
        states.push((s, n, base));
    }
    Ok(Table::Fse { log, states })
}

/// A little-endian bit stream read from its first bit on; bits past the end read as zero.
struct Forward<'a> {
    src: &'a [u8],
    pos: usize,
}

impl Forward<'_> {
    fn peek(&self, n: u32) -> u32 {
        (0..n).fold(0, |v, i| {
            let at = self.pos + i as usize;
            let bit = self.src.get(at / 8).map_or(0, |b| (b >> (at % 8)) & 1);
            v | u32::from(bit) << i
        })
    }

    fn take(&mut self, n: u32) -> u32 {
        let v = self.peek(n);
        self.pos += n as usize;
        v
    }
}

/// A zstd backward bit stream: read from its last bit, after the padding marker, towards the start.
struct Reverse<'a> {
    src: &'a [u8],
    /// Bits not read yet, from bit 0 of the first byte.
    left: usize,
}

impl<'a> Reverse<'a> {
    fn new(src: &'a [u8]) -> Result<Self, ChdError> {
        let last = *src
            .last()
            .ok_or_else(|| corrupt("zstd sequences have no bit stream"))?;
        if last == 0 {
            return Err(corrupt("zstd bit stream has no end marker"));
        }
        let left = src.len() * 8 - (last.leading_zeros() as usize + 1);
        Ok(Self { src, left })
    }

    /// The next `n` bits (at most 32), most significant first.
    fn read(&mut self, n: u32) -> Result<u32, ChdError> {
        let n = n as usize;
        if n == 0 {
            return Ok(0);
        }
        self.left = self
            .left
            .checked_sub(n)
            .ok_or_else(|| corrupt("zstd bit stream ends early"))?;
        let first = self.left / 8;
        let word = (0..8).fold(0u64, |v, i| {
            v | u64::from(self.src.get(first + i).copied().unwrap_or(0)) << (8 * i)
        });
        let v = (word >> (self.left % 8)) & ((1u64 << n) - 1);
        u32::try_from(v).map_err(|_| corrupt("zstd bits"))
    }

    fn is_empty(&self) -> bool {
        self.left == 0
    }
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation)] // synthetic test bytes wrap on purpose
mod tests {
    use super::*;
    use proptest::prelude::*;
    use ruzstd::encoding::{compress_to_vec, CompressionLevel};

    /// Data with repeats near and far, so the encoder writes sequences and FSE tables.
    fn sample(len: usize, seed: u32) -> Vec<u8> {
        let mut x = seed.wrapping_mul(2_654_435_761).max(1);
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            if x.is_multiple_of(3) && out.len() > 64 {
                let back = (x as usize >> 8) % out.len().min(4096) + 1;
                let run = (x as usize >> 20) % 200 + 3;
                for _ in 0..run {
                    out.push(out[out.len() - back]);
                }
            } else {
                out.push((x >> 24) as u8 & 0x3f);
            }
        }
        out.truncate(len);
        out
    }

    #[test]
    fn encoder_frames_measure_their_exact_length() {
        for (len, seed) in [(1usize, 1u32), (2352 * 8, 2), (300_000, 3), (523_872, 4)] {
            let data = sample(len, seed);
            for level in [CompressionLevel::Uncompressed, CompressionLevel::Fastest] {
                let frame = compress_to_vec(&data[..], level);
                assert!(check_frame(&frame, len).is_ok(), "len {len}");
                assert!(check_frame(&frame, len + 1).is_err());
                assert!(check_frame(&frame, len - 1).is_err());
            }
        }
    }

    /// A windowed frame: `blocks` of `(type, size, body)` then nothing.
    fn frame(window_exp: u8, blocks: &[(u32, u32, Vec<u8>)]) -> Vec<u8> {
        let mut f = vec![0x28, 0xb5, 0x2f, 0xfd, 0x00, window_exp << 3];
        for (i, (ty, size, body)) in blocks.iter().enumerate() {
            let last = u32::from(i + 1 == blocks.len());
            let h = last | ty << 1 | size << 3;
            f.extend_from_slice(&h.to_le_bytes()[..3]);
            f.extend_from_slice(body);
        }
        f
    }

    #[test]
    fn a_long_run_of_zero_probabilities_is_refused_at_the_symbol_limit() {
        let mut src = vec![0xFF; 128 << 10];
        src[..2].copy_from_slice(&[0x10, 0xFE]);
        let e = describe(&src, LL).expect_err("too many symbols");
        assert!(e.to_string().contains("too many symbols"), "{e}");
    }

    #[test]
    fn rle_blocks_past_the_hunk_are_refused_before_decoding() {
        let blocks: Vec<_> = (0..100).map(|_| (1, 128 << 10, vec![7])).collect();
        assert!(check_frame(&frame(7, &blocks), 4 * 2448).is_err());
        assert!(check_frame(&frame(13, &blocks), 4 * 2448).is_err());
        let one = frame(7, &[(1, 2448, vec![7])]);
        assert_eq!(check_frame(&one, 2448).expect("fits"), 128 << 10);
    }

    #[test]
    fn a_window_larger_than_the_hunk_and_128_kib_is_refused() {
        let ok = frame(7, &[(1, 100, vec![1])]);
        assert!(check_frame(&ok, 100).is_ok());
        let big = frame(8, &[(1, 100, vec![1])]);
        assert!(check_frame(&big, 100).is_err());
        assert!(check_frame(&big, 256 << 10).is_err(), "length differs");
        let huge = [0x28, 0xb5, 0x2f, 0xfd, 0x00, 16 << 3, 1, 0, 0];
        assert!(check_frame(&huge, 100).is_err());
        assert!(check_frame(&[1, 2, 3], 1).is_err());
        let dict = [0x28, 0xb5, 0x2f, 0xfd, 0x01, 7 << 3, 1];
        assert!(check_frame(&dict, 1).is_err());
    }

    /// One compressed block of `count` sequences in RLE mode: literal length 0, offset code
    /// 0, match length code 52 with all extra bits set, after a 16-byte raw block.
    fn exploding(count: usize) -> Vec<u8> {
        let mut body = vec![0x00]; // raw literals, none
        body.extend_from_slice(&[0xff, (count - 0x7f00) as u8, ((count - 0x7f00) >> 8) as u8]);
        body.extend_from_slice(&[0x54, 0, 0, 52]);
        body.extend(std::iter::repeat_n(0xffu8, count * 2));
        body.push(0x01);
        let size = u32::try_from(body.len()).expect("len");
        frame(7, &[(0, 16, vec![9; 16]), (2, size, body)])
    }

    #[test]
    fn a_block_of_long_matches_is_refused_before_decoding() {
        let f = exploding(40_000);
        let err = check_frame(&f, 2448 * 8).expect_err("refused");
        assert!(err.to_string().contains("128 KiB") || err.to_string().contains("sequences"));
    }

    #[test]
    fn predefined_tables_fill_their_states() {
        for (log, dist) in [
            (6, &LL_DEFAULT[..]),
            (6, &ML_DEFAULT[..]),
            (5, &OF_DEFAULT[..]),
        ] {
            let Table::Fse { states, .. } = build(log, dist).expect("table") else {
                panic!("not FSE");
            };
            assert_eq!(states.len(), 1 << log);
            assert!(states
                .iter()
                .all(|&(_, n, base)| n <= log && base < 1 << log));
        }
        assert!(build(5, &[1; 31]).is_err());
    }

    #[test]
    fn reverse_bits_read_from_the_marker_down() {
        // Bytes 0b1010_0110, 0b0000_0101: marker is bit 2 of the last byte.
        let mut r = Reverse::new(&[0b1010_0110, 0b0000_0101]).expect("stream");
        assert_eq!(r.read(2).expect("bits"), 0b01);
        assert_eq!(r.read(3).expect("bits"), 0b101);
        assert_eq!(r.read(5).expect("bits"), 0b00110);
        assert!(r.is_empty());
        assert!(r.read(1).is_err());
        assert!(Reverse::new(&[1, 0]).is_err());
    }

    proptest! {
        #[test]
        fn any_bytes_are_checked_without_panicking(
            bytes in proptest::collection::vec(any::<u8>(), 0..400),
            expect in 0usize..5000,
        ) {
            let mut f = vec![0x28, 0xb5, 0x2f, 0xfd];
            f.extend_from_slice(&bytes);
            let _ = check_frame(&f, expect);
        }

        #[test]
        fn mutated_frames_fail_or_keep_their_length(
            seed in any::<u32>(),
            flips in proptest::collection::vec((any::<usize>(), any::<u8>()), 1..4),
        ) {
            let data = sample(20_000, seed);
            let mut f = compress_to_vec(&data[..], CompressionLevel::Fastest);
            for (at, x) in flips {
                let i = at % f.len();
                f[i] ^= x.max(1);
            }
            if check_frame(&f, data.len()).is_ok() {
                let mut dec = ruzstd::decoding::FrameDecoder::new();
                let mut src = &f[..];
                if dec.init(&mut src).is_ok()
                    && dec.decode_blocks(&mut src, ruzstd::decoding::BlockDecodingStrategy::All).is_ok()
                {
                    prop_assert!(dec.can_collect() <= data.len());
                }
            }
        }
    }
}
