//! The directory for SQLite's temporary files, chosen and prepared before any connection
//! opens; see `docs/ARCHITECTURE.md` "Writes on a sync mount".

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// The environment variable SQLite reads for its temporary file directory.
pub const SQLITE_TMPDIR: &str = "SQLITE_TMPDIR";

/// The RAM-backed directory for SQLite's temporary files, used when it can be written.
pub const RAM_TEMP_DIR: &str = "/tmp/mistarr";

/// The environment variable that names another directory in place of [`RAM_TEMP_DIR`],
/// so tests and side-by-side servers each get their own.
pub const TEMP_DIR_ENV: &str = "MISTARR_TEMP_DIR";

/// Where SQLite's temporary files go, and why the RAM directory was refused when it was.
#[derive(Debug)]
pub struct TempDir {
    /// The directory chosen.
    pub dir: PathBuf,
    /// Why the RAM directory could not be used; `None` when it is `dir`.
    pub refused: Option<Error>,
}

/// Returns `ram` when it is, or can be made, a directory of mode 0700 that this user owns,
/// is not a symlink, and takes a file, having emptied it with [`prepare_temp_dir`]; else
/// prepares and returns `fallback`. On the card every temporary page would be written
/// through its `sync` mount; see `docs/ARCHITECTURE.md` "Writes on a sync mount".
///
/// # Errors
///
/// [`Error::File`] naming `fallback` when it cannot be prepared either.
///
/// ```
/// let dir = tempfile::tempdir().unwrap();
/// let (ram, disk) = (dir.path().join("ram"), dir.path().join("disk"));
/// let chosen = mistarr_server::db::tempdir::choose_temp_dir(&ram, &disk).unwrap();
/// assert_eq!(chosen.dir, ram);
/// assert!(chosen.refused.is_none());
/// ```
pub fn choose_temp_dir(ram: &Path, fallback: &Path) -> Result<TempDir> {
    let usable = |dir: &Path| -> Result<()> {
        private_dir(dir)?;
        prepare_temp_dir(dir)?;
        let probe = dir.join(format!(".probe-{}", std::process::id()));
        std::fs::write(&probe, b"x").map_err(crate::Error::io_at(&probe))?;
        std::fs::remove_file(&probe).map_err(crate::Error::io_at(&probe))?;
        Ok(())
    };
    match usable(ram) {
        Ok(()) => Ok(TempDir {
            dir: ram.to_path_buf(),
            refused: None,
        }),
        Err(e) => {
            prepare_temp_dir(fallback)?;
            Ok(TempDir {
                dir: fallback.to_path_buf(),
                refused: Some(e),
            })
        }
    }
}

/// Creates `dir` with mode 0700, or checks the one there is a real directory this user
/// owns and narrows it to 0700, so no other user can read or plant temporary files.
pub(crate) fn private_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    let refuse = |why: &str| -> Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("{} {why}", dir.display()),
        )
        .into())
    };
    let meta = match std::fs::symlink_metadata(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::DirBuilder::new()
                .mode(0o700)
                .recursive(true)
                .create(dir)?;
            std::fs::symlink_metadata(dir).map_err(crate::Error::io_at(dir))?
        }
        other => other?,
    };
    if meta.file_type().is_symlink() {
        return refuse("is a symlink");
    }
    if !meta.is_dir() {
        return refuse("is not a directory");
    }
    if meta.uid() != rustix::process::geteuid().as_raw() {
        return refuse("belongs to another user");
    }
    if meta.mode() & 0o777 != 0o700 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(crate::Error::io_at(dir))?;
    }
    Ok(())
}

/// Creates `dir` for SQLite's temporary files and removes the files a previous run left
/// there; the caller then points [`SQLITE_TMPDIR`] at it before any connection opens.
///
/// # Errors
///
/// [`Error::File`] naming `dir` when it cannot be created or listed.
///
/// ```
/// let base = tempfile::tempdir().unwrap();
/// let dir = base.path().join("sqlite-tmp");
/// mistarr_server::db::tempdir::prepare_temp_dir(&dir).unwrap();
/// assert!(dir.is_dir());
/// ```
pub fn prepare_temp_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).map_err(crate::Error::io_at(dir))?;
    for entry in std::fs::read_dir(dir)
        .map_err(crate::Error::io_at(dir))?
        .flatten()
    {
        // The frozen client's record outlives a restart so the client is resumed.
        if entry.file_name() == crate::freeze::FROZEN_NAME {
            continue;
        }
        if entry.file_type().is_ok_and(|t| t.is_file()) {
            if let Err(e) = std::fs::remove_file(entry.path()) {
                tracing::warn!(error = %e, "cannot remove a stale SQLite temporary file");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_files_go_to_ram_when_it_can_be_written() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let disk = dir.path().join("data/tmp");
        let ram = dir.path().join("ram");
        let chosen = choose_temp_dir(&ram, &disk).expect("ram");
        assert_eq!(chosen.dir, ram);
        let mode = std::fs::metadata(&ram).expect("ram").permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        std::fs::write(dir.path().join("file"), b"x").expect("write");
        let blocked = dir.path().join("file/sub");
        let chosen = choose_temp_dir(&blocked, &disk).expect("disk");
        assert_eq!(chosen.dir, disk);
        assert!(chosen.refused.is_some());
        assert!(disk.is_dir());
    }

    #[test]
    fn a_temp_dir_that_is_a_symlink_or_open_to_others_is_refused_or_narrowed() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let disk = dir.path().join("data/tmp");
        let target = dir.path().join("elsewhere");
        std::fs::create_dir(&target).expect("mkdir");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        let chosen = choose_temp_dir(&link, &disk).expect("disk");
        assert_eq!(chosen.dir, disk);
        let why = chosen.refused.expect("refused").to_string();
        assert!(why.contains("is a symlink"), "{why}");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o777)).expect("chmod");
        assert_eq!(choose_temp_dir(&target, &disk).expect("ram").dir, target);
        let mode = std::fs::metadata(&target)
            .expect("meta")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
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
}
