//! Files dropped into a watched directory; see the `mistarr-sources` row of `docs/ARCHITECTURE.md`.

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Subdirectory an accepted file is moved into.
pub const LOADED_DIR: &str = "loaded";
/// Subdirectory a rejected file is moved into.
pub const REJECTED_DIR: &str = "rejected";
/// Suffix of the file beside a rejected file that holds the reason.
pub const REASON_SUFFIX: &str = ".reason.txt";
/// Default minimum age, in seconds, before a file is considered stable.
pub const DEFAULT_MIN_AGE_SECS: u64 = 2;

/// Names tried before a free one is given up on.
const ATTEMPTS: u32 = 1000;

/// Finds files in a directory that have stopped changing: modified at least
/// the minimum age ago, with the same size across two polls. A file is
/// reported once per size and mtime, so a new file under a reused name is
/// reported again; a file that is gone is forgotten.
#[derive(Debug)]
pub struct StableFiles {
    min_age: Duration,
    accepts: fn(&str) -> bool,
    sizes: HashMap<PathBuf, u64>,
    reported: HashMap<PathBuf, (u64, Option<SystemTime>)>,
}

impl StableFiles {
    /// A watcher for files whose names `accepts`, stable after `min_age`.
    ///
    /// ```
    /// let dir = tempfile::tempdir().unwrap();
    /// std::fs::write(dir.path().join("a.dat"), b"x").unwrap();
    /// let mut w = mistarr_sources::intake::StableFiles::new(std::time::Duration::ZERO, |_| true);
    /// assert!(w.poll(dir.path()).is_empty());
    /// assert_eq!(w.poll(dir.path()).len(), 1);
    /// assert!(w.poll(dir.path()).is_empty());
    /// ```
    #[must_use]
    pub fn new(min_age: Duration, accepts: fn(&str) -> bool) -> Self {
        Self {
            min_age,
            accepts,
            sizes: HashMap::new(),
            reported: HashMap::new(),
        }
    }

    /// Reports `path` again once it is stable, for a file whose import failed.
    pub fn forget(&mut self, path: &Path) {
        self.reported.remove(path);
    }

    /// Regular files directly in `dir` that the name filter accepts and that
    /// became stable since the last poll, sorted. An unreadable `dir` yields
    /// none, so a transient error retries on the next poll.
    pub fn poll(&mut self, dir: &Path) -> Vec<PathBuf> {
        let Ok(entries) = fs::read_dir(dir) else {
            return Vec::new();
        };
        let now = SystemTime::now();
        let mut present = HashSet::new();
        let mut stable = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if !meta.is_file() || !(self.accepts)(&entry.file_name().to_string_lossy()) {
                continue;
            }
            present.insert(path.clone());
            let old = meta
                .modified()
                .is_ok_and(|m| now.duration_since(m).unwrap_or(Duration::ZERO) >= self.min_age);
            let same = self.sizes.insert(path.clone(), meta.len()) == Some(meta.len());
            let signature = (meta.len(), meta.modified().ok());
            if old && same && self.reported.get(&path) != Some(&signature) {
                self.reported.insert(path.clone(), signature);
                stable.push(path);
            }
        }
        self.sizes.retain(|p, _| present.contains(p));
        self.reported.retain(|p, _| present.contains(p));
        stable.sort();
        stable
    }
}

/// `dir/name`, or `dir/stem (N).ext` for the first N that is free. Nothing is
/// created, so the name can be read before the caller uses it.
///
/// # Errors
///
/// [`io::ErrorKind::AlreadyExists`] when no name is free within a bounded
/// number of attempts.
///
/// ```
/// let dir = tempfile::tempdir().unwrap();
/// let first = mistarr_sources::intake::free_name(dir.path(), "a.dat").unwrap();
/// std::fs::write(&first, b"").unwrap();
/// let second = mistarr_sources::intake::free_name(dir.path(), "a.dat").unwrap();
/// assert!(second.ends_with("a (1).dat"));
/// ```
pub fn free_name(dir: &Path, name: &str) -> io::Result<PathBuf> {
    (0..=ATTEMPTS)
        .map(|n| numbered(dir, name, n))
        .find(|c| !c.exists())
        .ok_or_else(exhausted)
}

/// Moves `path` into `loaded/` beside it, under its name or the first free
/// variant, and returns the new path. The work is synchronous; async callers
/// run it on a blocking thread.
///
/// # Errors
///
/// [`io::Error`] when the directory cannot be made, no name is free, or the
/// move fails.
///
/// ```
/// let dir = tempfile::tempdir().unwrap();
/// let file = dir.path().join("a.torrent");
/// std::fs::write(&file, b"data").unwrap();
/// let moved = mistarr_sources::intake::accept(&file).unwrap();
/// assert_eq!(moved, dir.path().join("loaded/a.torrent"));
/// assert!(!file.exists());
/// ```
pub fn accept(path: &Path) -> io::Result<PathBuf> {
    let target = claim(path, LOADED_DIR)?;
    fs::rename(path, &target)?;
    Ok(target)
}

/// Moves `path` into `rejected/` beside it and writes `reason\n` to
/// `<name>.reason.txt` next to it, returning the new path. The work is
/// synchronous; async callers run it on a blocking thread.
///
/// # Errors
///
/// [`io::Error`] when the directory cannot be made, no name is free, or the
/// move or the reason write fails.
///
/// ```
/// let dir = tempfile::tempdir().unwrap();
/// let file = dir.path().join("a.torrent");
/// std::fs::write(&file, b"data").unwrap();
/// mistarr_sources::intake::reject(&file, "malformed").unwrap();
/// let reason = std::fs::read_to_string(dir.path().join("rejected/a.torrent.reason.txt")).unwrap();
/// assert_eq!(reason, "malformed\n");
/// ```
pub fn reject(path: &Path, reason: &str) -> io::Result<PathBuf> {
    let target = claim(path, REJECTED_DIR)?;
    fs::rename(path, &target)?;
    let mut sidecar = target.clone().into_os_string();
    sidecar.push(REASON_SUFFIX);
    fs::write(sidecar, format!("{reason}\n"))?;
    Ok(target)
}

/// Creates an empty file for `path`'s name in `subdir` beside it with
/// `create_new`, so a concurrent writer cannot take the same name.
fn claim(path: &Path, subdir: &str) -> io::Result<PathBuf> {
    let dir = path.parent().unwrap_or_else(|| Path::new(".")).join(subdir);
    fs::create_dir_all(&dir)?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
    for n in 0..=ATTEMPTS {
        let target = numbered(&dir, name, n);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
        {
            Ok(_) => return Ok(target),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(exhausted())
}

/// `dir/name` for 0, else `dir/stem (n).ext`.
fn numbered(dir: &Path, name: &str, n: u32) -> PathBuf {
    if n == 0 {
        return dir.join(name);
    }
    let p = Path::new(name);
    let stem = p
        .file_stem()
        .map_or_else(|| name.to_owned(), |s| s.to_string_lossy().into_owned());
    let ext = p
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    dir.join(format!("{stem} ({n}){ext}"))
}

fn exhausted() -> io::Error {
    io::Error::new(io::ErrorKind::AlreadyExists, "no free file name")
}

#[cfg(test)]
mod tests {
    use std::fs::File;

    use super::*;

    fn any(_: &str) -> bool {
        true
    }

    fn backdate(path: &Path, age: Duration) {
        let modified = SystemTime::now() - age;
        File::open(path).unwrap().set_modified(modified).unwrap();
    }

    #[test]
    fn a_stable_file_is_reported_once() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.dat");
        fs::write(&a, b"one").unwrap();
        fs::write(dir.path().join(".part"), b"x").unwrap();
        fs::create_dir(dir.path().join("loaded")).unwrap();
        let mut w = StableFiles::new(Duration::ZERO, |n| !n.starts_with('.'));
        assert!(w.poll(dir.path()).is_empty(), "first poll records the size");
        fs::write(&a, b"grown").unwrap();
        assert!(w.poll(dir.path()).is_empty(), "size changed");
        assert_eq!(w.poll(dir.path()), std::slice::from_ref(&a));
        assert!(w.poll(dir.path()).is_empty(), "reported once");
        fs::write(&a, b"replaced").unwrap();
        w.poll(dir.path());
        assert_eq!(w.poll(dir.path()), std::slice::from_ref(&a), "new content");
    }

    #[test]
    fn a_young_file_waits_for_the_minimum_age() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.torrent");
        fs::write(&a, b"x").unwrap();
        let mut w = StableFiles::new(Duration::from_secs(3600), any);
        w.poll(dir.path());
        assert!(w.poll(dir.path()).is_empty());
        backdate(&a, Duration::from_secs(7200));
        assert_eq!(w.poll(dir.path()), [a]);
    }

    #[test]
    fn the_name_filter_decides_what_is_watched() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.torrent"), b"x").unwrap();
        fs::write(dir.path().join("b.txt"), b"x").unwrap();
        let mut w = StableFiles::new(Duration::ZERO, |n| {
            Path::new(n).extension().is_some_and(|e| e == "torrent")
        });
        w.poll(dir.path());
        assert_eq!(w.poll(dir.path()), [dir.path().join("a.torrent")]);
    }

    #[test]
    fn a_moved_file_is_forgotten_and_a_returning_one_is_new() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.dat");
        fs::write(&a, b"x").unwrap();
        let mut w = StableFiles::new(Duration::ZERO, any);
        w.poll(dir.path());
        assert_eq!(w.poll(dir.path()), std::slice::from_ref(&a));
        accept(&a).unwrap();
        assert!(w.poll(dir.path()).is_empty());
        fs::write(&a, b"x").unwrap();
        w.poll(dir.path());
        assert_eq!(w.poll(dir.path()), [a]);
    }

    #[test]
    fn a_forgotten_path_is_reported_again() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.dat");
        fs::write(&a, b"x").unwrap();
        let mut w = StableFiles::new(Duration::ZERO, any);
        w.poll(dir.path());
        assert_eq!(w.poll(dir.path()), std::slice::from_ref(&a));
        w.forget(&a);
        assert_eq!(w.poll(dir.path()), [a]);
    }

    #[test]
    fn an_unreadable_directory_yields_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = StableFiles::new(Duration::ZERO, any);
        assert!(w.poll(&dir.path().join("none")).is_empty());
    }

    #[test]
    fn free_name_numbers_taken_names() {
        let dir = tempfile::tempdir().unwrap();
        for expected in ["a.dat", "a (1).dat", "a (2).dat"] {
            let p = free_name(dir.path(), "a.dat").unwrap();
            assert_eq!(p, dir.path().join(expected));
            fs::write(p, b"").unwrap();
        }
        assert_eq!(
            free_name(dir.path(), "plain").unwrap(),
            dir.path().join("plain")
        );
    }

    #[test]
    fn a_name_already_loaded_gets_a_new_name() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(LOADED_DIR)).unwrap();
        fs::write(dir.path().join(LOADED_DIR).join("a.torrent"), b"old").unwrap();
        let file = dir.path().join("a.torrent");
        fs::write(&file, b"new").unwrap();
        let moved = accept(&file).unwrap();
        assert_eq!(moved, dir.path().join(LOADED_DIR).join("a (1).torrent"));
        assert_eq!(fs::read(&moved).unwrap(), b"new");
        assert_eq!(
            fs::read(dir.path().join(LOADED_DIR).join("a.torrent")).unwrap(),
            b"old"
        );
        assert!(!file.exists());
    }

    #[test]
    fn reject_moves_the_file_and_writes_the_reason() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("b.magnet");
        fs::write(&file, b"x").unwrap();
        let moved = reject(&file, "bad hash").unwrap();
        assert_eq!(moved, dir.path().join(REJECTED_DIR).join("b.magnet"));
        let reason = fs::read_to_string(dir.path().join("rejected/b.magnet.reason.txt")).unwrap();
        assert_eq!(reason, "bad hash\n");
        fs::write(&file, b"y").unwrap();
        let again = reject(&file, "worse").unwrap();
        assert_eq!(again, dir.path().join(REJECTED_DIR).join("b (1).magnet"));
    }

    #[test]
    fn a_path_without_a_file_name_is_invalid() {
        let err = accept(Path::new("/")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}
