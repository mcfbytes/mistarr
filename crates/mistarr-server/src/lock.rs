//! One server per data directory, held by an advisory lock on `mistarr.lock`.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use rustix::fs::{flock, FlockOperation};

use crate::error::{Error, Result};

/// File name of the lock inside the data directory.
pub const LOCK_FILE: &str = "mistarr.lock";

/// Holds the data directory's lock until dropped.
#[derive(Debug)]
pub struct InstanceLock {
    _file: File,
    path: PathBuf,
}

impl InstanceLock {
    /// Takes the lock on `<data>/mistarr.lock` without waiting.
    ///
    /// # Errors
    ///
    /// [`Error::AlreadyRunning`] when another process holds it, [`Error::Io`]
    /// when the file cannot be opened or locked.
    ///
    /// ```
    /// let dir = std::env::temp_dir().join("mistarr-doc-lock");
    /// std::fs::create_dir_all(&dir).unwrap();
    /// let held = mistarr_server::lock::InstanceLock::acquire(&dir).unwrap();
    /// assert!(held.path().ends_with("mistarr.lock"));
    /// ```
    pub fn acquire(data: &Path) -> Result<Self> {
        let path = data.join(LOCK_FILE);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)?;
        match flock(&file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => Ok(Self { _file: file, path }),
            Err(e) if e == rustix::io::Errno::WOULDBLOCK => {
                Err(Error::AlreadyRunning(data.to_path_buf()))
            }
            Err(e) => Err(Error::Io(e.into())),
        }
    }

    /// The lock file.
    ///
    /// ```
    /// let dir = std::env::temp_dir().join("mistarr-doc-lock-path");
    /// std::fs::create_dir_all(&dir).unwrap();
    /// let held = mistarr_server::lock::InstanceLock::acquire(&dir).unwrap();
    /// assert!(held.path().is_file());
    /// ```
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_holder_is_refused_until_the_first_drops() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = InstanceLock::acquire(dir.path()).expect("first");
        let second = InstanceLock::acquire(dir.path());
        assert!(matches!(second, Err(Error::AlreadyRunning(ref p)) if p == dir.path()));
        drop(first);
        InstanceLock::acquire(dir.path()).expect("free again");
    }
}
