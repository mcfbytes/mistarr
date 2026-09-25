//! MSB-first bit reader for the compressed hunk map; see `docs/CHD.md` "Map".

/// Reads bits MSB first; past the end it returns zero bits and records the overflow.
pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    overflow: bool,
}

impl<'a> BitReader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            overflow: false,
        }
    }

    fn bit(&self, at: usize) -> u32 {
        self.data
            .get(at / 8)
            .map_or(0, |b| u32::from(b >> (7 - at % 8)) & 1)
    }

    /// The next `n` bits (at most 32) without consuming them.
    pub(crate) fn peek(&self, n: u32) -> u32 {
        (0..n as usize).fold(0u32, |v, i| (v << 1) | self.bit(self.pos + i))
    }

    /// Consumes `n` bits, flagging an overflow when that passes the end.
    pub(crate) fn skip(&mut self, n: u32) {
        self.pos += n as usize;
        if self.pos > self.data.len() * 8 {
            self.overflow = true;
        }
    }

    /// Reads `n` bits (at most 32) as an unsigned value.
    pub(crate) fn read(&mut self, n: u32) -> u32 {
        let v = self.peek(n);
        self.skip(n);
        v
    }

    /// Reads 16 bits.
    pub(crate) fn read_u16(&mut self) -> u16 {
        u16::try_from(self.read(16)).unwrap_or(0)
    }

    /// Whether any read went past the end of the data.
    pub(crate) fn overflowed(&self) -> bool {
        self.overflow
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn reads_msb_first_across_bytes() {
        let mut r = BitReader::new(&[0b1010_0000, 0b1111_0001]);
        assert_eq!(r.read(1), 1);
        assert_eq!(r.read(3), 0b010);
        assert_eq!(r.peek(8), 0b0000_1111);
        assert_eq!(r.read(8), 0b0000_1111);
        assert_eq!(r.read(4), 0b0001);
        assert!(!r.overflowed());
        assert_eq!(r.read(0), 0);
        assert!(!r.overflowed());
    }

    #[test]
    fn past_the_end_reads_zero_and_flags_overflow() {
        let mut r = BitReader::new(&[0xff]);
        assert_eq!(r.read(12), 0xff0);
        assert!(r.overflowed());
        let mut r = BitReader::new(&[0x12, 0x34, 0x56, 0x78]);
        assert_eq!(r.read(32), 0x1234_5678);
        assert!(!r.overflowed());
    }

    proptest! {
        #[test]
        fn never_panics(data in proptest::collection::vec(any::<u8>(), 0..8),
                        widths in proptest::collection::vec(0u32..=32, 0..20)) {
            let mut r = BitReader::new(&data);
            for w in widths {
                let _ = r.read(w);
            }
        }
    }
}
