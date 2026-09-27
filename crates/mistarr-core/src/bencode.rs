//! A small bencode reader and encoder. Rejects malformed input with
//! [`BencodeError`] and never panics; see `docs/ARCHITECTURE.md` "Source
//! import" for why torrents are parsed here rather than pulled from a crate.

use std::collections::BTreeMap;

/// Longest byte string the reader accepts, chosen to fit the board's RAM
/// budget from `docs/ARCHITECTURE.md`.
pub const MAX_STRING_LEN: usize = 8 * 1024 * 1024;

/// Deepest list/dict nesting the reader accepts.
pub const MAX_DEPTH: u32 = 32;

/// Why bencode was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum BencodeError {
    /// The bencode grammar was violated at the given byte offset.
    #[error("malformed bencode at byte {0}")]
    Malformed(usize),
    /// A string declared a length over [`MAX_STRING_LEN`].
    #[error("bencode string exceeds the {0} byte limit")]
    StringTooLarge(usize),
    /// A list or dict nested deeper than [`MAX_DEPTH`].
    #[error("bencode nesting exceeds the depth limit of {0}")]
    NestingTooDeep(u32),
    /// Extra bytes followed a complete top-level value.
    #[error("trailing data after the top-level bencode value")]
    TrailingData,
}

/// A value to encode. Dict keys are raw bytes, since bencode does not
/// require them to be valid UTF-8.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// A signed integer (`i<digits>e`).
    Int(i64),
    /// A byte string (`<len>:<bytes>`), not necessarily UTF-8 text.
    Bytes(Vec<u8>),
    /// An ordered list of values (`l...e`).
    List(Vec<Value>),
    /// A dict with keys sorted by byte order (`d...e`).
    Dict(BTreeMap<Vec<u8>, Value>),
}

fn byte_at(data: &[u8], pos: usize) -> Result<u8, BencodeError> {
    data.get(pos).copied().ok_or(BencodeError::Malformed(pos))
}

fn decode_int(data: &[u8], pos: &mut usize) -> Result<i64, BencodeError> {
    let start_err = *pos;
    *pos += 1; // consume 'i'
    let digits_start = *pos;
    while byte_at(data, *pos)? != b'e' {
        *pos += 1;
    }
    let digits = &data[digits_start..*pos];
    *pos += 1; // consume 'e'
    if !valid_int_digits(digits) {
        return Err(BencodeError::Malformed(start_err));
    }
    let text = std::str::from_utf8(digits).map_err(|_| BencodeError::Malformed(start_err))?;
    text.parse::<i64>()
        .map_err(|_| BencodeError::Malformed(start_err))
}

// Bencode ints reject leading zeros and "-0"; both signal a non-canonical encoder.
fn valid_int_digits(s: &[u8]) -> bool {
    let (neg, digits) = if let Some(rest) = s.strip_prefix(b"-") {
        (true, rest)
    } else {
        (false, s)
    };
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return false;
    }
    if digits.len() > 1 && digits[0] == b'0' {
        return false;
    }
    !(neg && digits == b"0")
}

/// Reads a string's `<len>:` prefix, leaving `*pos` at its first byte.
fn string_len(data: &[u8], pos: &mut usize) -> Result<usize, BencodeError> {
    let start = *pos;
    while byte_at(data, *pos)? != b':' {
        *pos += 1;
    }
    let len_digits = &data[start..*pos];
    *pos += 1; // consume ':'
               // Reject non-digit bytes (e.g. a leading '+') and redundant leading zeros.
    if len_digits.is_empty()
        || !len_digits.iter().all(u8::is_ascii_digit)
        || (len_digits.len() > 1 && len_digits[0] == b'0')
    {
        return Err(BencodeError::Malformed(start));
    }
    let len_text = std::str::from_utf8(len_digits).map_err(|_| BencodeError::Malformed(start))?;
    let len: usize = len_text
        .parse()
        .map_err(|_| BencodeError::Malformed(start))?;
    if len > MAX_STRING_LEN {
        return Err(BencodeError::StringTooLarge(MAX_STRING_LEN));
    }
    Ok(len)
}

/// A bencode value borrowed from its input. Lists and dicts keep their encoded bytes
/// and are walked on demand, so reading a large torrent allocates nothing per entry.
/// A `Raw` comes only from [`Raw::parse`], which checks the whole value first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Raw<'a> {
    /// A signed integer.
    Int(i64),
    /// A byte string.
    Bytes(&'a [u8]),
    /// A list, as its encoding `l...e`.
    List(&'a [u8]),
    /// A dict, as its encoding `d...e`.
    Dict(&'a [u8]),
}

impl<'a> Raw<'a> {
    /// Checks one complete value at the start of `data`, under [`MAX_STRING_LEN`] and
    /// [`MAX_DEPTH`], and returns it with its encoded length; bytes after it are the caller's.
    ///
    /// # Errors
    ///
    /// Returns [`BencodeError`] when `data` violates the bencode grammar, a
    /// string exceeds [`MAX_STRING_LEN`], or nesting exceeds [`MAX_DEPTH`].
    ///
    /// ```
    /// use mistarr_core::bencode::Raw;
    /// let (v, len) = Raw::parse(b"d1:ai1ee4:tail").unwrap();
    /// assert_eq!(len, 8);
    /// assert_eq!(v.get("a"), Some(Raw::Int(1)));
    /// assert!(Raw::parse(b"not bencode").is_err());
    /// ```
    pub fn parse(data: &'a [u8]) -> Result<(Self, usize), BencodeError> {
        let mut pos = 0;
        let value = raw_value(data, &mut pos, 0)?;
        Ok((value, pos))
    }

    /// The raw bytes, if this is a byte string.
    ///
    /// ```
    /// use mistarr_core::bencode::Raw;
    /// assert_eq!(Raw::parse(b"4:spam").unwrap().0.as_bytes(), Some(&b"spam"[..]));
    /// ```
    #[must_use]
    pub fn as_bytes(self) -> Option<&'a [u8]> {
        match self {
            Raw::Bytes(b) => Some(b),
            _ => None,
        }
    }

    /// The text, if this is a byte string of valid UTF-8.
    ///
    /// ```
    /// use mistarr_core::bencode::Raw;
    /// assert_eq!(Raw::parse(b"2:ok").unwrap().0.as_str(), Some("ok"));
    /// ```
    #[must_use]
    pub fn as_str(self) -> Option<&'a str> {
        self.as_bytes().and_then(|b| std::str::from_utf8(b).ok())
    }

    /// The integer, if this is one.
    ///
    /// ```
    /// use mistarr_core::bencode::Raw;
    /// assert_eq!(Raw::parse(b"i7e").unwrap().0.as_int(), Some(7));
    /// ```
    #[must_use]
    pub fn as_int(self) -> Option<i64> {
        match self {
            Raw::Int(v) => Some(v),
            _ => None,
        }
    }

    /// Each item of a list in order; nothing for any other value.
    ///
    /// ```
    /// use mistarr_core::bencode::Raw;
    /// let (list, _) = Raw::parse(b"li1ei2ee").unwrap();
    /// assert_eq!(list.items().filter_map(Raw::as_int).collect::<Vec<_>>(), [1, 2]);
    /// ```
    pub fn items(self) -> impl Iterator<Item = Raw<'a>> {
        let body = match self {
            Raw::List(b) => inner(b),
            _ => &[][..],
        };
        let mut pos = 0;
        std::iter::from_fn(move || {
            if pos >= body.len() {
                return None;
            }
            raw_value(body, &mut pos, 0).ok()
        })
    }

    /// Each `(key, value, encoded value)` of a dict in order; nothing for any other value.
    ///
    /// ```
    /// use mistarr_core::bencode::Raw;
    /// let (dict, _) = Raw::parse(b"d1:ai1e1:b2:xye").unwrap();
    /// let keys: Vec<&[u8]> = dict.entries().map(|(k, _, _)| k).collect();
    /// assert_eq!(keys, [b"a", b"b"]);
    /// ```
    pub fn entries(self) -> impl Iterator<Item = (&'a [u8], Raw<'a>, &'a [u8])> {
        let body = match self {
            Raw::Dict(b) => inner(b),
            _ => &[][..],
        };
        let mut pos = 0;
        std::iter::from_fn(move || {
            if pos >= body.len() {
                return None;
            }
            let key = raw_bytes(body, &mut pos).ok()?;
            let start = pos;
            let value = raw_value(body, &mut pos, 0).ok()?;
            Some((key, value, &body[start..pos]))
        })
    }

    /// The value under `key` in a dict, the first one when a key repeats, as clients read it.
    ///
    /// ```
    /// use mistarr_core::bencode::Raw;
    /// let (dict, _) = Raw::parse(b"d4:name2:ok4:name2:noe").unwrap();
    /// assert_eq!(dict.get("name").and_then(Raw::as_str), Some("ok"));
    /// assert_eq!(dict.get("absent"), None);
    /// ```
    #[must_use]
    pub fn get(self, key: &str) -> Option<Raw<'a>> {
        self.entries()
            .find(|(k, _, _)| *k == key.as_bytes())
            .map(|(_, v, _)| v)
    }
}

/// The body of an encoded list or dict without its `l`/`d` and `e`; empty for a `Raw`
/// built by hand from bytes too short to hold them.
fn inner(encoded: &[u8]) -> &[u8] {
    encoded
        .get(1..encoded.len().saturating_sub(1))
        .unwrap_or_default()
}

/// Reads one value at `*pos`, checking all of it, and leaves `*pos` just past it.
fn raw_value<'a>(data: &'a [u8], pos: &mut usize, depth: u32) -> Result<Raw<'a>, BencodeError> {
    if depth > MAX_DEPTH {
        return Err(BencodeError::NestingTooDeep(MAX_DEPTH));
    }
    let start = *pos;
    match byte_at(data, start)? {
        b'i' => decode_int(data, pos).map(Raw::Int),
        b'0'..=b'9' => raw_bytes(data, pos).map(Raw::Bytes),
        b'l' => {
            *pos += 1;
            while byte_at(data, *pos)? != b'e' {
                raw_value(data, pos, depth + 1)?;
            }
            *pos += 1;
            Ok(Raw::List(&data[start..*pos]))
        }
        b'd' => {
            *pos += 1;
            while byte_at(data, *pos)? != b'e' {
                raw_bytes(data, pos)?;
                raw_value(data, pos, depth + 1)?;
            }
            *pos += 1;
            Ok(Raw::Dict(&data[start..*pos]))
        }
        _ => Err(BencodeError::Malformed(start)),
    }
}

/// A byte string at `*pos`, borrowed.
fn raw_bytes<'a>(data: &'a [u8], pos: &mut usize) -> Result<&'a [u8], BencodeError> {
    let start = *pos;
    let len = string_len(data, pos)?;
    let end = pos
        .checked_add(len)
        .filter(|&e| e <= data.len())
        .ok_or(BencodeError::Malformed(start))?;
    let bytes = &data[*pos..end];
    *pos = end;
    Ok(bytes)
}

/// Encodes a value into canonical bencode (dict keys sorted by byte
/// order, which [`Value::Dict`] already enforces via `BTreeMap`).
///
/// ```
/// use mistarr_core::bencode::{encode, Value};
/// assert_eq!(encode(&Value::Bytes(b"spam".to_vec())), b"4:spam");
/// ```
#[must_use]
pub fn encode(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    encode_into(value, &mut out);
    out
}

fn encode_bytes(b: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(b.len().to_string().as_bytes());
    out.push(b':');
    out.extend_from_slice(b);
}

fn encode_into(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Int(v) => {
            out.push(b'i');
            out.extend_from_slice(v.to_string().as_bytes());
            out.push(b'e');
        }
        Value::Bytes(b) => encode_bytes(b, out),
        Value::List(items) => {
            out.push(b'l');
            for item in items {
                encode_into(item, out);
            }
            out.push(b'e');
        }
        Value::Dict(map) => {
            out.push(b'd');
            for (key, val) in map {
                encode_bytes(key, out);
                encode_into(val, out);
            }
            out.push(b'e');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one value `data` holds, refusing trailing bytes.
    fn whole(data: &[u8]) -> Result<Raw<'_>, BencodeError> {
        match Raw::parse(data)? {
            (raw, len) if len == data.len() => Ok(raw),
            _ => Err(BencodeError::TrailingData),
        }
    }

    /// The owned value a borrowed one stands for.
    fn owned(raw: Raw<'_>) -> Value {
        match raw {
            Raw::Int(v) => Value::Int(v),
            Raw::Bytes(b) => Value::Bytes(b.to_vec()),
            Raw::List(_) => Value::List(raw.items().map(owned).collect()),
            Raw::Dict(_) => Value::Dict(
                raw.entries()
                    .map(|(k, v, _)| (k.to_vec(), owned(v)))
                    .collect(),
            ),
        }
    }

    #[test]
    fn reads_int() {
        assert_eq!(whole(b"i42e"), Ok(Raw::Int(42)));
        assert_eq!(whole(b"i-42e"), Ok(Raw::Int(-42)));
        assert_eq!(whole(b"i0e"), Ok(Raw::Int(0)));
        let mut pos = 0;
        assert_eq!(decode_int(b"i-7e", &mut pos), Ok(-7));
        assert_eq!(pos, 4);
    }

    #[test]
    fn rejects_malformed_int() {
        for bad in [
            &b"i042e"[..],
            b"i-0e",
            b"ie",
            b"i4",
            b"i99999999999999999999e",
        ] {
            assert!(whole(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn reads_bytes() {
        assert_eq!(whole(b"4:spam"), Ok(Raw::Bytes(b"spam")));
        assert_eq!(whole(b"0:"), Ok(Raw::Bytes(b"")));
        assert_eq!(Raw::Int(1).as_bytes(), None);
        assert_eq!(Raw::Bytes(b"\xff").as_str(), None);
    }

    #[test]
    fn rejects_plus_sign_in_string_length() {
        assert!(whole(b"1+:x").is_err());
    }

    #[test]
    fn rejects_oversized_and_truncated_strings() {
        assert!(matches!(
            whole(b"999999999999:x"),
            Err(BencodeError::Malformed(_) | BencodeError::StringTooLarge(_))
        ));
        assert_eq!(
            whole(b"9999999:x"),
            Err(BencodeError::StringTooLarge(MAX_STRING_LEN))
        );
        assert!(whole(b"4:sp").is_err());
    }

    #[test]
    fn reads_list_and_dict() {
        assert_eq!(
            owned(whole(b"l4:spam4:eggse").unwrap()),
            Value::List(vec![
                Value::Bytes(b"spam".to_vec()),
                Value::Bytes(b"eggs".to_vec())
            ])
        );
        let d = whole(b"d3:cow3:moo4:spam4:eggse").unwrap();
        assert_eq!(d.get("cow").and_then(Raw::as_str), Some("moo"));
        assert_eq!(d.get("spam").and_then(Raw::as_str), Some("eggs"));
    }

    #[test]
    fn leaves_trailing_data_to_the_caller() {
        assert_eq!(Raw::parse(b"i1ei2e"), Ok((Raw::Int(1), 3)));
        assert_eq!(whole(b"i1ei2e"), Err(BencodeError::TrailingData));
    }

    #[test]
    fn rejects_excessive_nesting() {
        let mut data = vec![b'l'; MAX_DEPTH as usize + 2];
        data.extend(std::iter::repeat_n(b'e', MAX_DEPTH as usize + 2));
        assert_eq!(
            Raw::parse(&data),
            Err(BencodeError::NestingTooDeep(MAX_DEPTH))
        );
    }

    #[test]
    fn encode_round_trips() {
        let data = b"d3:cow3:moo4:spam4:eggse";
        assert_eq!(encode(&owned(whole(data).unwrap())), data);
    }

    #[test]
    fn raw_walks_nested_values_in_place() {
        let data = b"d4:infod5:filesld6:lengthi3e4:pathl1:a1:beee4:name1:xe1:zi-2ee";
        let (top, len) = Raw::parse(data).unwrap();
        assert_eq!(len, data.len());
        let (_, info, bytes) = top.entries().next().unwrap();
        assert_eq!(bytes, &data[7..data.len() - 8]);
        let file = info.get("files").unwrap().items().next().unwrap();
        assert_eq!(file.get("length").and_then(Raw::as_int), Some(3));
        let path: Vec<&str> = file
            .get("path")
            .unwrap()
            .items()
            .filter_map(Raw::as_str)
            .collect();
        assert_eq!(path, ["a", "b"]);
        assert_eq!(top.get("z"), Some(Raw::Int(-2)));
        assert_eq!(Raw::Int(1).items().count(), 0);
        assert_eq!(Raw::Bytes(b"x").entries().count(), 0);
        for hand_built in [&b""[..], b"l", b"le", b"lxe", b"li1"] {
            assert_eq!(Raw::List(hand_built).items().count(), 0);
            assert_eq!(Raw::Dict(hand_built).entries().count(), 0);
        }
        assert!(matches!(
            Raw::parse(b"l1:a"),
            Err(BencodeError::Malformed(_))
        ));
        assert!(matches!(
            Raw::parse(b"5:ab"),
            Err(BencodeError::Malformed(_))
        ));
    }

    proptest::proptest! {
        #[test]
        fn never_panics(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..256)) {
            if let Ok((raw, len)) = Raw::parse(&bytes) {
                proptest::prop_assert!(len <= bytes.len());
                let _ = owned(raw);
            }
        }

        #[test]
        fn a_parsed_canonical_value_encodes_to_its_bytes(
            bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..256),
        ) {
            if let Ok((raw, len)) = Raw::parse(&bytes) {
                let again = encode(&owned(raw));
                let (back, back_len) = Raw::parse(&again).unwrap();
                proptest::prop_assert_eq!(back_len, again.len());
                proptest::prop_assert_eq!(owned(back), owned(raw));
                proptest::prop_assert!(len > 0);
            }
        }

        #[test]
        fn raw_reads_what_encode_writes(
            words in proptest::collection::vec("[a-z]{0,6}", 0..8),
            n in proptest::prelude::any::<i64>(),
        ) {
            let list = Value::List(words.iter().map(|w| Value::Bytes(w.clone().into_bytes())).collect());
            let dict = Value::Dict([(b"l".to_vec(), list), (b"n".to_vec(), Value::Int(n))].into());
            let data = encode(&dict);
            let (raw, len) = Raw::parse(&data).unwrap();
            proptest::prop_assert_eq!(len, data.len());
            proptest::prop_assert_eq!(owned(raw), dict);
        }
    }
}
