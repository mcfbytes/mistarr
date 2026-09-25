//! The progress file a start writes while it migrates the database; see `docs/DEPLOYMENT.md` "Upgrading".

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::error::Result;

/// The file's name in the data directory; `scripts/install.sh` reads it.
pub const FILE: &str = "mistarr.migrating";

/// How often the file is rewritten.
pub const PERIOD: Duration = Duration::from_secs(5);

/// Name of the thread that rewrites it.
const THREAD: &str = "db-migrate";

/// Keeps `<data>/mistarr.migrating` current until dropped, then removes it. Each line
/// names the versions and counts the process's I/O bytes and CPU ticks, so a reader
/// tells a migration that moves from one that stalled.
#[derive(Debug)]
pub struct Migrating {
    stop: Option<Sender<()>>,
    thread: Option<JoinHandle<()>>,
    path: PathBuf,
}

impl Migrating {
    /// Writes the file in `data` now and every [`PERIOD`] from a thread of its own.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Io`] when the file cannot be written or the thread not started.
    ///
    /// ```
    /// let dir = tempfile::tempdir().unwrap();
    /// let m = mistarr_server::migrating::Migrating::begin(dir.path(), 16, 17).unwrap();
    /// let path = dir.path().join(mistarr_server::migrating::FILE);
    /// assert!(std::fs::read_to_string(&path).unwrap().starts_with("migrating from 16 to 17"));
    /// drop(m);
    /// assert!(!path.exists());
    /// ```
    pub fn begin(data: &Path, from: u32, to: u32) -> Result<Self> {
        let path = data.join(FILE);
        let line = move || format!("migrating from {from} to {to}: {}\n", activity());
        std::fs::write(&path, line())?;
        let (stop, stopped) = mpsc::channel::<()>();
        let target = path.clone();
        let thread = std::thread::Builder::new()
            .name(THREAD.into())
            .spawn(move || {
                while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(PERIOD) {
                    if let Err(e) = std::fs::write(&target, line()) {
                        tracing::warn!(error = %e, "cannot update the migration progress file");
                    }
                }
            })?;
        Ok(Self {
            stop: Some(stop),
            thread: Some(thread),
            path,
        })
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

/// `io <bytes read and written> cpu <ticks>` of this process, each 0 when unreadable.
fn activity() -> String {
    let io = std::fs::read_to_string("/proc/self/io").unwrap_or_default();
    let field = |name: &str| -> u64 {
        io.lines()
            .find_map(|l| l.strip_prefix(name))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0)
    };
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    // utime and stime are the 12th and 13th fields after the parenthesised name.
    let cpu: u64 = stat
        .rsplit_once(')')
        .map(|(_, rest)| rest.split_whitespace().skip(11).take(2))
        .into_iter()
        .flatten()
        .filter_map(|v| v.parse::<u64>().ok())
        .sum();
    format!("io {} cpu {cpu}", field("rchar:") + field("wchar:"))
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

    #[test]
    fn activity_counts_io_and_cpu() {
        let a = activity();
        assert!(a.starts_with("io ") && a.contains(" cpu "), "{a}");
    }

    #[test]
    fn the_file_is_rewritten_while_the_migration_runs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let m = Migrating::begin(dir.path(), 3, 4).expect("begin");
        let path = dir.path().join(FILE);
        let first = std::fs::read_to_string(&path).expect("read");
        assert!(first.starts_with("migrating from 3 to 4: io "), "{first}");
        drop(m);
        assert!(!path.exists());
    }
}
