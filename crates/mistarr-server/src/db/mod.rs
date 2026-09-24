//! SQLite access; the schema is `docs/DATA-MODEL.md`. All SQL lives in this module's children.

pub mod dats;
pub mod jobs;
pub mod migrate;
pub mod platforms;
pub mod settings;
pub mod system;
pub mod titles;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::Connection;

use crate::error::{Error, Result};

/// Page cache per connection in KiB; two connections share the 2 MiB budget.
const CACHE_KIB: i64 = 1024;

/// How long a statement waits on a lock held by the other connection.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// The database: one writer and one reader connection, each behind a mutex.
/// WAL lets the reader run while the writer holds a transaction.
#[derive(Clone)]
pub struct Db {
    inner: Arc<Inner>,
}

struct Inner {
    writer: Mutex<Connection>,
    reader: Mutex<Connection>,
    path: PathBuf,
}

impl Db {
    /// Opens or creates the database at `path` and applies pending migrations.
    ///
    /// # Errors
    ///
    /// [`Error::Db`] when the file cannot be opened or configured, and
    /// [`Error::Migration`] when a migration fails.
    ///
    /// ```
    /// let dir = std::env::temp_dir().join("mistarr-doc-db-open");
    /// std::fs::create_dir_all(&dir).unwrap();
    /// let db = mistarr_server::db::Db::open(&dir.join("t.db")).unwrap();
    /// assert!(db.path().ends_with("t.db"));
    /// ```
    pub fn open(path: &Path) -> Result<Self> {
        let mut writer = Connection::open(path)?;
        configure(&writer)?;
        migrate::apply(&mut writer)?;
        let reader = Connection::open(path)?;
        configure(&reader)?;
        reader.pragma_update(None, "query_only", true)?;
        Ok(Self {
            inner: Arc::new(Inner {
                writer: Mutex::new(writer),
                reader: Mutex::new(reader),
                path: path.to_path_buf(),
            }),
        })
    }

    /// The database file.
    ///
    /// ```
    /// # let dir = std::env::temp_dir().join("mistarr-doc-db-path");
    /// # std::fs::create_dir_all(&dir).unwrap();
    /// let db = mistarr_server::db::Db::open(&dir.join("p.db")).unwrap();
    /// assert!(db.path().is_file());
    /// ```
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// Runs `f` on the writer connection on the calling thread.
    ///
    /// # Errors
    ///
    /// Whatever `f` returns, or [`Error::Poisoned`].
    ///
    /// ```
    /// # let dir = std::env::temp_dir().join("mistarr-doc-db-wb");
    /// # std::fs::create_dir_all(&dir).unwrap();
    /// # let db = mistarr_server::db::Db::open(&dir.join("w.db")).unwrap();
    /// use mistarr_server::db::settings;
    /// db.write_blocking(|c| settings::set(c, "k", "v")).unwrap();
    /// ```
    pub fn write_blocking<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        let mut conn = self.inner.writer.lock().map_err(|_| Error::Poisoned)?;
        f(&mut conn)
    }

    /// Runs `f` on the read-only connection on the calling thread.
    ///
    /// # Errors
    ///
    /// Whatever `f` returns, or [`Error::Poisoned`].
    ///
    /// ```
    /// # let dir = std::env::temp_dir().join("mistarr-doc-db-rb");
    /// # std::fs::create_dir_all(&dir).unwrap();
    /// # let db = mistarr_server::db::Db::open(&dir.join("r.db")).unwrap();
    /// let n = db.read_blocking(mistarr_server::db::migrate::current_version).unwrap();
    /// assert!(n >= 1);
    /// ```
    pub fn read_blocking<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let conn = self.inner.reader.lock().map_err(|_| Error::Poisoned)?;
        f(&conn)
    }

    /// [`Db::write_blocking`] on tokio's blocking pool.
    ///
    /// # Errors
    ///
    /// Whatever `f` returns, [`Error::Poisoned`] or [`Error::Task`].
    pub async fn write<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        let db = self.clone();
        tokio::task::spawn_blocking(move || db.write_blocking(f))
            .await
            .map_err(|e| Error::Task(e.to_string()))?
    }

    /// [`Db::read_blocking`] on tokio's blocking pool.
    ///
    /// # Errors
    ///
    /// Whatever `f` returns, [`Error::Poisoned`] or [`Error::Task`].
    pub async fn read<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
    {
        let db = self.clone();
        tokio::task::spawn_blocking(move || db.read_blocking(f))
            .await
            .map_err(|e| Error::Task(e.to_string()))?
    }
}

/// Applies the connection pragmas every connection shares.
fn configure(conn: &Connection) -> Result<()> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    let mode: String =
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        tracing::warn!(mode, "database is not in WAL mode");
    }
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", true)?;
    conn.pragma_update(None, "cache_size", -CACHE_KIB)?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::Db;

    /// A fresh migrated database in a temporary directory kept alive by the guard.
    pub fn db() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Db::open(&dir.path().join("test.db")).expect("open");
        (dir, db)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connections_use_wal_and_the_cache_budget() {
        let (_dir, db) = testutil::db();
        let (mode, cache): (String, i64) = db
            .read_blocking(|c| {
                let mode = c.pragma_query_value(None, "journal_mode", |r| r.get(0))?;
                let cache = c.pragma_query_value(None, "cache_size", |r| r.get(0))?;
                Ok((mode, cache))
            })
            .expect("pragmas");
        assert_eq!(mode, "wal");
        assert_eq!(cache, -CACHE_KIB);
    }

    #[test]
    fn reader_cannot_write() {
        let (_dir, db) = testutil::db();
        let r = db.read_blocking(|c| Ok(settings::set(c, "k", "v")));
        assert!(r.expect("lock").is_err());
    }

    #[tokio::test]
    async fn async_wrappers_reach_both_connections() {
        let (_dir, db) = testutil::db();
        db.write(|c| settings::set(c, "a", "1"))
            .await
            .expect("write");
        let v = db.read(|c| settings::get(c, "a")).await.expect("read");
        assert_eq!(v.as_deref(), Some("1"));
        assert!(db.path().is_file());
    }
}
