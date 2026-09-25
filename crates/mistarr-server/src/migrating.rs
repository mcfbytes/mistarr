//! The progress file a start writes while it migrates the database; see `docs/DEPLOYMENT.md` "Upgrading".

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use crate::error::Result;

/// The file's name in the data directory; `scripts/install.sh` reads it.
pub const FILE: &str = "mistarr.migrating";

/// How often the file is rewritten.
pub const PERIOD: Duration = Duration::from_secs(5);

/// SQLite virtual machine instructions between two counts of a [`Steps`] counter.
pub const STEP_OPS: i32 = 10_000;

/// Name of the thread that rewrites it.
const THREAD: &str = "db-migrate";

/// Counts the migrating connection's progress, bumped every [`STEP_OPS`] SQLite
/// instructions by the handler `db::Db::open_counting` installs; only the migration moves it.
pub type Steps = Arc<AtomicU64>;

/// Keeps `<data>/mistarr.migrating` current until dropped, then removes it. Each line
/// names the versions, the [`Steps`] count and the WAL's size, which grows while a commit
/// writes its pages, so it changes only while the migration moves.
#[derive(Debug)]
pub struct Migrating {
    stop: Option<Sender<()>>,
    thread: Option<JoinHandle<()>>,
    path: PathBuf,
    steps: Steps,
}

impl Migrating {
    /// Writes the file in `data` for the database `db` now and every [`PERIOD`] from a
    /// thread of its own.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Io`] when the file cannot be written or the thread not started.
    ///
    /// ```
    /// let dir = tempfile::tempdir().unwrap();
    /// let db = dir.path().join("mistarr.db");
    /// let m = mistarr_server::migrating::Migrating::begin(dir.path(), &db, 16, 17).unwrap();
    /// let path = dir.path().join(mistarr_server::migrating::FILE);
    /// let line = std::fs::read_to_string(&path).unwrap();
    /// assert_eq!(line, "migrating from 16 to 17: steps 0 wal 0\n");
    /// drop(m);
    /// assert!(!path.exists());
    /// ```
    pub fn begin(data: &Path, db: &Path, from: u32, to: u32) -> Result<Self> {
        Self::begin_every(data, db, (from, to), PERIOD)
    }

    fn begin_every(
        data: &Path,
        db: &Path,
        (from, to): (u32, u32),
        period: Duration,
    ) -> Result<Self> {
        let path = data.join(FILE);
        let steps = Steps::default();
        let count = Arc::clone(&steps);
        let mut wal = db.as_os_str().to_owned();
        wal.push("-wal");
        let wal = PathBuf::from(wal);
        let line = move || {
            let n = count.load(Ordering::Relaxed);
            let size = std::fs::metadata(&wal).map_or(0, |m| m.len());
            format!("migrating from {from} to {to}: steps {n} wal {size}\n")
        };
        std::fs::write(&path, line())?;
        let (stop, stopped) = mpsc::channel::<()>();
        let target = path.clone();
        let thread = std::thread::Builder::new()
            .name(THREAD.into())
            .spawn(move || {
                while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(period) {
                    if let Err(e) = std::fs::write(&target, line()) {
                        tracing::warn!(error = %e, "cannot update the migration progress file");
                    }
                }
            })?;
        Ok(Self {
            stop: Some(stop),
            thread: Some(thread),
            path,
            steps,
        })
    }

    /// The counter the migrating connection bumps; the file reports it.
    #[must_use]
    pub fn steps(&self) -> Steps {
        Arc::clone(&self.steps)
    }
}

impl Drop for Migrating {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        if let Err(e) = std::fs::remove_file(&self.path) {
            tracing::warn!(error = %e, "cannot remove the migration progress file");
        }
    }
}

/// Removes a progress file a stopped start left in `data`.
///
/// # Errors
///
/// [`crate::Error::Io`] when it exists and cannot be removed.
///
/// ```
/// let dir = tempfile::tempdir().unwrap();
/// std::fs::write(dir.path().join(mistarr_server::migrating::FILE), "x").unwrap();
/// mistarr_server::migrating::clear_stale(dir.path()).unwrap();
/// assert!(!dir.path().join(mistarr_server::migrating::FILE).exists());
/// ```
pub fn clear_stale(data: &Path) -> Result<()> {
    match std::fs::remove_file(data.join(FILE)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TICK: Duration = Duration::from_millis(50);

    /// The file's lines over the next `ticks` periods, one read per period.
    fn lines(path: &Path, ticks: usize) -> Vec<String> {
        (0..ticks)
            .map(|_| {
                std::thread::sleep(TICK * 2);
                std::fs::read_to_string(path).expect("read")
            })
            .collect()
    }

    #[test]
    fn an_idle_migration_writes_the_same_line_on_every_tick() {
        let dir = tempfile::tempdir().expect("tempdir");
        let m = Migrating::begin_every(dir.path(), &dir.path().join("m.db"), (3, 4), TICK)
            .expect("begin");
        let path = dir.path().join(FILE);
        let seen = lines(&path, 3);
        assert!(
            seen.iter()
                .all(|l| l == "migrating from 3 to 4: steps 0 wal 0\n"),
            "{seen:?}"
        );
        drop(m);
        assert!(!path.exists());
    }

    #[test]
    fn a_migration_running_statements_changes_the_line() {
        let dir = tempfile::tempdir().expect("tempdir");
        let m = Migrating::begin_every(dir.path(), &dir.path().join("m.db"), (3, 4), TICK)
            .expect("begin");
        let conn = rusqlite::Connection::open_in_memory().expect("open");
        crate::db::count_steps(&conn, &m.steps()).expect("handler");
        let rows: i64 = conn
            .query_row(
                "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 100000)
                 SELECT count(*) FROM n",
                [],
                |r| r.get(0),
            )
            .expect("busy");
        assert_eq!(rows, 100_000);
        let seen = lines(&dir.path().join(FILE), 1);
        assert_ne!(seen[0], "migrating from 3 to 4: steps 0 wal 0\n");
    }
}
