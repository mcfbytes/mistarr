//! Directory listings and file stats as the jobs read and `files` stores them.

use std::fs;
use std::io;
use std::path::Path;

/// Every entry of a directory listing, or the first error: an entry that fails partway
/// makes the whole directory unreadable, so no later file's row is pruned for it.
pub(crate) fn all_entries<T>(entries: impl Iterator<Item = io::Result<T>>) -> io::Result<Vec<T>> {
    entries.collect()
}

/// The size and mtime of `meta` as `files` stores them, or the error of a filesystem
/// that keeps no mtime; an mtime before 1970 is 0.
pub(crate) fn stat(meta: &fs::Metadata) -> io::Result<(i64, i64)> {
    let size = i64::try_from(meta.len()).unwrap_or(i64::MAX);
    let mtime = meta
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    Ok((size, mtime))
}

/// A file's size and mtime, as stored in `files`.
pub(crate) fn file_meta(path: &Path) -> io::Result<(i64, i64)> {
    stat(&fs::metadata(path)?)
}

/// `path`'s extension, lowercased.
pub(crate) fn extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
}

/// Whether `path` names a zip by its extension, in any case.
pub(crate) fn is_zip(path: &Path) -> bool {
    extension(path).as_deref() == Some("zip")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_error_partway_makes_the_listing_fail() {
        let fine: Vec<io::Result<u8>> = vec![Ok(1), Ok(2)];
        assert_eq!(all_entries(fine.into_iter()).expect("listed"), [1, 2]);
        let broken: Vec<io::Result<u8>> = vec![Ok(1), Err(io::Error::other("EIO")), Ok(3)];
        assert!(all_entries(broken.into_iter()).is_err());
    }

    #[test]
    fn stats_read_size_and_mtime() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("a.nes");
        fs::write(&path, b"12345").expect("write");
        let (size, mtime) = file_meta(&path).expect("stat");
        assert_eq!(size, 5);
        assert!(mtime > 0);
        let meta = fs::metadata(&path).expect("meta");
        assert_eq!(stat(&meta).expect("stat"), (size, mtime));
        assert!(file_meta(&dir.path().join("gone")).is_err());
    }

    #[test]
    fn extensions_are_lowercased_and_zips_found_in_any_case() {
        assert_eq!(extension(Path::new("a/b.NES")).as_deref(), Some("nes"));
        assert_eq!(extension(Path::new("a/b")), None);
        assert!(is_zip(Path::new("a.ZIP")));
        assert!(!is_zip(Path::new("a.nes")));
        assert!(!is_zip(Path::new("zip")));
    }
}
