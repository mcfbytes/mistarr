//! The server's error type.

/// Failures inside the server that a caller can act on.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The config file is missing, unreadable or invalid.
    #[error("config: {0}")]
    Config(String),
    /// A database call failed.
    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
    /// A migration failed; the database is left at the previous version.
    #[error("migration {version} failed: {source}")]
    Migration {
        /// The migration number.
        version: u32,
        /// The SQLite error.
        source: rusqlite::Error,
    },
    /// A stored value is not the JSON its reader expects.
    #[error("stored value `{key}` is invalid: {source}")]
    Stored {
        /// Settings key or column that held the value.
        key: String,
        /// Why it did not parse.
        source: serde_json::Error,
    },
    /// A thread holding a database connection panicked.
    #[error("database connection lock poisoned")]
    Poisoned,
    /// A blocking task was cancelled or panicked.
    #[error("background task failed: {0}")]
    Task(String),
    /// A job kind has no registered implementation.
    #[error("unknown job kind `{0}`")]
    UnknownJob(String),
    /// The server is shutting down; the job stopped at a checkpoint.
    #[error("cancelled by shutdown")]
    Cancelled,
    /// A job failed.
    #[error("job failed: {0}")]
    Job(String),
    /// File system access failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Result alias for this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_name_the_failure() {
        let e = Error::Migration {
            version: 3,
            source: rusqlite::Error::InvalidQuery,
        };
        assert!(e.to_string().starts_with("migration 3 failed"));
        assert_eq!(
            Error::UnknownJob("x".into()).to_string(),
            "unknown job kind `x`"
        );
    }
}
