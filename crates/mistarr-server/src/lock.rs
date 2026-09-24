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
    held: bool,
}

/// What a failed non-blocking `flock` means for starting up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Another process holds the lock.
    Held,
    /// The filesystem cannot lock files; start without the lock.
    Unsupported,
    /// Any other failure.
    Other,
}

/// Classifies an `flock` error.
///
/// ```
/// use mistarr_server::lock::{refusal, Refusal};
/// use rustix::io::Errno;
/// assert_eq!(refusal(Errno::WOULDBLOCK), Refusal::Held);
/// assert_eq!(refusal(Errno::NOLCK), Refusal::Unsupported);
/// assert_eq!(refusal(Errno::ACCESS), Refusal::Other);
/// ```
#[must_use]
pub fn refusal(e: rustix::io::Errno) -> Refusal {
    use rustix::io::Errno;
    if e == Errno::WOULDBLOCK {
        Refusal::Held
    } else if e == Errno::NOLCK || e == Errno::OPNOTSUPP || e == Errno::NOSYS {
        Refusal::Unsupported
    } else {
        Refusal::Other
    }
}

impl InstanceLock {
    /// Takes the lock on `<data>/mistarr.lock` without waiting. On a
    /// filesystem that cannot lock files it warns and carries on unlocked.
    ///
    /// # Errors
    ///
    /// [`Error::AlreadyRunning`] when another process holds it, [`Error::Io`]
    /// when the file cannot be opened or locked for another reason.
    ///
    /// ```
    /// let dir = tempfile::tempdir().unwrap();
    /// let held = mistarr_server::lock::InstanceLock::acquire(dir.path()).unwrap();
    /// assert!(held.path().ends_with("mistarr.lock") && held.held());
    /// ```
    pub fn acquire(data: &Path) -> Result<Self> {
        let path = data.join(LOCK_FILE);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)?;
        let held = match flock(&file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => true,
            Err(e) => match refusal(e) {
                Refusal::Held => return Err(Error::AlreadyRunning(data.to_path_buf())),
                Refusal::Unsupported => {
                    tracing::warn!(path = %path.display(), error = %e, "cannot lock the data directory; a second server would not be refused");
                    false
                }
                Refusal::Other => return Err(Error::Io(e.into())),
            },
        };
        Ok(Self {
            _file: file,
            path,
            held,
        })
    }

    /// Whether the lock is actually held, which is false on a filesystem
    /// without file locks.
    ///
    /// ```
    /// let dir = tempfile::tempdir().unwrap();
    /// assert!(mistarr_server::lock::InstanceLock::acquire(dir.path()).unwrap().held());
    /// ```
    #[must_use]
    pub fn held(&self) -> bool {
        self.held
    }

    /// The lock file.
    ///
    /// ```
    /// let dir = tempfile::tempdir().unwrap();
    /// let held = mistarr_server::lock::InstanceLock::acquire(dir.path()).unwrap();
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

    #[test]
    fn only_a_held_lock_refuses_and_unsupported_locking_is_not_fatal() {
        use rustix::io::Errno;
        assert_eq!(refusal(Errno::WOULDBLOCK), Refusal::Held);
        for e in [Errno::NOLCK, Errno::OPNOTSUPP, Errno::NOSYS] {
            assert_eq!(refusal(e), Refusal::Unsupported, "{e}");
        }
        assert_eq!(refusal(Errno::BADF), Refusal::Other);
    }
}
