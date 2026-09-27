//! Percent-decoding of URI components.

/// The bytes `%XX` escapes in `text` stand for, in either case; a malformed escape
/// is kept as written.
///
/// ```
/// assert_eq!(mistarr_core::percent_decode("a%20b%2F%zz%4"), b"a b/%zz%4");
/// ```
#[must_use]
pub fn percent_decode(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        let escaped = bytes
            .get(i + 1..i + 3)
            .filter(|_| b == b'%')
            .and_then(|pair| std::str::from_utf8(pair).ok())
            .and_then(crate::hex::decode);
        if let Some(&[byte]) = escaped.as_deref() {
            out.push(byte);
            i += 3;
        } else {
            out.push(b);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn decodes_escapes_and_keeps_the_rest() {
        assert_eq!(percent_decode("%41%2f%"), b"A/%");
        assert_eq!(percent_decode("%ff%00"), [0xff, 0]);
        assert_eq!(percent_decode("a+b"), b"a+b");
        assert_eq!(percent_decode("%%41"), b"%A");
        assert_eq!(percent_decode("%é1"), "%é1".as_bytes());
    }

    proptest! {
        #[test]
        fn inverts_escaping_every_byte(bytes in prop::collection::vec(any::<u8>(), 0..64)) {
            let hex = crate::hex::encode(&bytes).to_uppercase();
            let escaped: String = hex.as_bytes().chunks(2).flat_map(|p| ["%", std::str::from_utf8(p).unwrap_or_default()]).collect();
            prop_assert_eq!(percent_decode(&escaped), bytes);
        }

        #[test]
        fn text_without_escapes_is_unchanged(text in "[^%]*") {
            prop_assert_eq!(percent_decode(&text), text.as_bytes());
        }
    }
}
