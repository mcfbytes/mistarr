//! A bare FLAC frame writer: 16-bit stereo at 44.1 kHz, VERBATIM or FIXED order 0 to 2
//! subframes with Rice-coded residuals, the smaller per channel.

/// MSB-first bit writer.
#[derive(Default)]
pub(crate) struct BitWriter {
    pub(crate) bytes: Vec<u8>,
    used: u32,
}

impl BitWriter {
    /// Appends the low `n` bits of `value`, zero-extended past 64 bits.
    pub(crate) fn put(&mut self, value: u64, n: u32) {
        for i in (0..n).rev() {
            if self.used.is_multiple_of(8) {
                self.bytes.push(0);
            }
            let last = self.bytes.len() - 1;
            let bit = i < 64 && (value >> i) & 1 == 1;
            self.bytes[last] |= u8::from(bit) << (7 - self.used % 8);
            self.used += 1;
        }
    }

    /// Pads with zero bits to a byte boundary.
    pub(crate) fn align(&mut self) {
        self.used = self.used.next_multiple_of(8);
    }
}

fn crc8(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |mut c, &b| {
        c ^= b;
        for _ in 0..8 {
            c = if c & 0x80 != 0 {
                (c << 1) ^ 0x07
            } else {
                c << 1
            };
        }
        c
    })
}

fn crc16(data: &[u8]) -> u16 {
    data.iter().fold(0u16, |mut c, &b| {
        c ^= u16::from(b) << 8;
        for _ in 0..8 {
            c = if c & 0x8000 != 0 {
                (c << 1) ^ 0x8005
            } else {
                c << 1
            };
        }
        c
    })
}

/// FLAC's "UTF-8" coding of a frame number below 2^21.
fn utf8(n: u32) -> Vec<u8> {
    let b = |v: u32| u8::try_from(v & 0xff).unwrap_or(0);
    if n < 0x80 {
        vec![b(n)]
    } else if n < 0x800 {
        vec![b(0xc0 | (n >> 6)), b(0x80 | (n & 0x3f))]
    } else if n < 0x1_0000 {
        vec![
            b(0xe0 | (n >> 12)),
            b(0x80 | ((n >> 6) & 0x3f)),
            b(0x80 | (n & 0x3f)),
        ]
    } else {
        vec![
            b(0xf0 | (n >> 18)),
            b(0x80 | ((n >> 12) & 0x3f)),
            b(0x80 | ((n >> 6) & 0x3f)),
            b(0x80 | (n & 0x3f)),
        ]
    }
}

fn residuals(x: &[i32], order: usize) -> Vec<i32> {
    (order..x.len())
        .map(|n| match order {
            0 => x[n],
            1 => x[n] - x[n - 1],
            _ => x[n] - 2 * x[n - 1] + x[n - 2],
        })
        .collect()
}

fn zigzag(r: i32) -> u64 {
    u64::from(((r << 1) ^ (r >> 31)).cast_unsigned())
}

/// The Rice parameter below 15 that codes `res` in the fewest bits, and that count.
fn rice(res: &[i32]) -> (u32, u64) {
    (0..15u32)
        .map(|k| {
            (
                k,
                res.iter()
                    .map(|&r| (zigzag(r) >> k) + 1 + u64::from(k))
                    .sum(),
            )
        })
        .min_by_key(|&(_, bits)| bits)
        .unwrap_or((0, 0))
}

/// One channel's subframe, the smallest of VERBATIM and FIXED orders 0 to 2.
fn subframe(w: &mut BitWriter, x: &[i32]) {
    let verbatim = 16 * x.len() as u64;
    let best = (0..=2usize)
        .filter(|&o| x.len() > o)
        .map(|o| {
            let res = residuals(x, o);
            let (k, bits) = rice(&res);
            (o, k, res, 16 * o as u64 + 10 + bits)
        })
        .min_by_key(|t| t.3);
    match best {
        Some((order, k, res, bits)) if bits < verbatim => {
            w.put(0b0001_0000 | (order as u64) << 1, 8);
            for &s in &x[..order] {
                w.put(u64::from(s.cast_unsigned() & 0xffff), 16);
            }
            w.put(0, 2);
            w.put(0, 4);
            w.put(u64::from(k), 4);
            for r in res {
                let u = zigzag(r);
                w.put(0, u32::try_from(u >> k).unwrap_or(0));
                w.put(1, 1);
                w.put(u & ((1u64 << k) - 1), k);
            }
        }
        _ => {
            w.put(0b0000_0010, 8);
            for &s in x {
                w.put(u64::from(s.cast_unsigned() & 0xffff), 16);
            }
        }
    }
}

/// FLAC frames of the interleaved stereo `samples` in blocks of `block` samples.
pub(crate) fn encode(samples: &[(i16, i16)], block: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for (number, chunk) in samples.chunks(block.max(1)).enumerate() {
        let mut head = vec![0xff, 0xf8, 0x79, 0x18];
        head.extend_from_slice(&utf8(u32::try_from(number).unwrap_or(0)));
        head.extend_from_slice(&u16::try_from(chunk.len() - 1).unwrap_or(0).to_be_bytes());
        head.push(crc8(&head));
        let mut w = BitWriter {
            bytes: head,
            used: 0,
        };
        w.used = u32::try_from(w.bytes.len() * 8).unwrap_or(0);
        let left: Vec<i32> = chunk.iter().map(|s| i32::from(s.0)).collect();
        let right: Vec<i32> = chunk.iter().map(|s| i32::from(s.1)).collect();
        subframe(&mut w, &left);
        subframe(&mut w, &right);
        w.align();
        let crc = crc16(&w.bytes);
        out.extend_from_slice(&w.bytes);
        out.extend_from_slice(&crc.to_be_bytes());
    }
    out
}

/// Stereo samples of `bytes` read as big-endian 16-bit pairs.
pub(crate) fn samples_be(bytes: &[u8]) -> Vec<(i16, i16)> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| {
            (
                i16::from_be_bytes([c[0], c[1]]),
                i16::from_be_bytes([c[2], c[3]]),
            )
        })
        .collect()
}

/// Stereo samples of `bytes` read as little-endian 16-bit pairs.
pub(crate) fn samples_le(bytes: &[u8]) -> Vec<(i16, i16)> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| {
            (
                i16::from_le_bytes([c[0], c[1]]),
                i16::from_le_bytes([c[2], c[3]]),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(frames: &[u8], n: usize) -> Vec<(i16, i16)> {
        let mut r = claxon::frame::FrameReader::new(std::io::Cursor::new(frames));
        let mut out = Vec::new();
        while out.len() < n {
            let block = r
                .read_next_or_eof(Vec::new())
                .expect("frame")
                .expect("more");
            for (l, rr) in block.stereo_samples() {
                out.push((
                    i16::try_from(l).expect("16"),
                    i16::try_from(rr).expect("16"),
                ));
            }
        }
        assert_eq!(r.into_inner().position(), frames.len() as u64);
        out
    }

    #[test]
    fn frames_decode_with_claxon() {
        let smooth: Vec<(i16, i16)> = (0..3000i32)
            .map(|i| {
                (
                    i16::try_from((i % 400) * 50 - 10_000).expect("16"),
                    i16::try_from(-i).expect("16"),
                )
            })
            .collect();
        assert_eq!(decode(&encode(&smooth, 1176), smooth.len()), smooth);
        let noisy: Vec<(i16, i16)> = (0..700u32)
            .map(|i| {
                let v = i.wrapping_mul(2_654_435_761) >> 16;
                (
                    u16::try_from(v & 0xffff).expect("16").cast_signed(),
                    i16::MIN,
                )
            })
            .collect();
        assert_eq!(decode(&encode(&noisy, 588), noisy.len()), noisy);
        assert!(
            encode(&smooth, 1176).len() < smooth.len() * 4 / 2,
            "FIXED beats VERBATIM"
        );
    }

    #[test]
    fn crcs_match_their_check_values() {
        assert_eq!(crc8(b"123456789"), 0xf4);
        assert_eq!(crc16(b"123456789"), 0xfee8);
        assert_eq!(utf8(0x7f), [0x7f]);
        assert_eq!(utf8(0x80), [0xc2, 0x80]);
    }
}
