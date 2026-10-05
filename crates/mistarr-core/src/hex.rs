//! Lowercase hex encoding and decoding of byte strings.

use std::fmt;

/// The two lowercase hex digits of `b`.
fn pair(b: u8) -> [char; 2] {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    [
        char::from(DIGITS[usize::from(b >> 4)]),
        char::from(DIGITS[usize::from(b & 0xf)]),
    ]
}

/// Lowercase hex digits of `bytes`, two per byte.
///
/// ```
/// assert_eq!(mistarr_core::hex::encode(&[0x0a, 0xff]), "0aff");
/// ```
#[must_use]
pub fn encode(bytes: &[u8]) -> String {
    bytes.iter().flat_map(|&b| pair(b)).collect()
}

/// Writes the lowercase hex digits of `bytes` to `out`, as [`encode`] without allocating.
///
/// # Errors
///
/// Whatever `out` returns.
///
/// ```
/// let mut s = String::new();
/// mistarr_core::hex::write(&mut s, &[0x0a, 0xff]).unwrap();
/// assert_eq!(s, "0aff");
/// ```
pub fn write(out: &mut impl fmt::Write, bytes: &[u8]) -> fmt::Result {
    bytes
        .iter()
        .try_for_each(|&b| pair(b).into_iter().try_for_each(|c| out.write_char(c)))
}

/// Fills `out` with the bytes `text` stands for, as [`decode`] without allocating;
/// `None`, with `out` partly written, unless `text` is exactly `2 * out.len()` hex digits.
///
/// ```
/// let mut out = [0; 2];
/// assert_eq!(mistarr_core::hex::decode_into("0aFF", &mut out), Some(()));
/// assert_eq!(out, [0x0a, 0xff]);
/// assert_eq!(mistarr_core::hex::decode_into("0a", &mut out), None);
/// ```
pub fn decode_into(text: &str, out: &mut [u8]) -> Option<()> {
    let (pairs, rest) = text.as_bytes().as_chunks::<2>();
    if pairs.len() != out.len() || !rest.is_empty() {
        return None;
    }
    for (b, &[hi, lo]) in out.iter_mut().zip(pairs) {
        *b = digit(hi)? << 4 | digit(lo)?;
    }
    Some(())
}

/// The bytes an even number of hex digits in either case stand for; `None` for
/// anything else.
///
/// ```
/// use mistarr_core::hex::decode;
/// assert_eq!(decode("0aFF"), Some(vec![0x0a, 0xff]));
/// assert_eq!(decode("0a f"), None);
/// ```
#[must_use]
pub fn decode(text: &str) -> Option<Vec<u8>> {
    let mut out = vec![0; text.len() / 2];
    decode_into(text, &mut out)?;
    Some(out)
}

/// As [`decode`], ignoring ASCII whitespace anywhere, as a DAT `header` attribute
/// such as `4E 45 53 1A` is written.
///
/// ```
/// use mistarr_core::hex::decode_spaced;
/// assert_eq!(decode_spaced("4E 45 53\t1a"), Some(b"NES\x1a".to_vec()));
/// assert_eq!(decode_spaced("4E 4"), None);
/// ```
#[must_use]
pub fn decode_spaced(text: &str) -> Option<Vec<u8>> {
    let digits: String = text.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    decode(&digits)
}

/// The value of one hex digit.
fn digit(b: u8) -> Option<u8> {
    char::from(b)
        .to_digit(16)
        .and_then(|d| u8::try_from(d).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn encodes_lowercase_pairs() {
        assert_eq!(encode(&[]), "");
        assert_eq!(encode(&[0, 0x9a, 0xbc]), "009abc");
    }

    #[test]
    fn decodes_either_case_and_refuses_the_rest() {
        assert_eq!(decode(""), Some(Vec::new()));
        assert_eq!(decode("AbCd"), Some(vec![0xab, 0xcd]));
        for bad in ["a", "zz", "0x", "é0", "+1"] {
            assert_eq!(decode(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn writes_and_decodes_in_place() {
        let mut s = String::new();
        write(&mut s, &[0, 0x9a]).expect("write");
        assert_eq!(s, "009a");
        let mut out = [0; 2];
        assert_eq!(decode_into("009A", &mut out), Some(()));
        assert_eq!(out, [0, 0x9a]);
        for bad in ["", "009", "009a00", "009g"] {
            assert_eq!(decode_into(bad, &mut out), None, "{bad:?}");
        }
        assert_eq!(decode_into("", &mut []), Some(()));
    }

    #[test]
    fn spaced_ignores_whitespace_only() {
        assert_eq!(decode_spaced(" 0 1\n"), Some(vec![0x01]));
        assert_eq!(decode_spaced("01,02"), None);
        assert_eq!(decode_spaced(""), Some(Vec::new()));
    }

    proptest! {
        #[test]
        fn decode_inverts_encode(bytes in prop::collection::vec(any::<u8>(), 0..64)) {
            let text = encode(&bytes);
            prop_assert_eq!(decode(&text), Some(bytes.clone()));
            prop_assert_eq!(decode(&text.to_ascii_uppercase()), Some(bytes.clone()));
            let spaced = text.as_bytes().chunks(2).map(|p| String::from_utf8_lossy(p)).collect::<Vec<_>>().join(" ");
            prop_assert_eq!(decode_spaced(&spaced), Some(bytes));
        }

        #[test]
        fn decode_never_panics(text in "\\PC*") {
            let _ = decode(&text);
            let _ = decode_spaced(&text);
        }

        #[test]
        fn decode_into_agrees_with_decode(text in "[0-9a-fA-Fg é]{0,10}|\\PC*", len in 0usize..6) {
            let mut out = vec![0; len];
            let into = decode_into(&text, &mut out).map(|()| out);
            prop_assert_eq!(into, decode(&text).filter(|b| b.len() == len));
        }
    }
}
