//! The single error type for this crate. Every parser returns it instead of
//! panicking on malformed input.

use std::io;
use std::path::{Path, PathBuf};

use mistarr_core::bencode::BencodeError;

/// Failure reasons a caller can act on: reject the file, ask the user, or
/// retry. No variant here is produced by a `panic!`, `unwrap` or `expect`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
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
    /// A dropped file or its directory could not be read, created or moved.
    #[error("{}: {source}", path.display())]
    Io {
        /// The file or directory involved.
        path: PathBuf,
        /// The underlying error.
        source: io::Error,
    },
}

impl Error {
    /// Wraps an I/O error with the path it concerns.
    ///
    /// ```
    /// use std::path::Path;
    /// let e = mistarr_sources::Error::io_at(Path::new("a.dat"))(std::io::Error::other("no"));
    /// assert_eq!(e.to_string(), "a.dat: no");
    /// ```
    pub fn io_at(path: &Path) -> impl FnOnce(io::Error) -> Self + '_ {
        move |source| Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

/// Result alias for this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;
