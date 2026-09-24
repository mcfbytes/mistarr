//! Deterministic pseudo-random bytes, so a synthetic set is identical on every run.

/// A `SplitMix64` generator. Not for anything but fixtures.
#[derive(Debug, Clone)]
pub struct SplitMix(u64);

impl SplitMix {
    /// A generator seeded from `label` with FNV-1a, so each entry name has its own stream.
    ///
    /// ```
    /// use mistarr_fixture::rng::SplitMix;
    /// assert_eq!(SplitMix::from_label("a").next_u64(), SplitMix::from_label("a").next_u64());
    /// ```
    #[must_use]
    pub fn from_label(label: &str) -> Self {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in label.bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        Self(h)
    }

    /// The next 64 pseudo-random bits.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// `len` pseudo-random bytes.
    #[must_use]
    pub fn bytes(&mut self, len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len + 8);
        while out.len() < len {
            out.extend_from_slice(&self.next_u64().to_le_bytes());
        }
        out.truncate(len);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streams_differ_by_label_and_repeat_by_label() {
        let a = SplitMix::from_label("Example Quest (USA)").bytes(1000);
        let b = SplitMix::from_label("Example Quest (USA)").bytes(1000);
        let c = SplitMix::from_label("Example Quest (Europe)").bytes(1000);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 1000);
    }

    #[test]
    fn next_u64_advances() {
        let mut r = SplitMix::from_label("x");
        assert_ne!(r.next_u64(), r.next_u64());
    }
}
