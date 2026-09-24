//! Errors returned by download client operations.

/// A failed client operation, one variant per thing the caller can act on.
///
/// ```
/// use mistarr_clients::ClientError;
/// let e = ClientError::Protocol("duplicate torrent".into());
/// assert_eq!(e.to_string(), "download client protocol error: duplicate torrent");
/// ```
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ClientError {
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
    /// The torrent came from a magnet and the client has no metadata yet.
    #[error("the download client has no metadata for this torrent yet")]
    MetadataPending,
    /// A local file or socket operation failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::ClientError;

    #[test]
    fn messages_name_the_failure() {
        let e = ClientError::FileIndex {
            index: 5,
            file_count: 2,
        };
        assert_eq!(
            e.to_string(),
            "file index 5 is out of range for a torrent of 2 files"
        );
        let io = ClientError::from(std::io::Error::other("disk"));
        assert!(matches!(io, ClientError::Io(_)));
    }
}
