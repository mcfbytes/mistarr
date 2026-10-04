//! The single error type for this crate. Every parser returns it instead of
//! panicking on malformed input.

use mistarr_core::bencode::BencodeError;

/// Failure reasons a caller can act on: reject the file, ask the user, or
/// retry. No variant here is produced by a `panic!`, `unwrap` or `expect`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SourceError {
    /// The file is not valid bencode.
    #[error(transparent)]
    Bencode(#[from] BencodeError),
    /// The torrent's top-level dict has no `info` entry, or it is not a dict.
    #[error("torrent has no info dictionary")]
    MissingInfoDict,
    /// A required field was absent or had the wrong bencode type.
    #[error("torrent field `{0}` is missing or has the wrong type")]
    BadField(&'static str),
    /// The torrent describes only a `BitTorrent` v2 layout (`file tree`, no v1 `files`/`length`).
    #[error("v2-only torrents are not supported; a v1 or hybrid file list is required")]
    V2Only,
}
