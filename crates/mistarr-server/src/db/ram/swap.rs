//! The swap that puts a finished copy of the database in place of the live file; the
//! crash points are in `docs/ARCHITECTURE.md` "DAT import in RAM".

use std::path::Path;

use crate::{Error, Result};

/// Suffix the old database file takes while [`install_file`] puts a new one in its place.
pub const OLD_SUFFIX: &str = ".old";

/// Suffix of the empty marker [`install_file`] keeps beside the database for the length
/// of its renames, so a start after a crash that left no readable name never creates one.
pub const SWAP_SUFFIX: &str = ".swap";

/// Puts `new`, a complete database file synced beside `path`, in its place once no
/// connection has `path` open: removes the old file's `-wal` and `-shm`, which must hold
/// nothing unwritten, writes the `.swap` marker, renames `path` to `.old`, `new` to
/// `path`, and removes `.old` and the marker, syncing the directory after each.
/// [`super::clean_stale`] finishes a swap a crash cut short; `docs/ARCHITECTURE.md` "DAT
/// import in RAM" lists every crash point.
///
/// # Errors
///
/// [`Error::File`] naming the file a removal or a rename failed on; the old file is then
/// back under `path`, unless renaming it back failed too, which the error names.
pub(crate) fn install_file(path: &Path, new: &Path) -> Result<()> {
    for suffix in ["-wal", "-shm"] {
        remove_if_present(&crate::db::sibling(path, suffix))?;
    }
    let old = crate::db::sibling(path, OLD_SUFFIX);
    remove_if_present(&old)?;
    let marker = crate::db::sibling(path, SWAP_SUFFIX);
    std::fs::File::create(&marker)
        .and_then(|f| f.sync_all())
        .map_err(Error::io_at(&marker))?;
    sync_parent(path);
    if let Err(e) = std::fs::rename(path, &old) {
        remove_if_present(&marker)?;
        return Err(Error::io_at(path)(e));
    }
    sync_parent(path);
    if let Err(e) = std::fs::rename(new, path) {
        let back = std::fs::rename(&old, path);
        sync_parent(path);
        return Err(match back {
            Ok(()) => {
                remove_if_present(&marker)?;
                Error::io_at(new)(e)
            }
            Err(b) => Error::io_at(new)(std::io::Error::other(format!(
                "{e}; the old database stays at {}: {b}",
                old.display()
            ))),
        });
    }
    sync_parent(path);
    for leftover in [&old, &marker] {
        if let Err(e) = std::fs::remove_file(leftover) {
            tracing::warn!(error = %e, "cannot remove a file of the swap; the next start does");
        }
        sync_parent(path);
    }
    Ok(())
}

/// Syncs the directory holding `path`, making a rename in it durable where the mount is
/// not `dirsync`.
pub(super) fn sync_parent(path: &Path) {
    if let Some(dir) = path.parent() {
        if let Err(e) = std::fs::File::open(dir).and_then(|d| d.sync_all()) {
            tracing::warn!(error = %e, "cannot sync the database directory");
        }
    }
}

/// Removes `path`, treating a file already gone as removed.
fn remove_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(Error::io_at(path)(e)),
        _ => Ok(()),
    }
}
