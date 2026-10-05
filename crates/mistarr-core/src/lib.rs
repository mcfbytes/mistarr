//! Domain types and pure logic: DAT parsing, name parsing, hashing with
//! header rules, matching and 1G1R selection, and the codecs every crate
//! shares: hex, digests, bencode, magnet links and percent-decoding.
//!
//! This crate is synchronous, does no network I/O and owns no files. See
//! `docs/VERIFICATION.md` for the contracts implemented here and
//! `docs/WORKPLAN.md` WP-01 to WP-03 for the packages that fill it in.

pub mod bencode;
/// CHD v5 CD images: header, track layout and track decoding.
pub mod chd;
pub mod dat;
mod digest;
mod error;
pub mod hex;
mod id;
pub mod magnet;
pub mod matching;
pub mod naming;
mod percent;
pub mod xml;

pub use digest::{Crc32, Digest, InfoHash, Md5, ParseDigestError, Sha1};
pub use error::{Error, Result};
pub use id::{PlatformId, RomId};
pub use percent::percent_decode;

#[cfg(feature = "rusqlite")]
#[doc(hidden)]
pub use rusqlite as __rusqlite;

/// 1G1R selection and clone-group inference.
pub mod select;

/// One-pass hashing and platform header rules.
pub mod hash;

/// The three hashes and size that identify a dump; serde writes each digest as
/// lowercase hex.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Hashes {
    /// File size in bytes after any header rule was applied.
    pub size: u64,
    /// CRC32 of the content.
    pub crc32: Crc32,
    /// MD5 of the content.
    pub md5: Md5,
    /// SHA1 of the content.
    pub sha1: Sha1,
}
