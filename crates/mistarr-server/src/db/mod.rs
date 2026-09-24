//! SQLite access; the schema is `docs/DATA-MODEL.md`. All SQL lives in this module's children.

pub mod arcade;
pub mod candidates;
pub mod dat_stage;
pub mod dats;
pub mod downloads;
pub mod downloads_import;
pub mod files;
pub mod imports;
pub mod jobs;
pub mod launch;
pub mod migrate;
pub mod platforms;
pub mod settings;
pub mod sources;
pub mod system;
pub mod titles;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::Connection;

use crate::error::{Error, Result};

/// Page cache per connection in KiB; two connections share the 2 MiB budget.
const CACHE_KIB: i64 = 1024;

/// WAL pages written before an automatic checkpoint, about 1 MiB of 4 KiB pages.
const WAL_AUTOCHECKPOINT: i64 = 256;

/// Size the WAL file is cut back to after a checkpoint, in bytes.
const JOURNAL_SIZE_LIMIT: i64 = 1024 * 1024;

/// Heap SQLite tries to stay under, process-wide, by shedding cached pages.
const SOFT_HEAP_LIMIT: i64 = 8 * 1024 * 1024;

/// How long a statement waits on a lock held by the other connection.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// The database: one writer and one reader connection, each behind a mutex.
/// WAL lets the reader run while the writer holds a transaction.
#[derive(Clone)]
pub struct Db {
    inner: Arc<Inner>,
}

struct Inner {
    /// One permit, taken before a write reaches the blocking pool, so writers waiting
    /// their turn hold no blocking thread and reads keep running.
    write_turn: Arc<tokio::sync::Semaphore>,
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
                write_turn: Arc::new(tokio::sync::Semaphore::new(1)),
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

    /// [`Db::write_blocking`] on tokio's blocking pool, once no other async write is running.
    ///
    /// # Errors
    ///
    /// Whatever `f` returns, [`Error::Poisoned`] or [`Error::Task`].
    pub async fn write<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        let turn = Arc::clone(&self.inner.write_turn)
            .acquire_owned()
            .await
            .map_err(|_| Error::Poisoned)?;
        let db = self.clone();
        tokio::task::spawn_blocking(move || {
            let _turn = turn;
            db.write_blocking(f)
        })
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

/// The environment variable SQLite reads for its temporary file directory.
pub const SQLITE_TMPDIR: &str = "SQLITE_TMPDIR";

/// Creates `dir` for SQLite's temporary files and removes the files a previous run left
/// there; the caller then points [`SQLITE_TMPDIR`] at it before any connection opens.
///
/// # Errors
///
/// [`Error::Io`] when `dir` cannot be created or listed.
///
/// ```
/// let dir = std::env::temp_dir().join("mistarr-doc-sqlite-tmp");
/// mistarr_server::db::prepare_temp_dir(&dir).unwrap();
/// assert!(dir.is_dir());
/// ```
pub fn prepare_temp_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    for entry in std::fs::read_dir(dir)?.flatten() {
        if entry.file_type().is_ok_and(|t| t.is_file()) {
            if let Err(e) = std::fs::remove_file(entry.path()) {
                tracing::warn!(error = %e, "cannot remove a stale SQLite temporary file");
            }
        }
    }
    Ok(())
}

/// Applies the connection pragmas every connection shares; the memory-related ones are
/// listed in `docs/ARCHITECTURE.md` "Resource budgets".
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
    conn.pragma_update(None, "mmap_size", 0)?;
    conn.pragma_update(None, "temp_store", "FILE")?;
    conn.pragma_update(None, "wal_autocheckpoint", WAL_AUTOCHECKPOINT)?;
    conn.pragma_update_and_check(None, "journal_size_limit", JOURNAL_SIZE_LIMIT, |_| Ok(()))?;
    conn.pragma_update_and_check(None, "soft_heap_limit", SOFT_HEAP_LIMIT, |_| Ok(()))?;
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
    fn connections_keep_memory_and_the_wal_small() {
        let (_dir, db) = testutil::db();
        for read in [true, false] {
            let pragma = |c: &Connection, name: &str| -> Result<i64> {
                Ok(c.pragma_query_value(None, name, |r| r.get(0))?)
            };
            let values = |c: &Connection| -> Result<[i64; 5]> {
                Ok([
                    pragma(c, "mmap_size")?,
                    pragma(c, "temp_store")?,
                    pragma(c, "wal_autocheckpoint")?,
                    pragma(c, "journal_size_limit")?,
                    pragma(c, "soft_heap_limit")?,
                ])
            };
            let got = if read {
                db.read_blocking(values)
            } else {
                db.write_blocking(|c| values(c))
            }
            .expect("pragmas");
            assert_eq!(
                got,
                [
                    0,
                    1,
                    WAL_AUTOCHECKPOINT,
                    JOURNAL_SIZE_LIMIT,
                    SOFT_HEAP_LIMIT
                ]
            );
        }
    }

    #[test]
    fn the_temp_dir_is_created_and_emptied_of_stale_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tmp = dir.path().join("data/tmp");
        prepare_temp_dir(&tmp).expect("create");
        std::fs::write(tmp.join("etilqs_stale"), b"x").expect("write");
        std::fs::create_dir(tmp.join("keep")).expect("mkdir");
        prepare_temp_dir(&tmp).expect("clear");
        let left: Vec<_> = std::fs::read_dir(&tmp)
            .expect("list")
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(left, ["keep"]);
    }

    #[test]
    fn reader_cannot_write() {
        let (_dir, db) = testutil::db();
        let r = db.read_blocking(|c| Ok(settings::set(c, "k", "v")));
        assert!(r.expect("lock").is_err());
    }

    #[test]
    fn reads_run_while_writers_queue_behind_a_long_write() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(2)
            .enable_time()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let (_dir, db) = testutil::db();
            let (held, holding) = std::sync::mpsc::channel();
            let (release, released) = std::sync::mpsc::channel::<()>();
            let long = tokio::spawn({
                let db = db.clone();
                async move {
                    db.write(move |_| {
                        held.send(()).ok();
                        released.recv().ok();
                        Ok(())
                    })
                    .await
                }
            });
            while holding.try_recv().is_err() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let queued: Vec<_> = (0..4)
                .map(|i| {
                    let db = db.clone();
                    tokio::spawn(async move {
                        db.write(move |c| settings::set(c, "k", &i.to_string()))
                            .await
                    })
                })
                .collect();
            tokio::time::sleep(Duration::from_millis(20)).await;
            let read =
                tokio::time::timeout(Duration::from_secs(5), db.read(migrate::current_version))
                    .await;
            assert!(
                read.is_ok_and(|r| r.is_ok()),
                "a read waited on queued writers"
            );
            release.send(()).expect("release");
            long.await.expect("join").expect("long write");
            for q in queued {
                q.await.expect("join").expect("queued write");
            }
        });
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
