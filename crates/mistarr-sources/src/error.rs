//! The single error type for this crate. Every parser returns it instead of
//! panicking on malformed input.

/// Failure reasons a caller can act on: reject the file, ask the user, or
/// retry. No variant here is produced by a `panic!`, `unwrap` or `expect`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SourceError {
    /// The bencode grammar was violated at the given byte offset.
    #[error("malformed bencode at byte {0}")]
    MalformedBencode(usize),
    /// A bencode string declared a length over the decoder's limit.
    #[error("bencode string exceeds the {0} byte limit")]
    StringTooLarge(usize),
    /// A list or dict nested deeper than the decoder allows.
    #[error("bencode nesting exceeds the depth limit of {0}")]
    NestingTooDeep(u32),
    /// Extra bytes followed a complete top-level value.
    #[error("trailing data after the top-level bencode value")]
    TrailingData,
    /// The torrent's top-level dict has no `info` entry, or it is not a dict.
    #[error("torrent has no info dictionary")]
    MissingInfoDict,
    /// A required field was absent or had the wrong bencode type.
    #[error("torrent field `{0}` is missing or has the wrong type")]
    BadField(&'static str),
    /// The torrent describes only a `BitTorrent` v2 layout (`file tree`, no v1 `files`/`length`).
    #[error("v2-only torrents are not supported; a v1 or hybrid file list is required")]
    V2Only,
    /// The string did not start with the `magnet:?` scheme.
    #[error("not a magnet URI")]
    NotAMagnetUri,
    /// No `xt=urn:btih:...` parameter was present.
    #[error("magnet URI has no urn:btih topic")]
    MissingMagnetTopic,
    /// The `urn:btih:` value was not 40 hex characters or 32 base32 characters.
    #[error("magnet URI has an invalid infohash encoding")]
    InvalidMagnetHash,
    /// Moving or writing a file under the watched directory failed.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
