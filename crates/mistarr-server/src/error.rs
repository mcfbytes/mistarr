//! The server's error type.

/// Failures inside the server that a caller can act on.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The config file is missing or unreadable.
    #[error("config: {}: {source}", path.display())]
    ConfigRead {
        /// The config file.
        path: std::path::PathBuf,
        /// Why it could not be read.
        source: std::io::Error,
    },
    /// The config file is not valid TOML or a value has the wrong type.
    #[error("config: {}: {source}", path.display())]
    Config {
        /// The config file.
        path: std::path::PathBuf,
        /// Where and why parsing failed.
        source: toml::de::Error,
    },
    /// A database call failed.
    #[error("database: {0}")]
    Db(rusqlite::Error),
    /// A migration failed; the database is left at the previous version.
    #[error("migration {version} failed: {source}")]
    Migration {
        /// The migration number.
        version: u32,
        /// The SQLite error.
        source: rusqlite::Error,
    },
    /// The database was migrated by a newer mistarr; its contents are left unchanged.
    #[error(
        "database schema version {found} is newer than this mistarr supports ({supported}); \
         install a newer mistarr, or restore the mistarr.db.prev set with the previous version \
         when mistarr.prev.ok marks it complete"
    )]
    SchemaTooNew {
        /// The highest migration recorded in the database.
        found: u32,
        /// The highest migration this binary embeds.
        supported: u32,
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
    /// A blocking or spawned task was cancelled or panicked.
    #[error("background task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
    /// A value could not be written or read as JSON.
    #[error("JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// The download client refused or failed a request.
    #[error(transparent)]
    Client(#[from] mistarr_clients::Error),
    /// A URL fetch failed; the message never names the URL.
    #[error(transparent)]
    Fetched(#[from] mistarr_clients::fetch::FetchError),
    /// A source file could not be read, parsed or moved.
    #[error(transparent)]
    Source(#[from] mistarr_sources::Error),
    /// Main refused a command or the command interface failed.
    #[error(transparent)]
    Mister(#[from] mistarr_mister::Error),
    /// The server is shutting down; the job stopped at a checkpoint.
    #[error("cancelled by shutdown")]
    Cancelled,
    /// The user cancelled the job; see [`crate::jobs::Scheduler::cancel`].
    #[error("Cancelled.")]
    CancelledByUser,
    /// A pause held the job's lane while it held the writer; it let go to wait.
    #[error("stopped for a pause")]
    Paused,
    /// A job failed.
    #[error("job failed: {0}")]
    Job(String),
    /// A URL fetch failed or its file was refused; the message is the user's to read
    /// and never names the URL.
    #[error("{0}")]
    FetchRefused(String),
    /// The configured CA bundle cannot be read, so a URL fetch is refused.
    #[error("The CA bundle cannot be read: {0}.")]
    CaFile(std::io::Error),
    /// A settings change was refused; the effective settings are unchanged.
    #[error("{}", .0.message())]
    Settings(crate::config::ConfigProblem),
    /// Memory, the RAM directory or the card ran short; the message says which and what
    /// was left unchanged. An import in RAM falls back to the card on it.
    #[error("{0}")]
    NoRoom(String),
    /// The database file was replaced but its connections could not be reopened; every
    /// statement fails until mistarr restarts.
    #[error("cannot reopen the database; restart mistarr: {0}")]
    Reopen(Box<Error>),
    /// Another server holds the data directory's lock.
    #[error("another mistarr is already running with data directory {}", .0.display())]
    AlreadyRunning(std::path::PathBuf),
    /// A file was not placed in a watched directory.
    #[error(transparent)]
    Place(#[from] crate::incoming::place::PlaceError),
    /// A named file or directory could not be read, written, created or removed.
    #[error("{}: {source}", path.display())]
    File {
        /// The file or directory involved.
        path: std::path::PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// An operating system call with no file of its own failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// A benchmark command refused its database file.
    #[error("bench: {0}")]
    Bench(String),
}

impl From<rusqlite::Error> for Error {
    /// [`Error::Stored`] for a column [`crate::db::sql::StoredJson`] refused, else [`Error::Db`].
    fn from(e: rusqlite::Error) -> Self {
        let rusqlite::Error::FromSqlConversionFailure(i, ty, inner) = e else {
            return Self::Db(e);
        };
        match inner.downcast::<crate::db::sql::StoredJson>() {
            Ok(bad) => Self::Stored {
                key: bad.key.to_owned(),
                source: bad.source,
            },
            Err(inner) => Self::Db(rusqlite::Error::FromSqlConversionFailure(i, ty, inner)),
        }
    }
}

impl Error {
    /// Wraps an I/O error as [`Error::File`] naming `path`.
    ///
    /// ```
    /// use std::path::Path;
    /// let e = mistarr_server::Error::io_at(Path::new("/d/x"))(std::io::Error::other("no"));
    /// assert_eq!(e.to_string(), "/d/x: no");
    /// ```
    pub fn io_at(path: &std::path::Path) -> impl FnOnce(std::io::Error) -> Self + '_ {
        move |source| Self::File {
            path: path.to_path_buf(),
            source,
        }
    }

    /// The I/O error inside [`Error::Io`] or [`Error::File`].
    ///
    /// ```
    /// let e = mistarr_server::Error::from(std::io::Error::other("no"));
    /// assert_eq!(e.io().map(std::io::Error::kind), Some(std::io::ErrorKind::Other));
    /// ```
    #[must_use]
    pub fn io(&self) -> Option<&std::io::Error> {
        match self {
            Self::Io(source) | Self::File { source, .. } => Some(source),
            _ => None,
        }
    }
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
        let json = serde_json::from_str::<u8>("x").map_err(Error::from);
        assert!(json.is_err_and(|e| e.to_string().starts_with("JSON: ")));
        let client = Error::from(mistarr_clients::Error::NotFound);
        assert_eq!(
            client.to_string(),
            "torrent not found in the download client"
        );
        let config = Error::Config {
            path: "/d/mistarr.toml".into(),
            source: toml::from_str::<toml::Table>("[[[").expect_err("invalid"),
        };
        assert!(config.to_string().starts_with("config: /d/mistarr.toml: "));
        assert_eq!(config.to_string().matches("config:").count(), 1);
        let ca = Error::CaFile(std::io::Error::other("no certificate"));
        assert_eq!(
            ca.to_string(),
            "The CA bundle cannot be read: no certificate."
        );
        let source = serde_json::from_str::<u8>("x").expect_err("not JSON");
        let stored = rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(crate::db::sql::StoredJson { key: "t.c", source }),
        );
        assert!(matches!(Error::from(stored), Error::Stored { key, .. } if key == "t.c"));
        let other = rusqlite::Error::InvalidQuery;
        assert!(matches!(Error::from(other), Error::Db(_)));
        let file = Error::io_at("/d/a.db".as_ref())(std::io::Error::other("gone"));
        assert_eq!(file.to_string(), "/d/a.db: gone");
        assert_eq!(file.io().map(ToString::to_string).as_deref(), Some("gone"));
        assert!(Error::Cancelled.io().is_none());
        let reopen = Error::Reopen(Box::new(Error::NoRoom("full".into())));
        assert_eq!(
            reopen.to_string(),
            "cannot reopen the database; restart mistarr: full"
        );
    }
}
