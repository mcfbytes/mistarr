//! A synchronous, poll-based scanner for the watched `sources/` directory.
//! See `docs/PRINCIPLES.md` section 2: sources arrive only as files the user
//! placed here. No `notify` crate; the server calls [`scan_once`] on a timer.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::SourceError;

/// Subdirectory a loaded source's file is moved into.
pub const LOADED_DIR: &str = "loaded";
/// Subdirectory a rejected source's file is moved into.
pub const REJECTED_DIR: &str = "rejected";

/// The kind of source file found in the watched directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// A `.torrent` file.
    Torrent,
    /// A `.magnet` file.
    Magnet,
}

/// One unclassified or newly classified file found by [`scan_once`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Incoming {
    /// Full path to the file, still in the watched directory.
    pub path: PathBuf,
    /// Which parser should handle it.
    pub kind: SourceKind,
}

/// Lists `.torrent` and `.magnet` files directly inside `dir`, skipping the
/// `loaded/` and `rejected/` subdirectories and anything else. Returns an
/// empty list if `dir` cannot be read, so a transient error just retries on
/// the next poll.
///
/// ```
/// use mistarr_sources::watch::scan_once;
/// let dir = tempfile::tempdir().unwrap();
/// std::fs::write(dir.path().join("a.torrent"), b"data").unwrap();
/// let found = scan_once(dir.path());
/// assert_eq!(found.len(), 1);
/// ```
#[must_use]
pub fn scan_once(dir: &Path) -> Vec<Incoming> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
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
        found.push(Incoming { path, kind });
    }
    found
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
    use super::*;

    #[test]
    fn classifies_torrent_and_magnet_and_skips_others() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.torrent"), b"x").unwrap();
        fs::write(dir.path().join("b.magnet"), b"x").unwrap();
        fs::write(dir.path().join("c.txt"), b"x").unwrap();
        let mut found = scan_once(dir.path());
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
        assert!(scan_once(dir.path()).is_empty());
    }

    #[test]
    fn missing_dir_returns_empty() {
        assert!(scan_once(Path::new("/nonexistent/does-not-exist-mistarr")).is_empty());
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
