//! CD sector helpers: the hunk CRC16 and ECC regeneration; see `docs/CHD.md` "ECC".

#[allow(clippy::cast_possible_truncation)] // indexes are below 256
const fn crc16_table() -> [u16; 256] {
    let mut t = [0u16; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = (i as u16) << 8;
        let mut b = 0;
        while b < 8 {
            c = if c & 0x8000 != 0 {
                (c << 1) ^ 0x1021
            } else {
                c << 1
            };
            b += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}

static CRC16: [u16; 256] = crc16_table();

/// CRC-16/IBM-3740 (poly 0x1021, init 0xFFFF), the checksum of every CHD hunk and map.
///
/// ```
/// assert_eq!(mistarr_core::chd::crc16(b"123456789"), 0x29b1);
/// ```
#[must_use]
pub fn crc16(data: &[u8]) -> u16 {
    crc16_update(0xffff, data)
}

/// Continues a [`crc16`] over more bytes.
pub(crate) fn crc16_update(crc: u16, data: &[u8]) -> u16 {
    data.iter().fold(crc, |crc, &b| {
        (crc << 8) ^ CRC16[usize::from((crc >> 8) as u8 ^ b)]
    })
}

/// P and Q parity of CD-ROM sectors as MAME computes it; see `docs/CHD.md` "ECC".
pub mod ecc {
    const P_OFFSET: usize = 0x81c;
    const P_ROWS: usize = 86;
    const P_COMPONENTS: usize = 24;
    const Q_OFFSET: usize = 0x8c8;
    const Q_ROWS: usize = 52;
    const Q_COMPONENTS: usize = 43;

    #[allow(clippy::cast_possible_truncation)] // indexes are below 256
    const fn tables() -> ([u8; 256], [u8; 256]) {
        let mut low = [0u8; 256];
        let mut high = [0u8; 256];
        let mut i = 0;
        while i < 256 {
            let doubled = (i << 1) ^ if i & 0x80 != 0 { 0x11d } else { 0 };
            low[i] = doubled as u8;
            high[i ^ low[i] as usize] = i as u8;
            i += 1;
        }
        (low, high)
    }

    static TABLES: ([u8; 256], [u8; 256]) = tables();

    /// Byte `off` of the ECC source (the sector from its header on); Mode 2 zeroes the header.
    fn source(sector: &[u8; 2352], off: usize) -> u8 {
        if sector[15] == 2 && off < 4 {
            0
        } else {
            sector[12 + off]
        }
    }

    fn row(sector: &[u8; 2352], offsets: impl Iterator<Item = usize>) -> (u8, u8) {
        let (low, high) = (&TABLES.0, &TABLES.1);
        let (mut v1, mut v2) = (0u8, 0u8);
        for off in offsets {
            let s = source(sector, off);
            v1 ^= s;
            v2 ^= s;
            v1 = low[usize::from(v1)];
        }
        v1 = high[usize::from(low[usize::from(v1)] ^ v2)];
        (v1, v2 ^ v1)
    }

    fn p_row(b: usize) -> impl Iterator<Item = usize> {
        (0..P_COMPONENTS).map(move |c| b + P_ROWS * c)
    }

    fn q_row(b: usize) -> impl Iterator<Item = usize> {
        (0..Q_COMPONENTS).map(move |c| 2 * ((43 * (b / 2) + 44 * c) % 1118) + b % 2)
    }

    /// Writes the P and Q parity of `sector`; in Mode 2 the 4 header bytes count as zero.
    ///
    /// ```
    /// let mut s = [0u8; 2352];
    /// s[15] = 1;
    /// s[16] = 0x5a;
    /// mistarr_core::chd::ecc::generate(&mut s);
    /// assert!(mistarr_core::chd::ecc::verify(&s));
    /// ```
    pub fn generate(sector: &mut [u8; 2352]) {
        for b in 0..P_ROWS {
            let (x, y) = row(sector, p_row(b));
            sector[P_OFFSET + b] = x;
            sector[P_OFFSET + P_ROWS + b] = y;
        }
        for b in 0..Q_ROWS {
            let (x, y) = row(sector, q_row(b));
            sector[Q_OFFSET + b] = x;
            sector[Q_OFFSET + Q_ROWS + b] = y;
        }
    }

    /// Whether the stored P and Q parity of `sector` is what [`generate`] would write.
    ///
    /// ```
    /// let mut s = [0u8; 2352];
    /// s[15] = 1;
    /// s[16] = 1;
    /// assert!(!mistarr_core::chd::ecc::verify(&s));
    /// ```
    #[must_use]
    pub fn verify(sector: &[u8; 2352]) -> bool {
        (0..P_ROWS)
            .all(|b| row(sector, p_row(b)) == (sector[P_OFFSET + b], sector[P_OFFSET + P_ROWS + b]))
            && (0..Q_ROWS).all(|b| {
                row(sector, q_row(b)) == (sector[Q_OFFSET + b], sector[Q_OFFSET + Q_ROWS + b])
            })
    }
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation)] // synthetic test bytes wrap on purpose
mod tests {
    use super::*;

    /// A small deterministic byte stream for sector contents.
    fn fill(seed: u32, buf: &mut [u8]) {
        let mut x = seed.wrapping_mul(2_654_435_761) | 1;
        for b in buf {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            *b = x as u8;
        }
    }

    fn sector(seed: u32, mode: u8) -> [u8; 2352] {
        let mut s = [0u8; 2352];
        fill(seed, &mut s[12..0x81c]);
        s[..12].copy_from_slice(&[
            0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0,
        ]);
        s[15] = mode;
        s
    }

    /// GF(2^8) product by shift and add, with the ECMA-130 polynomial 0x11D.
    fn gf_mul(mut a: u8, mut b: u8) -> u8 {
        let mut p = 0u8;
        while b != 0 {
            if b & 1 != 0 {
                p ^= a;
            }
            let carry = a & 0x80 != 0;
            a <<= 1;
            if carry {
                a ^= 0x1d;
            }
            b >>= 1;
        }
        p
    }

    fn alpha_pow(n: usize) -> u8 {
        (0..n).fold(1u8, |v, _| gf_mul(v, 2))
    }

    /// Byte `off` of the parity source counted from the header, header zeroed in Mode 2.
    fn src(s: &[u8; 2352], off: usize) -> u8 {
        if s[15] == 2 && off < 4 {
            0
        } else {
            s[12 + off]
        }
    }

    /// Both syndromes of one codeword whose last two symbols are its parity.
    fn syndromes(word: &[u8]) -> (u8, u8) {
        let n = word.len();
        word.iter().enumerate().fold((0, 0), |(s0, s1), (i, &v)| {
            (s0 ^ v, s1 ^ gf_mul(alpha_pow(n - 1 - i), v))
        })
    }

    /// ECMA-130 P words are RS(26,24) columns and Q words RS(45,43) diagonals; every
    /// codeword of a correct sector has zero syndromes.
    fn ecma130_holds(s: &[u8; 2352]) -> bool {
        let p_ok = (0..86).all(|b| {
            let word: Vec<u8> = (0..26).map(|c| src(s, b + 86 * c)).collect();
            syndromes(&word) == (0, 0)
        });
        let q_ok = (0..52).all(|b| {
            let mut word: Vec<u8> = (0..43)
                .map(|c| src(s, 2 * ((43 * (b / 2) + 44 * c) % 1118) + b % 2))
                .collect();
            word.push(s[0x8c8 + b]);
            word.push(s[0x8c8 + 52 + b]);
            syndromes(&word) == (0, 0)
        });
        p_ok && q_ok
    }

    #[test]
    fn crc16_matches_the_check_value() {
        assert_eq!(crc16(b"123456789"), 0x29b1);
        assert_eq!(crc16(b""), 0xffff);
        assert_eq!(crc16_update(crc16(b"1234"), b"56789"), 0x29b1);
    }

    #[test]
    fn generated_parity_verifies_in_mode_1_and_mode_2() {
        for mode in [1u8, 2] {
            let mut s = sector(7, mode);
            assert!(!ecc::verify(&s));
            ecc::generate(&mut s);
            assert!(ecc::verify(&s));
            s[100] ^= 1;
            assert!(!ecc::verify(&s));
        }
    }

    #[test]
    fn mode_2_parity_ignores_the_header() {
        let mut a = sector(9, 2);
        ecc::generate(&mut a);
        let mut b = a;
        b[12..15].copy_from_slice(&[1, 2, 3]);
        assert!(ecc::verify(&b), "the Mode 2 header is not covered");
        let mut c = sector(9, 1);
        ecc::generate(&mut c);
        c[12] ^= 1;
        assert!(!ecc::verify(&c), "the Mode 1 header is covered");
    }

    #[test]
    fn parity_matches_an_independent_ecma_130_reference() {
        for seed in 0..64u32 {
            let mode = if seed % 2 == 0 { 1 } else { 2 };
            let mut s = sector(seed + 100, mode);
            ecc::generate(&mut s);
            assert!(ecma130_holds(&s), "seed {seed}");
            s[0x81c + seed as usize] ^= 0x40;
            assert!(!ecma130_holds(&s));
        }
    }
}
