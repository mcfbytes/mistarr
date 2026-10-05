//! Errors returned by download client operations.

/// A failed client operation, one variant per thing the caller can act on.
///
/// ```
/// use mistarr_clients::Error;
/// let e = Error::Protocol("duplicate torrent".into());
/// assert_eq!(e.to_string(), "download client protocol error: duplicate torrent");
/// ```
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// No connection, refused, reset or timed out. Retry later and mark the
    /// client unreachable after repeated failures.
    #[error("download client unreachable: {0}")]
    Unreachable(String),
    /// The client wants credentials, or rejected the ones configured.
    #[error("download client rejected the credentials")]
    Auth,
    /// The client answered with something other than the protocol promises,
    /// or reported a failure for the request.
    #[error("download client protocol error: {0}")]
    Protocol(String),
    /// The client does not have the torrent.
    #[error("torrent not found in the download client")]
    NotFound,
    /// A wanted file index is past the end of the torrent's file list.
    #[error("file index {index} is out of range for a torrent of {file_count} files")]
    FileIndex {
        /// The offending index.
        index: u32,
        /// How many files the torrent has.
        file_count: usize,
    },
    /// An installed client could not be started, or is not installed.
    #[error("cannot start the download client: {0}")]
    Launch(String),
    /// The torrent came from a magnet and the client has no metadata yet.
    #[error("the download client has no metadata for this torrent yet")]
    MetadataPending,
    /// A local file or directory could not be read or written.
    #[error("{}: {source}", path.display())]
    Io {
        /// The file or directory involved.
        path: std::path::PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
}

impl Error {
    /// Wraps an I/O error with the path it concerns.
    ///
    /// ```
    /// use std::path::Path;
    /// let e = mistarr_clients::Error::io_at(Path::new("a.rc"))(std::io::Error::other("no"));
    /// assert_eq!(e.to_string(), "a.rc: no");
    /// ```
    pub fn io_at(path: &std::path::Path) -> impl FnOnce(std::io::Error) -> Self + '_ {
        move |source| Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }

    /// A [`Error::Protocol`] carrying `what` as its text.
    ///
    /// ```
    /// use mistarr_clients::Error;
    /// let e = Error::protocol(format_args!("status {}", 7));
    /// assert!(matches!(e, Error::Protocol(ref m) if m == "status 7"));
    /// ```
    pub fn protocol(what: impl std::fmt::Display) -> Self {
        Self::Protocol(what.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::Error;

    #[test]
    fn messages_name_the_failure() {
        let e = Error::FileIndex {
            index: 5,
            file_count: 2,
        };
        assert_eq!(
            e.to_string(),
            "file index 5 is out of range for a torrent of 2 files"
        );
        let io = Error::io_at("/d".as_ref())(std::io::Error::other("disk"));
        assert!(matches!(io, Error::Io { ref path, .. } if path.as_os_str() == "/d"));
        assert_eq!(io.to_string(), "/d: disk");
        let p = Error::protocol("bad reply");
        assert_eq!(p.to_string(), "download client protocol error: bad reply");
    }
}
