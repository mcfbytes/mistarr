//! Parsing magnet links (`magnet:?...` URIs) into an infohash and display
//! name. Trackers are parsed only to be discarded; see `docs/PRINCIPLES.md`.

use crate::{percent_decode, InfoHash};

/// Why a magnet link was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum MagnetError {
    /// The string did not start with the `magnet:?` scheme.
    #[error("not a magnet URI")]
    NotAMagnetUri,
    /// No `xt=urn:btih:...` parameter was present.
    #[error("magnet URI has no urn:btih topic")]
    MissingTopic,
    /// The `urn:btih:` value was not 40 hex characters or 32 base32 characters.
    #[error("magnet URI has an invalid infohash encoding")]
    InvalidHash,
}

/// A parsed magnet link. Deliberately has no tracker field: trackers in the
/// URI are read and dropped, never stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Magnet {
    /// The `BitTorrent` v1 infohash from the `xt=urn:btih:` parameter.
    pub infohash: InfoHash,
    /// The `dn` parameter, percent-decoded, if present.
    pub display_name: Option<String>,
}

/// Parses a magnet URI. Each value is percent-decoded; an `xt` value starting
/// `urn:btih:` in any case carries the infohash in 40 hex or 32 base32 characters.
///
/// # Errors
///
/// Returns [`MagnetError`] when the URI lacks the `magnet:?` scheme, has no
/// `urn:btih:` topic, or that topic is not a valid hex or base32 hash.
///
/// ```
/// use mistarr_core::magnet::parse_magnet;
/// let hex = "0".repeat(40);
/// let uri = format!("magnet:?xt=urn:btih:{hex}&dn=Example&tr=http%3A%2F%2Ftracker.invalid%2Fannounce");
/// let magnet = parse_magnet(&uri).unwrap();
/// assert_eq!(magnet.display_name.as_deref(), Some("Example"));
/// ```
pub fn parse_magnet(uri: &str) -> Result<Magnet, MagnetError> {
    let query = uri
        .trim()
        .strip_prefix("magnet:?")
        .ok_or(MagnetError::NotAMagnetUri)?;

    let mut infohash = None;
    let mut display_name = None;
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        match key {
            "xt" => {
                let topic = String::from_utf8_lossy(&percent_decode(value)).into_owned();
                if let Some(hash) = topic
                    .get(..9)
                    .filter(|p| p.eq_ignore_ascii_case("urn:btih:"))
                    .and_then(|_| topic.get(9..))
                {
                    infohash = Some(decode_btih(hash)?);
                }
            }
            "dn" => display_name = Some(form_decode(value)),
            _ => {}
        }
    }

    let infohash = infohash.ok_or(MagnetError::MissingTopic)?;
    Ok(Magnet {
        infohash,
        display_name,
    })
}

fn decode_btih(topic: &str) -> Result<InfoHash, MagnetError> {
    match topic.len() {
        40 => topic.parse().map_err(|_| MagnetError::InvalidHash),
        32 => decode_base32_20(topic).map(InfoHash::from_bytes),
        _ => Err(MagnetError::InvalidHash),
    }
}

// RFC4648 base32, either case, no padding: 32 chars * 5 bits = 160 bits = 20 bytes.
fn decode_base32_20(s: &str) -> Result<[u8; 20], MagnetError> {
    let mut out = [0u8; 20];
    let mut buffer: u64 = 0;
    let mut bits: u32 = 0;
    let mut out_pos = 0usize;
    for c in s.bytes() {
        let val = match c.to_ascii_uppercase() {
            c @ b'A'..=b'Z' => c - b'A',
            c @ b'2'..=b'7' => c - b'2' + 26,
            _ => return Err(MagnetError::InvalidHash),
        };
        buffer = (buffer << 5) | u64::from(val);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            let byte =
                u8::try_from((buffer >> bits) & 0xFF).map_err(|_| MagnetError::InvalidHash)?;
            *out.get_mut(out_pos).ok_or(MagnetError::InvalidHash)? = byte;
            out_pos += 1;
        }
    }
    if out_pos != 20 {
        return Err(MagnetError::InvalidHash);
    }
    Ok(out)
}

// The `dn` parameter is form-encoded by common torrent clients, so `+` is a
// space alongside `%20`; text that decodes to invalid UTF-8 is kept as written.
fn form_decode(s: &str) -> String {
    String::from_utf8(percent_decode(&s.replace('+', " "))).unwrap_or_else(|_| s.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn parses_hex_infohash() {
        let hex = "ab".repeat(20);
        let uri = format!("magnet:?xt=urn:btih:{hex}");
        let magnet = parse_magnet(&uri).unwrap();
        assert_eq!(magnet.infohash, InfoHash::from_bytes([0xab; 20]));
        assert_eq!(magnet.display_name, None);
    }

    #[test]
    fn parses_base32_infohash() {
        // "AAAA…" is 32 zero-bit quintets: 20 zero bytes.
        let uri = "magnet:?xt=urn:btih:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let magnet = parse_magnet(uri).unwrap();
        assert_eq!(magnet.infohash, InfoHash::from_bytes([0u8; 20]));
        let lower = parse_magnet(&format!("magnet:?xt=urn:btih:{}", "a".repeat(32)));
        assert_eq!(lower.unwrap().infohash, InfoHash::from_bytes([0u8; 20]));
    }

    #[test]
    fn topic_prefix_is_matched_in_any_case_and_percent_decoded() {
        let hex = "cd".repeat(20);
        for uri in [
            format!("magnet:?xt=URN:BTIH:{hex}"),
            format!("magnet:?xt=urn%3Abtih%3A{hex}"),
            format!("magnet:?xt=Urn%3aBtIh%3a{}", hex.to_uppercase()),
        ] {
            assert_eq!(
                parse_magnet(&uri).unwrap().infohash,
                InfoHash::from_bytes([0xcd; 20]),
                "{uri}"
            );
        }
        let v2 = format!("magnet:?xt=urn:btmh:{hex}");
        assert_eq!(parse_magnet(&v2), Err(MagnetError::MissingTopic));
    }

    #[test]
    fn reads_base32_bits_and_an_escaped_topic_after_other_parameters() {
        let ones = format!("magnet:?xt=URN:BTIH:{}", "7".repeat(32));
        assert_eq!(
            parse_magnet(&ones).unwrap().infohash,
            InfoHash::from_bytes([0xff; 20])
        );
        let mixed = "magnet:?xt=urn:btih:AEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIB";
        assert_eq!(
            parse_magnet(mixed).unwrap().infohash,
            InfoHash::from_bytes([1; 20])
        );
        let encoded = format!("magnet:?dn=a%20b&xt=urn%3Abtih%3A{}", "5e".repeat(20));
        assert_eq!(
            parse_magnet(&encoded).unwrap().infohash,
            InfoHash::from_bytes([0x5e; 20])
        );
        for bad in [
            "magnet:?xt=urn:sha1:",
            "magnet:?xt=urn%3Abtih%3",
            "magnet:?xt=urn%ZZbtih",
        ] {
            assert_eq!(parse_magnet(bad), Err(MagnetError::MissingTopic), "{bad}");
        }
    }

    #[test]
    fn discards_trackers_and_keeps_display_name() {
        let hex = "11".repeat(20);
        let uri = format!(
            "magnet:?xt=urn:btih:{hex}&tr=http%3A%2F%2Ftracker.invalid%2Fannounce&dn=My%20Set"
        );
        let magnet = parse_magnet(&uri).unwrap();
        assert_eq!(magnet.display_name.as_deref(), Some("My Set"));
    }

    #[test]
    fn trims_surrounding_whitespace_and_newline() {
        let hex = "22".repeat(20);
        let uri = format!("  magnet:?xt=urn:btih:{hex}\n");
        assert_eq!(
            parse_magnet(&uri).unwrap().infohash,
            InfoHash::from_bytes([0x22; 20])
        );
    }

    #[test]
    fn display_name_is_form_decoded() {
        let hex = "33".repeat(20);
        for (dn, want) in [
            ("My+Set+Name", "My Set Name"),
            ("A%2BB", "A+B"),
            ("bad%ff+x", "bad%ff+x"),
        ] {
            let uri = format!("magnet:?xt=urn:btih:{hex}&dn={dn}");
            assert_eq!(
                parse_magnet(&uri).unwrap().display_name.as_deref(),
                Some(want)
            );
        }
    }

    #[test]
    fn rejects_non_magnet_and_bad_hash() {
        assert_eq!(
            parse_magnet("http://example.invalid"),
            Err(MagnetError::NotAMagnetUri)
        );
        assert_eq!(
            parse_magnet("magnet:?xt=urn:btih:zz"),
            Err(MagnetError::InvalidHash)
        );
        let bad32 = format!("magnet:?xt=urn:btih:{}", "1".repeat(32));
        assert_eq!(parse_magnet(&bad32), Err(MagnetError::InvalidHash));
        let bad40 = format!("magnet:?xt=urn:btih:{}", "g".repeat(40));
        assert_eq!(parse_magnet(&bad40), Err(MagnetError::InvalidHash));
        assert_eq!(
            parse_magnet("magnet:?dn=only"),
            Err(MagnetError::MissingTopic)
        );
    }

    proptest! {
        #[test]
        fn never_panics(s in "\\PC*") {
            let _ = parse_magnet(&s);
        }

        #[test]
        fn reads_any_hash_in_any_encoding(bytes in any::<[u8; 20]>(), upper in any::<bool>(), escape in any::<bool>()) {
            let mut hex = crate::hex::encode(&bytes);
            if upper {
                hex = hex.to_uppercase();
            }
            let prefix = if escape { "urn%3Abtih%3A" } else { "urn:btih:" };
            let uri = format!("magnet:?dn=x&xt={prefix}{hex}");
            prop_assert_eq!(parse_magnet(&uri).map(|m| m.infohash), Ok(InfoHash::from_bytes(bytes)));
        }
    }
}
