//! Placing an uploaded or fetched file in a watched directory under a free name, then
//! queuing its import; see `docs/API.md` "Incoming files".

use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use mistarr_core::InfoHash;
use mistarr_sources::intake::candidates;

use super::{queue_placed, IncomingFile};
use crate::app::AppState;
use crate::db::sources::{self as rows, SourceState};
use crate::error::Result;
use crate::jobs::dat_import::DatImport;
use crate::jobs::source_import::{SourceImport, DUPLICATE};
use crate::jobs::JobKind;

/// Prefix of the temporary files a placement writes; the watchers skip them as hidden.
pub const PART_PREFIX: &str = ".upload-";

/// Why a file was not placed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PlaceError {
    /// The file repeats a source that is already loaded.
    #[error("{DUPLICATE}")]
    Duplicate,
    /// Every name the file could take is in use.
    #[error("Too many files with this name are waiting in the directory.")]
    NoFreeName,
    /// The file could not be written or moved.
    #[error("The file could not be placed at {}: {source}.", path.display())]
    Io {
        /// The directory, the claimed name or the file being moved.
        path: PathBuf,
        /// The failure.
        source: io::Error,
    },
}

impl PlaceError {
    /// Wraps an I/O failure on `path`.
    fn io_at(path: &Path) -> impl FnOnce(io::Error) -> Self + '_ {
        move |source| Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

/// A `.torrent` or `.magnet` file on its way into `sources/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceFile {
    /// The safe file name to place it under.
    pub name: String,
    /// Its contents.
    pub bytes: Vec<u8>,
    /// The torrent's infohash.
    pub infohash: InfoHash,
    /// A `.torrent`, which may complete a magnet that is still resolving.
    pub is_torrent: bool,
}

/// A temporary name in `dir`, unique in this process, that the watchers ignore.
///
/// ```
/// use mistarr_server::incoming::place::{part_path, PART_PREFIX};
/// let p = part_path(std::path::Path::new("/d"));
/// assert!(p.file_name().unwrap().to_string_lossy().starts_with(PART_PREFIX));
/// assert_ne!(p, part_path(std::path::Path::new("/d")));
/// ```
#[must_use]
pub fn part_path(dir: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    dir.join(format!("{PART_PREFIX}{}-{n}.part", std::process::id()))
}

/// Claims `name` in `dir`, or the first free `name (N)`, with `create_new`, so no file is
/// ever replaced, then has `write` fill the claimed path; the claim is dropped when it fails.
///
/// # Errors
///
/// [`PlaceError::NoFreeName`] when every candidate name is taken, [`PlaceError::Io`]
/// when `dir` cannot be made, a name cannot be claimed or `write` fails.
///
/// ```
/// use mistarr_server::incoming::place::place_unique;
/// let dir = tempfile::tempdir().unwrap();
/// let a = place_unique(dir.path(), "a.dat", |p| std::fs::write(p, b"1")).unwrap();
/// let b = place_unique(dir.path(), "a.dat", |p| std::fs::write(p, b"2")).unwrap();
/// assert!(a.ends_with("a.dat") && b.ends_with("a (1).dat"));
/// ```
pub fn place_unique(
    dir: &Path,
    name: &str,
    write: impl FnOnce(&Path) -> io::Result<()>,
) -> Result<PathBuf, PlaceError> {
    std::fs::create_dir_all(dir).map_err(PlaceError::io_at(dir))?;
    for target in candidates(dir, name.as_ref()) {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
        {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(PlaceError::io_at(&target)(e)),
        }
        return match write(&target) {
            Ok(()) => Ok(target),
            Err(e) => {
                let _ = std::fs::remove_file(&target);
                Err(PlaceError::io_at(&target)(e))
            }
        };
    }
    Err(PlaceError::NoFreeName)
}

/// Writes `bytes` in `dir` under `name` or a free variant: a [`part_path`] is filled and
/// renamed over the claimed name, so concurrent placements never share a path.
///
/// # Errors
///
/// As [`place_unique`].
///
/// ```
/// let dir = tempfile::tempdir().unwrap();
/// let p = mistarr_server::incoming::place::place(dir.path(), "s.torrent", b"d1:ae").unwrap();
/// assert_eq!(std::fs::read(p).unwrap(), b"d1:ae");
/// ```
pub fn place(dir: &Path, name: &str, bytes: &[u8]) -> Result<PathBuf, PlaceError> {
    place_unique(dir, name, |target| {
        let part = part_path(dir);
        let written = std::fs::write(&part, bytes).and_then(|()| std::fs::rename(&part, target));
        if written.is_err() {
            let _ = std::fs::remove_file(&part);
        }
        written
    })
}

/// Moves `from` into `dir` under `name` or a free variant: a rename, or a copy when `from`
/// sits on another file system.
///
/// # Errors
///
/// As [`place_unique`]; [`PlaceError::Io`] with `NotFound` when `from` is gone.
///
/// ```
/// let dir = tempfile::tempdir().unwrap();
/// let from = dir.path().join("x.part");
/// std::fs::write(&from, b"x").unwrap();
/// let p = mistarr_server::incoming::place::place_moved(&from, dir.path(), "a.dat").unwrap();
/// assert!(p.ends_with("a.dat") && !from.exists());
/// ```
pub fn place_moved(from: &Path, dir: &Path, name: &str) -> Result<PathBuf, PlaceError> {
    if !from.exists() {
        return Err(PlaceError::io_at(from)(io::ErrorKind::NotFound.into()));
    }
    place_unique(dir, name, |target| match std::fs::rename(from, target) {
        Err(e) if e.kind() == io::ErrorKind::CrossesDevices => copy_into(from, dir, target),
        moved => moved,
    })
}

/// Copies `from` through a [`part_path`] of `dir` onto `target`, then removes `from`.
fn copy_into(from: &Path, dir: &Path, target: &Path) -> io::Result<()> {
    let part = part_path(dir);
    let copied = std::fs::copy(from, &part)
        .and_then(|_| std::fs::File::open(&part)?.sync_all())
        .and_then(|()| std::fs::rename(&part, target));
    if copied.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    copied?;
    std::fs::remove_file(from)
}

/// A safe basename ending in `.ext` from a user-supplied name, else `fallback.ext`.
///
/// ```
/// use mistarr_server::incoming::place::file_name;
/// assert_eq!(file_name("../x/Set.TORRENT", "u", "torrent"), "Set.torrent");
/// assert_eq!(file_name("", "abc", "magnet"), "abc.magnet");
/// ```
#[must_use]
pub fn file_name(given: &str, fallback: &str, ext: &str) -> String {
    let base = given.rsplit(['/', '\\']).next().unwrap_or("");
    let suffix = format!(".{ext}");
    let stem = if base.len() > suffix.len() && base.to_ascii_lowercase().ends_with(&suffix) {
        &base[..base.len() - suffix.len()]
    } else {
        base
    };
    let clean: String = stem
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " -_.,()[]+'".contains(c) {
                c
            } else {
                '_'
            }
        })
        .take(120)
        .collect();
    let clean = clean.trim().trim_start_matches('.');
    if clean.is_empty() {
        format!("{fallback}{suffix}")
    } else {
        format!("{clean}{suffix}")
    }
}

/// Places `file` in `sources/` as an upload does and queues its import.
///
/// # Errors
///
/// [`PlaceError::Duplicate`] when it repeats a loaded source, other [`PlaceError`]s as
/// [`place`] has them, and [`crate::Error::Db`] when its job cannot be recorded.
pub(crate) async fn place_source(app: &Arc<AppState>, file: SourceFile) -> Result<IncomingFile> {
    let SourceFile {
        name,
        bytes,
        infohash,
        is_torrent,
    } = file;
    let hex = infohash.to_string();
    let existing = app
        .db
        .read(move |c| rows::find_by_infohash(c, &hex))
        .await?;
    if existing.is_some_and(|r| !is_torrent || r.state != SourceState::Resolving) {
        return Err(PlaceError::Duplicate.into());
    }
    let dir = app.config().paths.sources();
    let path = crate::threads::run(crate::threads::label::SOURCE_FILE, move || {
        place(&dir, &name, &bytes)
    })
    .await??;
    let job = Arc::new(SourceImport { path: path.clone() });
    queue_placed(app, &path, JobKind::SourceImport, job).await
}

/// Moves the finished `part`, a [`part_path`] of `dats/`, to `name` there or a free
/// variant, and queues its import as an upload's; `part` is removed when that fails.
///
/// # Errors
///
/// [`crate::Error::Place`] when the move fails, [`crate::Error::Db`] when the job cannot
/// be recorded.
pub async fn place_part(app: &Arc<AppState>, part: &Path, name: &str) -> Result<IncomingFile> {
    let dir = app.config().paths.dats();
    let (from, name) = (part.to_path_buf(), name.to_owned());
    let target = crate::threads::run(crate::threads::label::DAT_SAVE, move || {
        let moved = place_moved(&from, &dir, &name);
        if moved.is_err() {
            let _ = std::fs::remove_file(&from);
        }
        moved
    })
    .await??;
    let job = Arc::new(DatImport::new(&target));
    queue_placed(app, &target, JobKind::DatImport, job).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::state;
    use crate::Error;

    #[test]
    fn names_are_made_safe() {
        assert_eq!(
            file_name("../../x/Set (A).TORRENT", "u", "torrent"),
            "Set (A).torrent"
        );
        assert_eq!(file_name("a/b:c?.torrent", "u", "torrent"), "b_c_.torrent");
        assert_eq!(file_name("...", "u", "magnet"), "u.magnet");
        assert_eq!(file_name("", "abc", "magnet"), "abc.magnet");
    }

    #[test]
    fn placing_never_overwrites() {
        let dir = tempfile::tempdir().expect("tempdir");
        let a = place(dir.path(), "s.torrent", b"1").expect("place");
        let b = place(dir.path(), "s.torrent", b"2").expect("place");
        assert_eq!(a.file_name().and_then(|n| n.to_str()), Some("s.torrent"));
        assert_eq!(
            b.file_name().and_then(|n| n.to_str()),
            Some("s (1).torrent")
        );
        assert_eq!(std::fs::read(&b).expect("read"), b"2");
        assert_eq!(std::fs::read_dir(dir.path()).expect("dir").count(), 2);
    }

    #[test]
    fn concurrent_placements_keep_every_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let placed: Vec<PathBuf> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..8u8)
                .map(|i| s.spawn(move || place(root, "c.torrent", &[i]).expect("place")))
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("join"))
                .collect()
        });
        let mut contents: Vec<u8> = placed
            .iter()
            .map(|p| std::fs::read(p).expect("read")[0])
            .collect();
        contents.sort_unstable();
        assert_eq!(contents, (0..8).collect::<Vec<u8>>());
        assert_eq!(std::fs::read_dir(dir.path()).expect("dir").count(), 8);
    }

    #[test]
    fn placing_reports_when_no_name_is_free() {
        let dir = tempfile::tempdir().expect("tempdir");
        for target in candidates(dir.path(), "f.magnet".as_ref()) {
            std::fs::write(target, b"x").expect("write");
        }
        let taken = std::fs::read_dir(dir.path()).expect("dir").count();
        let err = place(dir.path(), "f.magnet", b"y").expect_err("full");
        assert!(matches!(err, PlaceError::NoFreeName), "{err:?}");
        assert_eq!(std::fs::read_dir(dir.path()).expect("dir").count(), taken);
    }

    #[test]
    fn a_failed_write_gives_the_name_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = place_unique(dir.path(), "a.dat", |_| Err(io::Error::other("full")))
            .expect_err("refused");
        let target = dir.path().join("a.dat");
        assert!(
            matches!(&err, PlaceError::Io { path, .. } if *path == target),
            "{err:?}"
        );
        assert_eq!(std::fs::read_dir(dir.path()).expect("dir").count(), 0);
    }

    #[test]
    fn a_moved_file_never_replaces_one_of_the_same_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let from = dir.path().join("rejected.dat");
        std::fs::write(&from, b"retried").expect("write");
        std::fs::write(dir.path().join("a.dat"), b"waiting").expect("write");
        let moved = place_moved(&from, dir.path(), "a.dat").expect("move");
        assert!(moved.ends_with("a (1).dat"));
        assert_eq!(std::fs::read(&moved).expect("read"), b"retried");
        assert_eq!(
            std::fs::read(dir.path().join("a.dat")).expect("read"),
            b"waiting"
        );
        assert!(!from.exists());
        let gone = place_moved(&from, dir.path(), "b.dat").expect_err("moved already");
        assert!(matches!(&gone, PlaceError::Io { path, source }
            if *path == from && source.kind() == io::ErrorKind::NotFound));
        assert!(gone.to_string().contains("rejected.dat"), "{gone}");
        assert!(!dir.path().join("b.dat").exists());
        let to = dir.path().join("c.dat");
        std::fs::write(&to, b"").expect("claim");
        copy_into(&moved, dir.path(), &to).expect("copy");
        assert_eq!(std::fs::read(&to).expect("read"), b"retried");
        assert!(!moved.exists());
    }

    #[tokio::test]
    async fn a_source_is_placed_once_and_queued() {
        let (_dir, app) = state();
        let file = SourceFile {
            name: "Set.magnet".into(),
            bytes: b"magnet:?xt=urn:btih:00\n".to_vec(),
            infohash: InfoHash::from_bytes([7; 20]),
            is_torrent: false,
        };
        let placed = place_source(&app, file.clone()).await.expect("placed");
        assert_eq!(placed.file, "Set.magnet");
        let sources = app.config().paths.sources();
        assert!(sources.join("Set.magnet").is_file());
        let again = place_source(&app, file).await.expect("placed again");
        assert_eq!(again.file, "Set (1).magnet");
    }

    #[tokio::test]
    async fn a_loaded_source_is_a_duplicate() {
        let (_dir, app) = state();
        let hex = InfoHash::from_bytes([9; 20]).to_string();
        app.db
            .write(move |c| {
                let row = rows::NewSource {
                    infohash: &hex,
                    display_name: "Set",
                    origin_file: "Set.torrent",
                    state: SourceState::Bound,
                    reason: None,
                    added_at: 0,
                };
                rows::insert(c, &row)
            })
            .await
            .expect("seed");
        let file = SourceFile {
            name: "Set.magnet".into(),
            bytes: Vec::new(),
            infohash: InfoHash::from_bytes([9; 20]),
            is_torrent: false,
        };
        let err = place_source(&app, file).await.expect_err("duplicate");
        assert!(
            matches!(err, Error::Place(PlaceError::Duplicate)),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn a_part_is_moved_into_dats_and_queued() {
        let (_dir, app) = state();
        let dats = app.config().paths.dats();
        std::fs::create_dir_all(&dats).expect("mkdir");
        let part = part_path(&dats);
        std::fs::write(&part, b"<datafile/>").expect("write");
        let placed = place_part(&app, &part, "Set.dat").await.expect("placed");
        assert_eq!(placed.file, "Set.dat");
        assert!(!part.exists() && dats.join("Set.dat").is_file());
        let gone = place_part(&app, &part, "Set.dat")
            .await
            .expect_err("no part");
        let missing = matches!(&gone, Error::Place(PlaceError::Io { path, .. }) if *path == part);
        assert!(missing, "{gone:?}");
    }
}
