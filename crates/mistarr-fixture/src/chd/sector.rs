//! Synthetic CD sectors: Mode 1, Mode 2 Form 1 and audio, with EDC and ECC.

use mistarr_core::chd::ecc;

use crate::rng::SplitMix;

/// The 12-byte sync pattern at the start of every data sector.
pub(crate) const SYNC: [u8; 12] = [
    0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0,
];

#[allow(clippy::cast_possible_truncation)] // indexes are below 256
const fn edc_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut b = 0;
        while b < 8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0xd801_8001
            } else {
                c >> 1
            };
            b += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}

static EDC: [u32; 256] = edc_table();

/// The CD-ROM EDC over `data`.
pub(crate) fn edc(data: &[u8]) -> u32 {
    data.iter().fold(0u32, |e, &b| {
        (e >> 8) ^ EDC[usize::from(e.to_le_bytes()[0] ^ b)]
    })
}

fn bcd(v: u32) -> u8 {
    u8::try_from((v / 10) * 16 + v % 10).unwrap_or(0)
}

/// The BCD minute, second and frame of a sector at `lba`, counting the 150-frame lead-in.
pub(crate) fn msf(lba: u32) -> [u8; 3] {
    let f = lba + 150;
    [bcd(f / 4500), bcd((f / 75) % 60), bcd(f % 75)]
}

/// 2048 bytes of low-entropy user data; every fifth sector repeats one 512-byte block.
pub(crate) fn user_data(rng: &mut SplitMix, index: u32) -> [u8; 2048] {
    let mut out = [0u8; 2048];
    if index % 5 == 4 {
        let block: Vec<u8> = rng.bytes(512).iter().map(|b| b'A' + (b & 15)).collect();
        for chunk in out.chunks_mut(512) {
            chunk.copy_from_slice(&block);
        }
    } else {
        for (o, b) in out.iter_mut().zip(rng.bytes(2048)) {
            *o = b'A' + (b & 15);
        }
    }
    out
}

/// A raw Mode 1 sector at `lba` holding `data`.
pub(crate) fn mode1(lba: u32, data: &[u8; 2048]) -> [u8; 2352] {
    let mut s = [0u8; 2352];
    s[..12].copy_from_slice(&SYNC);
    s[12..15].copy_from_slice(&msf(lba));
    s[15] = 1;
    s[16..0x810].copy_from_slice(data);
    let e = edc(&s[..0x810]);
    s[0x810..0x814].copy_from_slice(&e.to_le_bytes());
    ecc::generate(&mut s);
    s
}

/// A raw Mode 2 Form 1 sector at `lba` holding `data`; its ECC counts the header as zero.
pub(crate) fn mode2_form1(lba: u32, data: &[u8; 2048]) -> [u8; 2352] {
    let mut s = [0u8; 2352];
    s[..12].copy_from_slice(&SYNC);
    s[12..15].copy_from_slice(&msf(lba));
    s[15] = 2;
    let sub = [0, 0, 0x08, 0];
    s[16..20].copy_from_slice(&sub);
    s[20..24].copy_from_slice(&sub);
    s[24..0x818].copy_from_slice(data);
    let e = edc(&s[16..0x818]);
    s[0x818..0x81c].copy_from_slice(&e.to_le_bytes());
    ecc::generate(&mut s);
    s
}

fn triangle(n: i64, period: i64, amp: i64) -> i64 {
    let x = n.rem_euclid(period);
    let half = period / 2;
    let ramp = if x < half { x } else { period - x };
    (ramp * 2 * amp) / period.max(1) * 2 - amp
}

/// One audio sector, little-endian as in a `.bin`: a smooth wave plus low noise.
pub(crate) fn audio(rng: &mut SplitMix, first_sample: i64) -> [u8; 2352] {
    let mut s = [0u8; 2352];
    let noise = rng.bytes(588 * 2);
    for (j, frame) in (0i64..).zip(s.as_chunks_mut::<4>().0) {
        let n = first_sample + j;
        let at = usize::try_from(j).unwrap_or(0) * 2;
        let l = triangle(n, 200, 8000) + i64::from(noise[at] % 7) - 3;
        let r = triangle(n, 317, 6000) + i64::from(noise[at + 1] % 7) - 3;
        let l = i16::try_from(l).unwrap_or(0).to_le_bytes();
        let r = i16::try_from(r).unwrap_or(0).to_le_bytes();
        frame[..2].copy_from_slice(&l);
        frame[2..].copy_from_slice(&r);
    }
    s
}

/// A cue sheet time for `frames` frames.
pub(crate) fn cue_time(frames: u32) -> String {
    format!(
        "{:02}:{:02}:{:02}",
        frames / 4500,
        (frames / 75) % 60,
        frames % 75
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msf_is_bcd_after_the_lead_in() {
        assert_eq!(msf(0), [0x00, 0x02, 0x00]);
        assert_eq!(msf(4500 - 150 + 76), [0x01, 0x01, 0x01]);
    }

    #[test]
    fn data_sectors_carry_valid_ecc() {
        let mut rng = SplitMix::from_label("sector");
        let data = user_data(&mut rng, 4);
        assert_eq!(&data[..512], &data[512..1024]);
        assert!(ecc::verify(&mode1(10, &data)));
        let m2 = mode2_form1(10, &data);
        assert!(ecc::verify(&m2));
        assert_eq!(edc(&m2[16..0x818]).to_le_bytes(), m2[0x818..0x81c]);
    }

    #[test]
    fn audio_is_smooth_and_deterministic() {
        let a = audio(&mut SplitMix::from_label("a"), 0);
        let b = audio(&mut SplitMix::from_label("a"), 0);
        assert_eq!(a, b);
        let l0 = i16::from_le_bytes([a[0], a[1]]);
        let l1 = i16::from_le_bytes([a[4], a[5]]);
        assert!((i32::from(l0) - i32::from(l1)).abs() < 400);
    }

    #[test]
    fn cue_times_count_75_frames_a_second() {
        assert_eq!(cue_time(150), "00:02:00");
        assert_eq!(cue_time(4500 + 76), "01:01:01");
    }
}
