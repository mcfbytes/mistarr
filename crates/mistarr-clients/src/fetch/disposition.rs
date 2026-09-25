//! The file name of a `Content-Disposition` header (RFC 6266, with RFC 8187 for `filename*`).

use super::percent_decode_bytes;

/// The parameters of a header value after its disposition type, keys lowercased, quoted
/// strings unquoted with their `\` escapes resolved; `;` inside quotes is kept.
fn params(value: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut chars = value.chars().peekable();
    // The disposition type comes first and has no value.
    for c in chars.by_ref() {
        if c == ';' {
            break;
        }
    }
    loop {
        let mut key = String::new();
        for c in chars.by_ref() {
            if c == '=' || c == ';' {
                break;
            }
            key.push(c);
        }
        let key = key.trim().to_ascii_lowercase();
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let mut val = String::new();
        if chars.peek() == Some(&'"') {
            chars.next();
            while let Some(c) = chars.next() {
                match c {
                    '\\' => val.extend(chars.next()),
                    '"' => break,
                    c => val.push(c),
                }
            }
            for c in chars.by_ref() {
                if c == ';' {
                    break;
                }
            }
        } else {
            for c in chars.by_ref() {
                if c == ';' {
                    break;
                }
                val.push(c);
            }
            val.truncate(val.trim_end().len());
            val.drain(..val.len() - val.trim_start().len());
        }
        if !key.is_empty() {
            out.push((key, val));
        }
        if chars.peek().is_none() {
            return out;
        }
    }
}

/// An RFC 8187 extended value, `charset'language'percent-encoded`, in UTF-8 or ISO-8859-1.
fn extended(value: &str) -> Option<String> {
    let (charset, rest) = value.split_once('\'')?;
    let (_language, encoded) = rest.split_once('\'')?;
    let bytes = percent_decode_bytes(encoded);
    match charset.to_ascii_lowercase().as_str() {
        "utf-8" => String::from_utf8(bytes).ok(),
        "iso-8859-1" => Some(bytes.into_iter().map(char::from).collect()),
        _ => None,
    }
}

/// The file name a `Content-Disposition` header gives: `filename*` when it decodes,
/// else `filename`; `None` when neither names a file.
///
/// ```
/// use mistarr_clients::fetch::disposition_file_name;
/// assert_eq!(disposition_file_name(r#"attachment; filename="a; \"b\".dat""#).as_deref(), Some(r#"a; "b".dat"#));
/// assert_eq!(disposition_file_name("attachment; filename=p.zip; filename*=UTF-8''p%C3%A9.zip").as_deref(), Some("pé.zip"));
/// assert_eq!(disposition_file_name("inline"), None);
/// ```
#[must_use]
pub fn disposition_file_name(value: &str) -> Option<String> {
    let params = params(value);
    let find = |name: &str| params.iter().find(|(k, _)| k == name).map(|(_, v)| v);
    find("filename*")
        .and_then(|v| extended(v))
        .or_else(|| find("filename").cloned())
        .filter(|n| !n.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_read_as_the_rfcs_say() {
        let n = |v: &str| disposition_file_name(v);
        assert_eq!(n("attachment; filename=a.dat").as_deref(), Some("a.dat"));
        assert_eq!(
            n("attachment;filename = \"x y.zip\" ; size=3").as_deref(),
            Some("x y.zip")
        );
        assert_eq!(n("attachment; filename=\"\"").as_deref(), None);
        assert_eq!(
            n("attachment; FILENAME*=utf-8'en'x%20y.zip").as_deref(),
            Some("x y.zip")
        );
        assert_eq!(
            n("attachment; filename*=iso-8859-1''caf%E9.dat").as_deref(),
            Some("café.dat")
        );
        assert_eq!(
            n("attachment; filename*=koi8-r''x.dat; filename=fallback.dat").as_deref(),
            Some("fallback.dat")
        );
        assert_eq!(
            n("attachment; filename*=UTF-8''%FF.dat; filename=b.dat").as_deref(),
            Some("b.dat")
        );
        assert_eq!(
            n("attachment; filename=\"unterminated").as_deref(),
            Some("unterminated")
        );
    }

    proptest::proptest! {
        #[test]
        fn quoted_names_round_trip(name in "[ -~]{1,40}") {
            let quoted = name.replace('\\', "\\\\").replace('"', "\\\"");
            let header = format!("attachment; filename=\"{quoted}\"; creation-date=\"x;y\"");
            let expect = (!name.trim().is_empty()).then(|| name.clone());
            proptest::prop_assert_eq!(disposition_file_name(&header), expect);
        }

        #[test]
        fn extended_names_round_trip(name in "\\PC{1,20}") {
            let encoded = name.bytes().fold(String::new(), |mut out, b| {
                let _ = std::fmt::Write::write_fmt(&mut out, format_args!("%{b:02X}"));
                out
            });
            let header = format!("attachment; filename=\"plain.dat\"; filename*=UTF-8''{encoded}");
            let expect = if name.trim().is_empty() { Some("plain.dat".to_owned()) } else { Some(name.clone()) };
            proptest::prop_assert_eq!(disposition_file_name(&header), expect);
        }

        #[test]
        fn any_header_is_read_without_panicking(value in "\\PC{0,80}") {
            let _ = disposition_file_name(&value);
        }
    }
}
