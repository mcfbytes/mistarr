//! A small bencode decoder and encoder. Rejects malformed input with
//! [`SourceError`] and never panics; see `docs/ARCHITECTURE.md` "Source
//! import" for why torrents are parsed here rather than pulled from a crate.

use std::collections::BTreeMap;

use crate::error::SourceError;

/// Longest byte string the decoder accepts, chosen to fit the board's RAM
/// budget from `docs/ARCHITECTURE.md`.
pub const MAX_STRING_LEN: usize = 8 * 1024 * 1024;

/// Deepest list/dict nesting the decoder accepts.
pub const MAX_DEPTH: u32 = 32;

/// A decoded bencode value. Dict keys are raw bytes, since bencode does not
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

impl Value {
    /// The integer, if this value is [`Value::Int`].
    ///
    /// ```
    /// use mistarr_sources::bencode::decode;
    /// assert_eq!(decode(b"i42e").unwrap().as_int(), Some(42));
    /// ```
    #[must_use]
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(v) => Some(*v),
            _ => None,
        }
    }

    /// The raw bytes, if this value is [`Value::Bytes`].
    ///
    /// ```
    /// use mistarr_sources::bencode::decode;
    /// assert_eq!(decode(b"4:spam").unwrap().as_bytes(), Some(&b"spam"[..]));
    /// ```
    #[must_use]
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Value::Bytes(b) => Some(b),
            _ => None,
        }
    }

    /// The bytes as UTF-8 text, if this value is [`Value::Bytes`] and valid.
    ///
    /// ```
    /// use mistarr_sources::bencode::decode;
    /// assert_eq!(decode(b"4:spam").unwrap().as_str(), Some("spam"));
    /// ```
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        self.as_bytes().and_then(|b| std::str::from_utf8(b).ok())
    }

    /// The list items, if this value is [`Value::List`].
    ///
    /// ```
    /// use mistarr_sources::bencode::decode;
    /// assert_eq!(decode(b"le").unwrap().as_list(), Some(&[][..]));
    /// ```
    #[must_use]
    pub fn as_list(&self) -> Option<&[Value]> {
        match self {
            Value::List(items) => Some(items),
            _ => None,
        }
    }

    /// The dict entries, if this value is [`Value::Dict`].
    ///
    /// ```
    /// use mistarr_sources::bencode::decode;
    /// assert!(decode(b"de").unwrap().as_dict().unwrap().is_empty());
    /// ```
    #[must_use]
    pub fn as_dict(&self) -> Option<&BTreeMap<Vec<u8>, Value>> {
        match self {
            Value::Dict(d) => Some(d),
            _ => None,
        }
    }

    /// A dict entry by key name, or `None` if this is not a dict or the key
    /// is absent.
    ///
    /// ```
    /// use mistarr_sources::bencode::decode;
    /// let v = decode(b"d4:name4:spame").unwrap();
    /// assert_eq!(v.get("name").and_then(|n| n.as_str()), Some("spam"));
    /// ```
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_dict()?.get(key.as_bytes())
    }
}

/// Decodes a single bencode value, rejecting trailing bytes after it.
///
/// # Errors
///
/// Returns [`SourceError`] when `data` violates the bencode grammar, a
/// string exceeds [`MAX_STRING_LEN`], or nesting exceeds [`MAX_DEPTH`].
///
/// ```
/// use mistarr_sources::bencode::decode;
/// assert!(decode(b"i-3e").is_ok());
/// assert!(decode(b"not bencode").is_err());
/// ```
pub fn decode(data: &[u8]) -> Result<Value, SourceError> {
    let mut pos = 0usize;
    let value = decode_value(data, &mut pos, 0)?;
    if pos != data.len() {
        return Err(SourceError::TrailingData);
    }
    Ok(value)
}

fn byte_at(data: &[u8], pos: usize) -> Result<u8, SourceError> {
    data.get(pos)
        .copied()
        .ok_or(SourceError::MalformedBencode(pos))
}

fn decode_value(data: &[u8], pos: &mut usize, depth: u32) -> Result<Value, SourceError> {
    if depth > MAX_DEPTH {
        return Err(SourceError::NestingTooDeep(MAX_DEPTH));
    }
    match byte_at(data, *pos)? {
        b'i' => decode_int(data, pos),
        b'l' => decode_list(data, pos, depth),
        b'd' => decode_dict(data, pos, depth),
        b'0'..=b'9' => decode_bytes(data, pos).map(Value::Bytes),
        _ => Err(SourceError::MalformedBencode(*pos)),
    }
}

fn decode_int(data: &[u8], pos: &mut usize) -> Result<Value, SourceError> {
    let start_err = *pos;
    *pos += 1; // consume 'i'
    let digits_start = *pos;
    while byte_at(data, *pos)? != b'e' {
        *pos += 1;
    }
    let digits = &data[digits_start..*pos];
    *pos += 1; // consume 'e'
    if !valid_int_digits(digits) {
        return Err(SourceError::MalformedBencode(start_err));
    }
    let text = std::str::from_utf8(digits).map_err(|_| SourceError::MalformedBencode(start_err))?;
    let value = text
        .parse::<i64>()
        .map_err(|_| SourceError::MalformedBencode(start_err))?;
    Ok(Value::Int(value))
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

fn decode_bytes(data: &[u8], pos: &mut usize) -> Result<Vec<u8>, SourceError> {
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
        return Err(SourceError::MalformedBencode(start));
    }
    let len_text =
        std::str::from_utf8(len_digits).map_err(|_| SourceError::MalformedBencode(start))?;
    let len: usize = len_text
        .parse()
        .map_err(|_| SourceError::MalformedBencode(start))?;
    if len > MAX_STRING_LEN {
        return Err(SourceError::StringTooLarge(MAX_STRING_LEN));
    }
    let end = pos
        .checked_add(len)
        .ok_or(SourceError::MalformedBencode(start))?;
    if end > data.len() {
        return Err(SourceError::MalformedBencode(start));
    }
    let bytes = data[*pos..end].to_vec();
    *pos = end;
    Ok(bytes)
}

fn decode_list(data: &[u8], pos: &mut usize, depth: u32) -> Result<Value, SourceError> {
    *pos += 1; // consume 'l'
    let mut items = Vec::new();
    loop {
        if byte_at(data, *pos)? == b'e' {
            *pos += 1;
            return Ok(Value::List(items));
        }
        items.push(decode_value(data, pos, depth + 1)?);
    }
}

/// One entry of a top-level dict: its key, value, and the exact byte range
/// in the input the value came from.
pub(crate) type TopLevelEntry = (Vec<u8>, Value, std::ops::Range<usize>);

/// Decodes a top-level dict, returning each entry's key, value, and the
/// exact byte range in `data` the value came from (before any re-encoding).
/// Used to hash a torrent's `info` dict over its original bytes.
pub(crate) fn decode_top_level_dict(data: &[u8]) -> Result<Vec<TopLevelEntry>, SourceError> {
    let mut pos = 0usize;
    if byte_at(data, pos)? != b'd' {
        return Err(SourceError::MalformedBencode(pos));
    }
    pos += 1;
    let mut entries = Vec::new();
    loop {
        if byte_at(data, pos)? == b'e' {
            pos += 1;
            break;
        }
        let key = decode_bytes(data, &mut pos)?;
        let value_start = pos;
        let value = decode_value(data, &mut pos, 0)?;
        entries.push((key, value, value_start..pos));
    }
    if pos != data.len() {
        return Err(SourceError::TrailingData);
    }
    Ok(entries)
}

fn decode_dict(data: &[u8], pos: &mut usize, depth: u32) -> Result<Value, SourceError> {
    *pos += 1; // consume 'd'
    let mut map = BTreeMap::new();
    loop {
        if byte_at(data, *pos)? == b'e' {
            *pos += 1;
            return Ok(Value::Dict(map));
        }
        let key = decode_bytes(data, pos)?;
        let value = decode_value(data, pos, depth + 1)?;
        map.insert(key, value);
    }
}

/// Encodes a value back into canonical bencode (dict keys sorted by byte
/// order, which [`Value::Dict`] already enforces via `BTreeMap`).
///
/// ```
/// use mistarr_sources::bencode::{decode, encode};
/// let v = decode(b"4:spam").unwrap();
/// assert_eq!(encode(&v), b"4:spam");
/// ```
#[must_use]
pub fn encode(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    encode_into(value, &mut out);
    out
}

fn encode_into(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Int(v) => {
            out.push(b'i');
            out.extend_from_slice(v.to_string().as_bytes());
            out.push(b'e');
        }
        Value::Bytes(b) => {
            out.extend_from_slice(b.len().to_string().as_bytes());
            out.push(b':');
            out.extend_from_slice(b);
        }
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
                encode_into(&Value::Bytes(key.clone()), out);
                encode_into(val, out);
            }
            out.push(b'e');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_int() {
        assert_eq!(decode(b"i42e").unwrap(), Value::Int(42));
        assert_eq!(decode(b"i-42e").unwrap(), Value::Int(-42));
        assert_eq!(decode(b"i0e").unwrap(), Value::Int(0));
    }

    #[test]
    fn rejects_malformed_int() {
        assert!(decode(b"i042e").is_err());
        assert!(decode(b"i-0e").is_err());
        assert!(decode(b"ie").is_err());
        assert!(decode(b"i4").is_err());
    }

    #[test]
    fn decodes_bytes() {
        assert_eq!(decode(b"4:spam").unwrap(), Value::Bytes(b"spam".to_vec()));
        assert_eq!(decode(b"0:").unwrap(), Value::Bytes(Vec::new()));
    }

    #[test]
    fn rejects_plus_sign_in_string_length() {
        assert!(decode(b"1+:x").is_err());
    }

    #[test]
    fn rejects_oversized_and_truncated_strings() {
        assert!(matches!(
            decode(b"999999999999:x"),
            Err(SourceError::MalformedBencode(_) | SourceError::StringTooLarge(_))
        ));
        assert!(decode(b"4:sp").is_err());
    }

    #[test]
    fn decodes_list_and_dict() {
        assert_eq!(
            decode(b"l4:spam4:eggse").unwrap(),
            Value::List(vec![
                Value::Bytes(b"spam".to_vec()),
                Value::Bytes(b"eggs".to_vec())
            ])
        );
        let d = decode(b"d3:cow3:moo4:spam4:eggse").unwrap();
        assert_eq!(d.get("cow").unwrap().as_str(), Some("moo"));
        assert_eq!(d.get("spam").unwrap().as_str(), Some("eggs"));
    }

    #[test]
    fn rejects_trailing_data() {
        assert!(matches!(decode(b"i1ei2e"), Err(SourceError::TrailingData)));
    }

    #[test]
    fn rejects_excessive_nesting() {
        let mut data = vec![b'l'; MAX_DEPTH as usize + 2];
        data.extend(std::iter::repeat(b'e').take(MAX_DEPTH as usize + 2));
        assert!(matches!(decode(&data), Err(SourceError::NestingTooDeep(_))));
    }

    #[test]
    fn encode_round_trips() {
        let v = decode(b"d3:cow3:moo4:spam4:eggse").unwrap();
        assert_eq!(encode(&v), b"d3:cow3:moo4:spam4:eggse");
    }

    proptest::proptest! {
        #[test]
        fn never_panics(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..256)) {
            let _ = decode(&bytes);
        }
    }
}
