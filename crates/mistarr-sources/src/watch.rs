//! A synchronous, poll-based scanner for the watched `sources/` directory.
//! See `docs/PRINCIPLES.md` section 2: sources arrive only as files the user
//! placed here. No `notify` crate; the server calls [`Scanner::scan_once`]
//! on a timer.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::error::SourceError;

/// Subdirectory a loaded source's file is moved into.
pub const LOADED_DIR: &str = "loaded";
/// Subdirectory a rejected source's file is moved into.
pub const REJECTED_DIR: &str = "rejected";
/// Default minimum age, in seconds, before a file is considered stable.
pub const DEFAULT_MIN_AGE_SECS: u64 = 2;

/// The kind of source file found in the watched directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// A `.torrent` file.
    Torrent,
    /// A `.magnet` file.
    Magnet,
}

/// One unclassified or newly classified file found by [`Scanner::scan_once`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Incoming {
    /// Full path to the file, still in the watched directory.
    pub path: PathBuf,
    /// Which parser should handle it.
    pub kind: SourceKind,
}

/// Polls a watched directory across calls, remembering each file's size so
/// a file still being written (e.g. copied over SMB) is not surfaced until
/// its size has held steady across two scans and its mtime has aged past
/// the stability window.
pub struct Scanner {
    min_age: Duration,
    last_sizes: HashMap<PathBuf, u64>,
}

impl Default for Scanner {
    fn default() -> Self {
        Self::new()
    }
}

impl Scanner {
    /// A scanner using [`DEFAULT_MIN_AGE_SECS`] as its stability window.
    #[must_use]
    pub fn new() -> Self {
        Self::with_min_age_secs(DEFAULT_MIN_AGE_SECS)
    }

    /// A scanner that requires a file's mtime to be at least `min_age_secs`
    /// old, as well as size-stable across two scans, before returning it.
    #[must_use]
    pub fn with_min_age_secs(min_age_secs: u64) -> Self {
        Self {
            min_age: Duration::from_secs(min_age_secs),
            last_sizes: HashMap::new(),
        }
    }

    /// Lists `.torrent` and `.magnet` files directly inside `dir` that are
    /// old enough and unchanged in size since the previous call, skipping
    /// the `loaded/` and `rejected/` subdirectories and anything else.
    /// Returns an empty list if `dir` cannot be read, so a transient error
    /// just retries on the next poll.
    ///
    /// ```
    /// use mistarr_sources::watch::Scanner;
    /// let dir = tempfile::tempdir().unwrap();
    /// std::fs::write(dir.path().join("a.torrent"), b"data").unwrap();
    /// let mut scanner = Scanner::with_min_age_secs(0);
    /// assert!(scanner.scan_once(dir.path()).is_empty(), "first scan only records the size");
    /// assert_eq!(scanner.scan_once(dir.path()).len(), 1);
    /// ```
    pub fn scan_once(&mut self, dir: &Path) -> Vec<Incoming> {
        let Ok(entries) = fs::read_dir(dir) else {
            return Vec::new();
        };
        let now = SystemTime::now();
        let mut found = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let kind = match path.extension().and_then(|e| e.to_str()) {
                Some("torrent") => SourceKind::Torrent,
                Some("magnet") => SourceKind::Magnet,
                _ => continue,
            };
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            let size = metadata.len();
            let old_enough = metadata
                .modified()
                .is_ok_and(|m| now.duration_since(m).unwrap_or(Duration::ZERO) >= self.min_age);
            let previous_size = self.last_sizes.insert(path.clone(), size);
            if old_enough && previous_size == Some(size) {
                found.push(Incoming { path, kind });
            }
        }
        found
    }
}

/// Moves a successfully parsed and bound source's file into `loaded/`
/// beside `dir`.
///
/// # Errors
///
/// Returns [`SourceError::Io`] when the directory cannot be created or the
/// file cannot be moved.
///
/// ```
/// use mistarr_sources::watch::mark_loaded;
/// let dir = tempfile::tempdir().unwrap();
/// let file = dir.path().join("a.torrent");
/// std::fs::write(&file, b"data").unwrap();
/// mark_loaded(&file).unwrap();
/// assert!(dir.path().join("loaded/a.torrent").exists());
/// ```
pub fn mark_loaded(path: &Path) -> Result<(), SourceError> {
    move_into(path, LOADED_DIR)?;
    Ok(())
}

/// Moves a source's file into `rejected/` beside `dir` and writes
/// `<name>.reason.txt` with `reason` next to it.
///
/// # Errors
///
/// Returns [`SourceError::Io`] when the directory cannot be created, the
/// file cannot be moved, or the reason file cannot be written.
///
/// ```
/// use mistarr_sources::watch::mark_rejected;
/// let dir = tempfile::tempdir().unwrap();
/// let file = dir.path().join("a.torrent");
/// std::fs::write(&file, b"data").unwrap();
/// mark_rejected(&file, "malformed bencode").unwrap();
/// assert!(dir.path().join("rejected/a.torrent").exists());
/// assert!(dir.path().join("rejected/a.torrent.reason.txt").exists());
/// ```
pub fn mark_rejected(path: &Path, reason: &str) -> Result<(), SourceError> {
    let moved = move_into(path, REJECTED_DIR)?;
    let mut reason_path = moved.clone().into_os_string();
    reason_path.push(".reason.txt");
    fs::write(reason_path, reason)?;
    Ok(())
}

fn move_into(path: &Path, subdir: &str) -> Result<PathBuf, SourceError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let target_dir = parent.join(subdir);
    fs::create_dir_all(&target_dir)?;
    let file_name = path.file_name().ok_or_else(|| {
        SourceError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path has no file name",
        ))
    })?;
    let target = target_dir.join(file_name);
    fs::rename(path, &target)?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use std::fs::File;

    use super::*;

    // Backdates a file's mtime so it clears the scanner's stability window
    // without the test having to sleep for real seconds.
    fn backdate(path: &Path, age: Duration) {
        let modified = SystemTime::now() - age;
        File::open(path).unwrap().set_modified(modified).unwrap();
    }

    #[test]
    fn classifies_torrent_and_magnet_and_skips_others() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.torrent"), b"x").unwrap();
        fs::write(dir.path().join("b.magnet"), b"x").unwrap();
        fs::write(dir.path().join("c.txt"), b"x").unwrap();
        backdate(&dir.path().join("a.torrent"), Duration::from_secs(10));
        backdate(&dir.path().join("b.magnet"), Duration::from_secs(10));

        let mut scanner = Scanner::with_min_age_secs(2);
        scanner.scan_once(dir.path());
        let mut found = scanner.scan_once(dir.path());
        found.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].kind, SourceKind::Torrent);
        assert_eq!(found[1].kind, SourceKind::Magnet);
    }

    #[test]
    fn ignores_loaded_and_rejected_subdirs() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(LOADED_DIR)).unwrap();
        fs::write(dir.path().join(LOADED_DIR).join("old.torrent"), b"x").unwrap();
        let mut scanner = Scanner::with_min_age_secs(0);
        scanner.scan_once(dir.path());
        assert!(scanner.scan_once(dir.path()).is_empty());
    }

    #[test]
    fn missing_dir_returns_empty() {
        let mut scanner = Scanner::new();
        assert!(scanner
            .scan_once(Path::new("/nonexistent/does-not-exist-mistarr"))
            .is_empty());
    }

    #[test]
    fn fresh_file_is_not_returned_before_min_age() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.torrent"), b"x").unwrap();
        let mut scanner = Scanner::with_min_age_secs(2);
        scanner.scan_once(dir.path());
        assert!(scanner.scan_once(dir.path()).is_empty());
    }

    #[test]
    fn stable_old_file_is_returned_after_a_second_scan() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.torrent");
        fs::write(&path, b"x").unwrap();
        backdate(&path, Duration::from_secs(10));

        let mut scanner = Scanner::with_min_age_secs(2);
        assert!(scanner.scan_once(dir.path()).is_empty());
        let found = scanner.scan_once(dir.path());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, path);
    }

    #[test]
    fn growing_file_is_not_returned_while_its_size_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.torrent");
        fs::write(&path, b"x").unwrap();
        backdate(&path, Duration::from_secs(10));

        let mut scanner = Scanner::with_min_age_secs(2);
        assert!(scanner.scan_once(dir.path()).is_empty());

        fs::write(&path, b"xx").unwrap();
        backdate(&path, Duration::from_secs(10));
        assert!(
            scanner.scan_once(dir.path()).is_empty(),
            "size changed since the previous scan"
        );

        let found = scanner.scan_once(dir.path());
        assert_eq!(found.len(), 1, "size held steady on this scan");
    }

    #[test]
    fn mark_loaded_moves_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.torrent");
        fs::write(&file, b"x").unwrap();
        mark_loaded(&file).unwrap();
        assert!(!file.exists());
        assert!(dir.path().join(LOADED_DIR).join("a.torrent").exists());
    }

    #[test]
    fn mark_rejected_moves_file_and_writes_reason() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("b.magnet");
        fs::write(&file, b"x").unwrap();
        mark_rejected(&file, "bad hash").unwrap();
        let moved = dir.path().join(REJECTED_DIR).join("b.magnet");
        assert!(moved.exists());
        let reason = fs::read_to_string(format!("{}.reason.txt", moved.display())).unwrap();
        assert_eq!(reason, "bad hash");
    }
}
