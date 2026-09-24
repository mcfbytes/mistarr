//! Parsing `.magnet` files (`magnet:?...` URIs) into an infohash and display
//! name. Trackers are parsed only to be discarded; see `docs/PRINCIPLES.md`.

use crate::error::SourceError;

/// A parsed magnet link. Deliberately has no tracker field: trackers in the
/// URI are read and dropped, never stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Magnet {
    /// The `BitTorrent` v1 infohash from the `xt=urn:btih:` parameter.
    pub infohash: [u8; 20],
    /// The `dn` parameter, percent-decoded, if present.
    pub display_name: Option<String>,
}

/// Parses a magnet URI, accepting `xt=urn:btih:` in 40-character hex or
/// 32-character base32.
///
/// # Errors
///
/// Returns [`SourceError`] when the URI lacks the `magnet:?` scheme, has no
/// `xt=urn:btih:` topic, or that topic is not a valid hex or base32 hash.
///
/// ```
/// use mistarr_sources::magnet::parse_magnet;
/// let hex = "0".repeat(40);
/// let uri = format!("magnet:?xt=urn:btih:{hex}&dn=Example&tr=http%3A%2F%2Ftracker.invalid%2Fannounce");
/// let magnet = parse_magnet(&uri).unwrap();
/// assert_eq!(magnet.display_name.as_deref(), Some("Example"));
/// ```
pub fn parse_magnet(uri: &str) -> Result<Magnet, SourceError> {
    let query = uri
        .trim()
        .strip_prefix("magnet:?")
        .ok_or(SourceError::NotAMagnetUri)?;

    let mut infohash = None;
    let mut display_name = None;
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        match key {
            "xt" => {
                if let Some(topic) = value.strip_prefix("urn:btih:") {
                    infohash = Some(decode_btih(topic)?);
                }
            }
            "dn" => display_name = Some(percent_decode(value)),
            _ => {}
        }
    }

    let infohash = infohash.ok_or(SourceError::MissingMagnetTopic)?;
    Ok(Magnet {
        infohash,
        display_name,
    })
}

fn decode_btih(topic: &str) -> Result<[u8; 20], SourceError> {
    if topic.len() == 40 {
        decode_hex_20(topic)
    } else if topic.len() == 32 {
        decode_base32_20(topic)
    } else {
        Err(SourceError::InvalidMagnetHash)
    }
}

fn decode_hex_20(s: &str) -> Result<[u8; 20], SourceError> {
    let mut out = [0u8; 20];
    let bytes = s.as_bytes();
    for i in 0..20 {
        let hi = hex_val(bytes[i * 2])?;
        let lo = hex_val(bytes[i * 2 + 1])?;
        out[i] = (hi << 4) | lo;
    }
    Ok(out)
}

fn hex_val(b: u8) -> Result<u8, SourceError> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err(SourceError::InvalidMagnetHash),
    }
}

// RFC4648 base32, uppercase alphabet, no padding: 32 chars * 5 bits = 160 bits = 20 bytes.
fn decode_base32_20(s: &str) -> Result<[u8; 20], SourceError> {
    let mut out = [0u8; 20];
    let mut buffer: u64 = 0;
    let mut bits: u32 = 0;
    let mut out_pos = 0usize;
    for c in s.chars() {
        let val = base32_val(c)?;
        buffer = (buffer << 5) | u64::from(val);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            let byte = ((buffer >> bits) & 0xFF) as u8;
            *out.get_mut(out_pos).ok_or(SourceError::InvalidMagnetHash)? = byte;
            out_pos += 1;
        }
    }
    if out_pos != 20 {
        return Err(SourceError::InvalidMagnetHash);
    }
    Ok(out)
}

fn base32_val(c: char) -> Result<u8, SourceError> {
    match c.to_ascii_uppercase() {
        'A'..='Z' => Ok(c.to_ascii_uppercase() as u8 - b'A'),
        '2'..='7' => Ok(c as u8 - b'2' + 26),
        _ => Err(SourceError::InvalidMagnetHash),
    }
}

// The `dn` parameter is form-encoded by common torrent clients, so `+` is a
// space alongside `%20`.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Ok(hi), Ok(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_infohash() {
        let hex = "ab".repeat(20);
        let uri = format!("magnet:?xt=urn:btih:{hex}");
        let magnet = parse_magnet(&uri).unwrap();
        assert_eq!(magnet.infohash, [0xab; 20]);
        assert_eq!(magnet.display_name, None);
    }

    #[test]
    fn parses_base32_infohash() {
        // "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA" is 32 zero-bit quintets: 20 zero bytes.
        let uri = "magnet:?xt=urn:btih:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let magnet = parse_magnet(uri).unwrap();
        assert_eq!(magnet.infohash, [0u8; 20]);
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
        assert_eq!(parse_magnet(&uri).unwrap().infohash, [0x22; 20]);
    }

    #[test]
    fn plus_in_display_name_decodes_to_space() {
        let hex = "33".repeat(20);
        let uri = format!("magnet:?xt=urn:btih:{hex}&dn=My+Set+Name");
        assert_eq!(
            parse_magnet(&uri).unwrap().display_name.as_deref(),
            Some("My Set Name")
        );
    }

    #[test]
    fn rejects_non_magnet_and_bad_hash() {
        assert!(matches!(
            parse_magnet("http://example.invalid"),
            Err(SourceError::NotAMagnetUri)
        ));
        assert!(matches!(
            parse_magnet("magnet:?xt=urn:btih:zz"),
            Err(SourceError::InvalidMagnetHash)
        ));
        assert!(matches!(
            parse_magnet("magnet:?dn=only"),
            Err(SourceError::MissingMagnetTopic)
        ));
    }

    proptest::proptest! {
        #[test]
        fn never_panics(s in "\\PC*") {
            let _ = parse_magnet(&s);
        }
    }
}
