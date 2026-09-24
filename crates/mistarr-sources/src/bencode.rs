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
    raw_bytes(data, pos).map(<[u8]>::to_vec)
}

/// Reads a string's `<len>:` prefix, leaving `*pos` at its first byte.
fn string_len(data: &[u8], pos: &mut usize) -> Result<usize, SourceError> {
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
    Ok(len)
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
    /// Checks one complete value at the start of `data` under the limits of [`decode`] and
    /// returns it with its encoded length; bytes after it are the caller's.
    ///
    /// # Errors
    ///
    /// As [`decode`], except that trailing bytes are not an error.
    ///
    /// ```
    /// use mistarr_sources::bencode::Raw;
    /// let (v, len) = Raw::parse(b"d1:ai1ee4:tail").unwrap();
    /// assert_eq!(len, 8);
    /// assert_eq!(v.get("a"), Some(Raw::Int(1)));
    /// ```
    pub fn parse(data: &'a [u8]) -> Result<(Self, usize), SourceError> {
        let mut pos = 0;
        let value = raw_value(data, &mut pos, 0)?;
        Ok((value, pos))
    }

    /// The text, if this is a byte string of valid UTF-8.
    ///
    /// ```
    /// use mistarr_sources::bencode::Raw;
    /// assert_eq!(Raw::parse(b"2:ok").unwrap().0.as_str(), Some("ok"));
    /// ```
    #[must_use]
    pub fn as_str(self) -> Option<&'a str> {
        match self {
            Raw::Bytes(b) => std::str::from_utf8(b).ok(),
            _ => None,
        }
    }

    /// The integer, if this is one.
    ///
    /// ```
    /// use mistarr_sources::bencode::Raw;
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
    /// use mistarr_sources::bencode::Raw;
    /// let (list, _) = Raw::parse(b"li1ei2ee").unwrap();
    /// assert_eq!(list.items().filter_map(Raw::as_int).collect::<Vec<_>>(), [1, 2]);
    /// ```
    pub fn items(self) -> impl Iterator<Item = Raw<'a>> {
        let body = match self {
            Raw::List(b) => &b[1..b.len() - 1],
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
    /// use mistarr_sources::bencode::Raw;
    /// let (dict, _) = Raw::parse(b"d1:ai1e1:b2:xye").unwrap();
    /// let keys: Vec<&[u8]> = dict.entries().map(|(k, _, _)| k).collect();
    /// assert_eq!(keys, [b"a", b"b"]);
    /// ```
    pub fn entries(self) -> impl Iterator<Item = (&'a [u8], Raw<'a>, &'a [u8])> {
        let body = match self {
            Raw::Dict(b) => &b[1..b.len() - 1],
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

    /// The value under `key` in a dict, the last one when a key repeats.
    ///
    /// ```
    /// use mistarr_sources::bencode::Raw;
    /// let (dict, _) = Raw::parse(b"d4:name2:oke").unwrap();
    /// assert_eq!(dict.get("name").and_then(Raw::as_str), Some("ok"));
    /// assert_eq!(dict.get("absent"), None);
    /// ```
    #[must_use]
    pub fn get(self, key: &str) -> Option<Raw<'a>> {
        self.entries()
            .filter(|(k, _, _)| *k == key.as_bytes())
            .map(|(_, v, _)| v)
            .last()
    }
}

/// Reads one value at `*pos`, checking all of it, and leaves `*pos` just past it.
fn raw_value<'a>(data: &'a [u8], pos: &mut usize, depth: u32) -> Result<Raw<'a>, SourceError> {
    if depth > MAX_DEPTH {
        return Err(SourceError::NestingTooDeep(MAX_DEPTH));
    }
    let start = *pos;
    match byte_at(data, start)? {
        b'i' => match decode_int(data, pos)? {
            Value::Int(v) => Ok(Raw::Int(v)),
            _ => Err(SourceError::MalformedBencode(start)),
        },
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
        _ => Err(SourceError::MalformedBencode(start)),
    }
}

/// A byte string at `*pos`, borrowed.
fn raw_bytes<'a>(data: &'a [u8], pos: &mut usize) -> Result<&'a [u8], SourceError> {
    let start = *pos;
    let len = string_len(data, pos)?;
    let end = pos
        .checked_add(len)
        .filter(|&e| e <= data.len())
        .ok_or(SourceError::MalformedBencode(start))?;
    let bytes = &data[*pos..end];
    *pos = end;
    Ok(bytes)
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
        data.extend(std::iter::repeat_n(b'e', MAX_DEPTH as usize + 2));
        assert!(matches!(decode(&data), Err(SourceError::NestingTooDeep(_))));
    }

    #[test]
    fn encode_round_trips() {
        let v = decode(b"d3:cow3:moo4:spam4:eggse").unwrap();
        assert_eq!(encode(&v), b"d3:cow3:moo4:spam4:eggse");
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
        assert!(matches!(
            Raw::parse(b"l1:a"),
            Err(SourceError::MalformedBencode(_))
        ));
        assert!(matches!(
            Raw::parse(b"5:ab"),
            Err(SourceError::MalformedBencode(_))
        ));
    }

    proptest::proptest! {
        #[test]
        fn never_panics(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..256)) {
            let _ = decode(&bytes);
            let _ = Raw::parse(&bytes);
        }

        #[test]
        fn raw_agrees_with_decode(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..256)) {
            let whole = Raw::parse(&bytes).ok().filter(|(_, len)| *len == bytes.len());
            match (decode(&bytes), whole) {
                (Ok(v), Some((raw, _))) => proptest::prop_assert_eq!(owned(raw), v),
                (Err(_), None) => {}
                (d, r) => proptest::prop_assert!(false, "decode {:?}, raw {:?}", d, r),
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
