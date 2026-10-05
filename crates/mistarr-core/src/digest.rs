//! Fixed-length digests that read and write as lowercase hex.

use std::fmt;
use std::str::FromStr;

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

/// `N` digest bytes. Displays, serializes and parses as `2 * N` hex digits,
/// lowercase when written; parsing accepts either case.
///
/// ```
/// use mistarr_core::Sha1;
/// let d = Sha1::from_bytes([0xab; 20]);
/// assert_eq!(&d.to_string()[..4], "abab");
/// assert_eq!(d.to_string().parse::<Sha1>(), Ok(d));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Digest<const N: usize>([u8; N]);

/// A CRC32, its four bytes big-endian as DATs write it.
pub type Crc32 = Digest<4>;
/// An MD5 digest.
pub type Md5 = Digest<16>;
/// A SHA1 digest.
pub type Sha1 = Digest<20>;

impl<const N: usize> Digest<N> {
    /// Wraps raw digest bytes.
    ///
    /// ```
    /// assert_eq!(mistarr_core::Md5::from_bytes([1; 16]).as_bytes(), &[1; 16]);
    /// ```
    #[must_use]
    pub const fn from_bytes(bytes: [u8; N]) -> Self {
        Self(bytes)
    }

    /// The raw digest bytes.
    ///
    /// ```
    /// assert_eq!(mistarr_core::Crc32::from_bytes([0, 0, 0, 7]).as_bytes()[3], 7);
    /// ```
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; N] {
        &self.0
    }
}

impl<const N: usize> From<[u8; N]> for Digest<N> {
    fn from(bytes: [u8; N]) -> Self {
        Self(bytes)
    }
}

impl Digest<4> {
    /// A CRC32 from its value.
    ///
    /// ```
    /// assert_eq!(mistarr_core::Crc32::from_u32(0x0102_0304).to_string(), "01020304");
    /// ```
    #[must_use]
    pub const fn from_u32(value: u32) -> Self {
        Self(value.to_be_bytes())
    }

    /// The CRC32's value.
    ///
    /// ```
    /// assert_eq!(mistarr_core::Crc32::from_bytes([0, 0, 1, 2]).to_u32(), 0x0102);
    /// ```
    #[must_use]
    pub const fn to_u32(self) -> u32 {
        u32::from_be_bytes(self.0)
    }
}

impl<const N: usize> fmt::Display for Digest<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        crate::hex::write(f, &self.0)
    }
}

/// Text that is not a digest of the expected length in hex.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("expected {digits} hex digits")]
pub struct ParseDigestError {
    /// Hex digits the digest has.
    pub digits: usize,
}

impl<const N: usize> FromStr for Digest<N> {
    type Err = ParseDigestError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut bytes = [0; N];
        crate::hex::decode_into(s, &mut bytes).ok_or(ParseDigestError { digits: 2 * N })?;
        Ok(Self(bytes))
    }
}

impl<const N: usize> Serialize for Digest<N> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de, const N: usize> Deserialize<'de> for Digest<N> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_str(HexVisitor)
    }
}

/// Parses the hex text in place, so reading a digest allocates nothing.
struct HexVisitor<const N: usize>;

impl<const N: usize> de::Visitor<'_> for HexVisitor<N> {
    type Value = Digest<N>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} hex digits", 2 * N)
    }

    fn visit_str<E: de::Error>(self, s: &str) -> Result<Self::Value, E> {
        s.parse().map_err(E::custom)
    }
}

/// Stored as lowercase hex text; a column that is not that hex fails to read.
#[cfg(feature = "rusqlite")]
impl<const N: usize> rusqlite::types::ToSql for Digest<N> {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(self.to_string().into())
    }
}

#[cfg(feature = "rusqlite")]
impl<const N: usize> rusqlite::types::FromSql for Digest<N> {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        value
            .as_str()?
            .parse()
            .map_err(|e| rusqlite::types::FromSqlError::Other(Box::new(e)))
    }
}

/// A `BitTorrent` v1 infohash: the SHA1 of a torrent's encoded `info` dict. Reads and
/// writes as 40 hex digits like [`Digest`].
///
/// ```
/// use mistarr_core::InfoHash;
/// let h = InfoHash::from_bytes([0x0a; 20]);
/// assert_eq!("0A".repeat(20).parse::<InfoHash>(), Ok(h));
/// assert_eq!(h.to_string(), "0a".repeat(20));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InfoHash(Digest<20>);

impl InfoHash {
    /// Wraps raw hash bytes.
    ///
    /// ```
    /// assert_eq!(mistarr_core::InfoHash::from_bytes([1; 20]).as_bytes(), &[1; 20]);
    /// ```
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 20]) -> Self {
        Self(Digest(bytes))
    }

    /// The raw hash bytes.
    ///
    /// ```
    /// assert_eq!(mistarr_core::InfoHash::from_bytes([2; 20]).as_bytes()[0], 2);
    /// ```
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 20] {
        self.0.as_bytes()
    }
}

impl fmt::Display for InfoHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for InfoHash {
    type Err = ParseDigestError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse().map(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn crc32_reads_as_dats_write_it() {
        let crc: Crc32 = "352441C2".parse().expect("crc");
        assert_eq!(crc.as_bytes(), &0x3524_41c2_u32.to_be_bytes());
        assert_eq!(crc.to_string(), "352441c2");
        assert_eq!(Crc32::from([1, 2, 3, 4]).to_string(), "01020304");
    }

    #[test]
    fn wrong_lengths_and_digits_are_refused() {
        for bad in ["", "00", &"0".repeat(41), &"g".repeat(40)] {
            assert_eq!(bad.parse::<Sha1>(), Err(ParseDigestError { digits: 40 }));
            assert!(bad.parse::<InfoHash>().is_err());
        }
        assert_eq!(
            ParseDigestError { digits: 8 }.to_string(),
            "expected 8 hex digits"
        );
    }

    #[test]
    fn serde_uses_lowercase_hex() {
        let md5 = Md5::from_bytes([0xcd; 16]);
        let json = serde_json::to_string(&md5).expect("json");
        assert_eq!(json, format!("\"{}\"", "cd".repeat(16)));
        assert_eq!(serde_json::from_str::<Md5>(&json).expect("md5"), md5);
        let read = serde_json::from_reader::<_, Md5>(json.as_bytes()).expect("read");
        assert_eq!(read, md5);
        assert!(serde_json::from_str::<Md5>("7").is_err());
        let h = InfoHash::from_bytes([0xef; 20]);
        let json = serde_json::to_string(&h).expect("json");
        assert_eq!(json, format!("\"{}\"", "ef".repeat(20)));
        assert_eq!(
            serde_json::from_str::<InfoHash>(&json.to_uppercase()).expect("h"),
            h
        );
        assert!(serde_json::from_str::<Md5>("\"00\"").is_err());
    }

    #[cfg(feature = "rusqlite")]
    #[test]
    fn sql_stores_lowercase_hex_and_refuses_anything_else() {
        let c = rusqlite::Connection::open_in_memory().expect("open");
        let d = Sha1::from_bytes([0xab; 20]);
        let text: String = c.query_row("SELECT ?1", [d], |r| r.get(0)).expect("text");
        assert_eq!(text, "ab".repeat(20));
        let back: Sha1 = c
            .query_row("SELECT upper(?1)", [d], |r| r.get(0))
            .expect("back");
        assert_eq!(back, d);
        assert!(c
            .query_row("SELECT 'zz'", [], |r| r.get::<_, Md5>(0))
            .is_err());
        assert!(c.query_row("SELECT 7", [], |r| r.get::<_, Md5>(0)).is_err());
    }

    proptest! {
        #[test]
        fn display_and_parse_round_trip(bytes in any::<[u8; 20]>()) {
            let d = Sha1::from_bytes(bytes);
            prop_assert_eq!(d.to_string().parse::<Sha1>(), Ok(d));
            let h = InfoHash::from_bytes(bytes);
            prop_assert_eq!(h.to_string(), d.to_string());
            prop_assert_eq!(h.to_string().to_uppercase().parse::<InfoHash>(), Ok(h));
        }

        #[test]
        fn parse_agrees_with_hex_decode(text in "[0-9a-fA-Fg é]{0,10}|\\PC*") {
            let parsed = text.parse::<Crc32>().ok().map(|d| d.as_bytes().to_vec());
            prop_assert_eq!(parsed, crate::hex::decode(&text).filter(|b| b.len() == 4));
        }
    }
}
