//! Domain types and pure logic: DAT parsing, name parsing, hashing with
//! header rules, matching and 1G1R selection.
//!
//! This crate is synchronous, does no network I/O and owns no files. See
//! `docs/VERIFICATION.md` for the contracts implemented here and
//! `docs/WORKPLAN.md` WP-01 to WP-03 for the packages that fill it in.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

/// 1G1R selection and clone-group inference.
pub mod select;

/// One-pass hashing and platform header rules.
pub mod hash;

/// Stable platform identifier, e.g. `nes`, `megadrive`, `psx`.
/// The full table lives in `docs/PLATFORMS.md` and in `mistarr-mister`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct PlatformId(pub String);

/// The three hashes and size that identify a dump. Hex fields are lowercase.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HashSet {
    /// File size in bytes after any header rule was applied.
    pub size: u64,
    /// CRC32 as 8 lowercase hex characters.
    pub crc32: String,
    /// MD5 as 32 lowercase hex characters.
    pub md5: String,
    /// SHA1 as 40 lowercase hex characters.
    pub sha1: String,
}
