//! Applies placement steps within staging and `games/`; see `docs/PLATFORMS.md`
//! "Adapter contract" and `docs/ARCHITECTURE.md` "Import".

use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Component, Path, PathBuf};

use mistarr_mister::{ByteOrder, Step};

/// Streaming buffer, the hashing budget of `docs/ARCHITECTURE.md`.
const BUF_SIZE: usize = 256 * 1024;

/// `EXDEV`, which `rename(2)` returns across filesystems.
const EXDEV: i32 = 18;

/// Why a step was not applied.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PlaceError {
    /// A step named a path that is absolute, climbs with `..`, or leaves its root.
    #[error("`{0}` is outside the staging and games directories")]
    Outside(PathBuf),
    /// Staging and `games/` are on different filesystems, so a move would be a copy.
    #[error(
        "staging `{staging}` and games `{games}` are on different filesystems; files are moved, never copied"
    )]
    CrossDevice {
        /// The staging side.
        staging: PathBuf,
        /// The games side.
        games: PathBuf,
    },
    /// A target the step would create already exists.
    #[error("`{0}` already exists")]
    Exists(PathBuf),
    /// Reading or writing a file failed.
    #[error("`{path}`: {source}")]
    Io {
        /// The file.
        path: PathBuf,
        /// The failure.
        source: io::Error,
    },
    /// The staged archive could not be read or a zip could not be written.
    #[error("`{path}`: {message}")]
    Zip {
        /// The archive.
        path: PathBuf,
        /// The failure.
        message: String,
    },
}

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> PlaceError + '_ {
    move |source| PlaceError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// The directories a plan may touch. Staging paths are read from `scratch`
/// when a step already wrote them there, else from the directory holding the
/// staged item, and are always written to `scratch`, so originals are never
/// changed before the final renames. Library paths resolve against `games`.
#[derive(Debug, Clone)]
pub struct Roots {
    staging: PathBuf,
    work: PathBuf,
    staged: PathBuf,
    scratch: PathBuf,
    games: PathBuf,
}

/// True when `rel` is non-empty and made only of plain names.
fn is_plain(rel: &Path) -> bool {
    rel.components().next().is_some() && rel.components().all(|c| matches!(c, Component::Normal(_)))
}

impl Roots {
    /// Roots for a staged item under `staging`, with an empty scratch directory
    /// `scratch` inside staging; creates `games` and `scratch` when missing.
    ///
    /// # Errors
    ///
    /// [`PlaceError::Outside`] when `staged` or `scratch` is not inside
    /// `staging`, [`PlaceError::Io`] when a directory cannot be resolved.
    pub fn new(
        staging: &Path,
        staged: &Path,
        scratch: &Path,
        games: &Path,
    ) -> Result<Self, PlaceError> {
        fs::create_dir_all(games).map_err(io_err(games))?;
        let staging = staging.canonicalize().map_err(io_err(staging))?;
        let games = games.canonicalize().map_err(io_err(games))?;
        let inside = |p: &Path| p.starts_with(&staging) && !p.starts_with(&games);
        let name = staged
            .file_name()
            .ok_or_else(|| PlaceError::Outside(staged.to_path_buf()))?;
        let parent = staged
            .parent()
            .ok_or_else(|| PlaceError::Outside(staged.to_path_buf()))?;
        let work = parent.canonicalize().map_err(io_err(parent))?;
        if !inside(&work) {
            return Err(PlaceError::Outside(staged.to_path_buf()));
        }
        fs::create_dir_all(scratch).map_err(io_err(scratch))?;
        let scratch = scratch.canonicalize().map_err(io_err(scratch))?;
        if !inside(&scratch) || scratch == staging || work.starts_with(&scratch) {
            return Err(PlaceError::Outside(scratch));
        }
        let staged = work.join(name);
        Ok(Self {
            staging,
            work,
            staged,
            scratch,
            games,
        })
    }

    /// The staged item's absolute path.
    #[must_use]
    pub fn staged(&self) -> &Path {
        &self.staged
    }

    /// The scratch directory steps write to.
    #[must_use]
    pub fn scratch(&self) -> &Path {
        &self.scratch
    }

    /// Where a staging path is read from: its scratch copy when a step wrote
    /// one, else the original beside the staged item.
    ///
    /// # Errors
    ///
    /// [`PlaceError::Outside`] unless `rel` is a plain relative path.
    pub fn read(&self, rel: &Path) -> Result<PathBuf, PlaceError> {
        if !is_plain(rel) {
            return Err(PlaceError::Outside(rel.to_path_buf()));
        }
        let written = self.scratch.join(rel);
        Ok(if written.exists() {
            written
        } else {
            self.work.join(rel)
        })
    }

    /// Where a step writes a staging path, creating its parent in scratch.
    ///
    /// # Errors
    ///
    /// [`PlaceError::Outside`] unless `rel` is a plain relative path,
    /// [`PlaceError::Io`] when the parent cannot be created.
    pub fn write(&self, rel: &Path) -> Result<PathBuf, PlaceError> {
        if !is_plain(rel) {
            return Err(PlaceError::Outside(rel.to_path_buf()));
        }
        let path = self.scratch.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(io_err(parent))?;
        }
        Ok(path)
    }

    /// Resolves a library path.
    ///
    /// # Errors
    ///
    /// [`PlaceError::Outside`] unless `rel` is a plain relative path.
    pub fn library(&self, rel: &Path) -> Result<PathBuf, PlaceError> {
        library_path(&self.games, rel)
    }

    /// Fails unless staging and `games/` share a filesystem, so every rename is a move.
    ///
    /// # Errors
    ///
    /// [`PlaceError::CrossDevice`] naming both paths, [`PlaceError::Io`] when
    /// either cannot be read.
    pub fn check_same_filesystem(&self) -> Result<(), PlaceError> {
        if same_filesystem(&self.work, &self.games)? {
            Ok(())
        } else {
            Err(self.cross_device())
        }
    }

    fn cross_device(&self) -> PlaceError {
        PlaceError::CrossDevice {
            staging: self.staging.clone(),
            games: self.games.clone(),
        }
    }

    /// Removes a staged file or directory that will not be placed; a missing one is fine.
    ///
    /// # Errors
    ///
    /// [`PlaceError::Outside`] when `path` is not inside staging, [`PlaceError::Io`]
    /// when it cannot be removed.
    pub fn discard(&self, path: &Path) -> Result<(), PlaceError> {
        let Some(parent) = path.parent().and_then(|p| p.canonicalize().ok()) else {
            return Ok(());
        };
        if !parent.starts_with(&self.staging) || parent.starts_with(&self.games) {
            return Err(PlaceError::Outside(path.to_path_buf()));
        }
        let result = if path.is_dir() {
            fs::remove_dir_all(path)
        } else {
            fs::remove_file(path)
        };
        match result {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(io_err(path)(e)),
            _ => Ok(()),
        }
    }

    /// Removes the scratch directory and everything the steps wrote there.
    pub fn rollback(&self) {
        let _ = fs::remove_dir_all(&self.scratch);
    }
}

fn library_path(games: &Path, rel: &Path) -> Result<PathBuf, PlaceError> {
    if is_plain(rel) {
        Ok(games.join(rel))
    } else {
        Err(PlaceError::Outside(rel.to_path_buf()))
    }
}

/// Whether two existing paths are on the same filesystem.
///
/// # Errors
///
/// [`PlaceError::Io`] when either cannot be read.
pub fn same_filesystem(a: &Path, b: &Path) -> Result<bool, PlaceError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let da = fs::metadata(a).map_err(io_err(a))?.dev();
        let db = fs::metadata(b).map_err(io_err(b))?.dev();
        Ok(da == db)
    }
    #[cfg(not(unix))]
    {
        fs::metadata(a).map_err(io_err(a))?;
        fs::metadata(b).map_err(io_err(b))?;
        Ok(true)
    }
}

/// True for the steps that touch `games/`: they run last, in [`commit`].
#[must_use]
pub fn is_library_step(step: &Step) -> bool {
    matches!(step, Step::CreateDir { .. } | Step::Rename { .. })
}

/// Applies every staging step in order, writing only to scratch.
///
/// # Errors
///
/// The failing step's [`PlaceError`]; the caller rolls back.
pub fn prepare(steps: &[Step], roots: &Roots) -> Result<(), PlaceError> {
    steps
        .iter()
        .filter(|s| !is_library_step(s))
        .try_for_each(|s| apply_step(s, roots))
}

/// The library paths a [`commit`] moved into place before it stopped.
#[derive(Debug)]
pub struct Partial {
    /// Rename targets that landed, as library paths.
    pub landed: Vec<PathBuf>,
    /// Why the next step failed.
    pub error: PlaceError,
}

/// Applies the library steps in order and returns the rename targets that landed.
///
/// # Errors
///
/// [`Partial`] with the targets that landed before the failing step.
pub fn commit(steps: &[Step], roots: &Roots) -> Result<Vec<PathBuf>, Partial> {
    let mut landed = Vec::new();
    for step in steps.iter().filter(|s| is_library_step(s)) {
        if let Err(error) = apply_step(step, roots) {
            return Err(Partial { landed, error });
        }
        if let Step::Rename { to, .. } = step {
            landed.push(to.clone());
        }
    }
    Ok(landed)
}

/// [`prepare`] then [`commit`], rolling back scratch when preparing fails.
///
/// # Errors
///
/// The first failure; renames that landed stay in place.
pub fn apply(steps: &[Step], roots: &Roots) -> Result<(), PlaceError> {
    if let Err(e) = prepare(steps, roots) {
        roots.rollback();
        return Err(e);
    }
    commit(steps, roots).map(drop).map_err(|p| p.error)
}

/// Applies one of the seven permitted steps.
///
/// # Errors
///
/// [`PlaceError::Outside`] for a path outside its root, [`PlaceError::CrossDevice`]
/// for a rename across filesystems, else the I/O or archive failure.
pub fn apply_step(step: &Step, roots: &Roots) -> Result<(), PlaceError> {
    match step {
        Step::Unzip { member, to } => unzip(roots.staged(), member, &roots.write(to)?),
        Step::Zip { from, to } => zip_dir(&roots.read(from)?, &roots.write(to)?),
        Step::AddHeader { file, bytes } => rewrite(roots, file, |r, w| {
            w.write_all(bytes)?;
            io::copy(r, w).map(drop)
        }),
        Step::StripHeader { file, len } => rewrite(roots, file, |r, w| {
            let skipped = io::copy(&mut r.take(*len), &mut io::sink())?;
            if skipped < *len {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "file is shorter than its header",
                ));
            }
            io::copy(r, w).map(drop)
        }),
        Step::SwapByteOrder { file, from } => rewrite(roots, file, |r, w| swap_order(r, w, *from)),
        Step::CreateDir { path } => {
            let dir = roots.library(path)?;
            fs::create_dir_all(&dir).map_err(io_err(&dir))
        }
        Step::Rename { from, to } => {
            let (src, dst) = (roots.read(from)?, roots.library(to)?);
            move_path(&src, &dst).map_err(|e| match e {
                PlaceError::CrossDevice { .. } => roots.cross_device(),
                other => other,
            })
        }
    }
}

/// Renames `src` to `dst`, creating `dst`'s parent. Never copies.
fn move_path(src: &Path, dst: &Path) -> Result<(), PlaceError> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).map_err(io_err(parent))?;
    }
    fs::rename(src, dst).map_err(|e| {
        if e.raw_os_error() == Some(EXDEV) {
            PlaceError::CrossDevice {
                staging: src.to_path_buf(),
                games: dst.to_path_buf(),
            }
        } else {
            io_err(src)(e)
        }
    })
}

/// Renames a file already in `games/` from one library path to another.
///
/// # Errors
///
/// [`PlaceError::Outside`] for a path outside `games`, [`PlaceError::Exists`]
/// when the target is taken, else the I/O failure.
pub fn rename_in_library(games: &Path, from: &Path, to: &Path) -> Result<PathBuf, PlaceError> {
    let (src, dst) = (library_path(games, from)?, library_path(games, to)?);
    if dst.exists() {
        return Err(PlaceError::Exists(dst));
    }
    move_path(&src, &dst)?;
    Ok(dst)
}

/// Removes `dir` and every directory under it that is empty, deepest first.
pub fn remove_empty_dirs(dir: &Path) {
    if let Ok(entries) = fs::read_dir(dir) {
        for e in entries.flatten() {
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                remove_empty_dirs(&e.path());
            }
        }
    }
    // Fails, and is meant to, while anything is left inside.
    let _ = fs::remove_dir(dir);
}

fn unzip(archive: &Path, member: &str, to: &Path) -> Result<(), PlaceError> {
    let zip_err = |e: zip::result::ZipError| PlaceError::Zip {
        path: archive.to_path_buf(),
        message: e.to_string(),
    };
    let file = File::open(archive).map_err(io_err(archive))?;
    let mut zip = zip::ZipArchive::new(BufReader::new(file)).map_err(zip_err)?;
    let mut entry = zip.by_name(member).map_err(zip_err)?;
    if to.exists() {
        return Err(PlaceError::Exists(to.to_path_buf()));
    }
    let out = File::create(to).map_err(io_err(to))?;
    let mut w = BufWriter::with_capacity(BUF_SIZE, out);
    let copied = io::copy(&mut entry, &mut w).and_then(|_| w.flush());
    if let Err(e) = copied {
        let _ = fs::remove_file(to);
        return Err(io_err(to)(e));
    }
    Ok(())
}

fn zip_dir(from: &Path, to: &Path) -> Result<(), PlaceError> {
    let zip_err = |e: zip::result::ZipError| PlaceError::Zip {
        path: to.to_path_buf(),
        message: e.to_string(),
    };
    let mut files = Vec::new();
    collect_files(from, from, &mut files)?;
    files.sort();
    if to.exists() {
        return Err(PlaceError::Exists(to.to_path_buf()));
    }
    let out = File::create(to).map_err(io_err(to))?;
    let mut zip = zip::ZipWriter::new(BufWriter::with_capacity(BUF_SIZE, out));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, path) in files {
        zip.start_file(name, options).map_err(zip_err)?;
        let mut f = File::open(&path).map_err(io_err(&path))?;
        io::copy(&mut f, &mut zip).map_err(io_err(&path))?;
    }
    zip.finish().map_err(zip_err)?.flush().map_err(io_err(to))
}

/// Lists the files under `dir` as `(member name with '/', path)`.
fn collect_files(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(String, PathBuf)>,
) -> Result<(), PlaceError> {
    for e in fs::read_dir(dir).map_err(io_err(dir))? {
        let path = e.map_err(io_err(dir))?.path();
        if path.is_dir() {
            collect_files(root, &path, out)?;
        } else if let Ok(rel) = path.strip_prefix(root) {
            let name: Vec<String> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            out.push((name.join("/"), path));
        }
    }
    Ok(())
}

/// Streams staging file `rel` through `f` into its scratch copy.
fn rewrite(
    roots: &Roots,
    rel: &Path,
    f: impl FnOnce(&mut BufReader<File>, &mut BufWriter<File>) -> io::Result<()>,
) -> Result<(), PlaceError> {
    let (src, dst) = (roots.read(rel)?, roots.write(rel)?);
    let name = dst
        .file_name()
        .ok_or_else(|| PlaceError::Outside(rel.to_path_buf()))?;
    let tmp = dst.with_file_name(format!(".{}.part", name.to_string_lossy()));
    let input = File::open(&src).map_err(io_err(&src))?;
    let mut r = BufReader::with_capacity(BUF_SIZE, input);
    let output = File::create(&tmp).map_err(io_err(&tmp))?;
    let mut w = BufWriter::with_capacity(BUF_SIZE, output);
    let written = f(&mut r, &mut w)
        .and_then(|()| w.flush())
        .and_then(|()| fs::rename(&tmp, &dst));
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(io_err(&src)(e));
    }
    Ok(())
}

/// Streams `r` to `w` in big-endian order; a trailing partial word is copied as is.
fn swap_order(r: &mut impl Read, w: &mut impl Write, from: ByteOrder) -> io::Result<()> {
    let group = match from {
        ByteOrder::BigEndian => return io::copy(r, w).map(drop),
        ByteOrder::ByteSwapped => 2,
        ByteOrder::LittleEndian => 4,
    };
    let mut buf = vec![0u8; BUF_SIZE];
    let mut filled = 0;
    loop {
        let n = r.read(&mut buf[filled..])?;
        filled += n;
        if n == 0 || filled == buf.len() {
            let usable = filled - filled % group;
            for chunk in buf[..usable].chunks_exact_mut(group) {
                chunk.reverse();
            }
            let tail = if n == 0 { filled } else { usable };
            w.write_all(&buf[..tail])?;
            if n == 0 {
                return Ok(());
            }
            buf.copy_within(usable..filled, 0);
            filled -= usable;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    struct Tree {
        _dir: tempfile::TempDir,
        staging: PathBuf,
        games: PathBuf,
        item: PathBuf,
    }

    /// `staging/<hash>/<name>` holding `data`, and an empty `games/`.
    fn tree(name: &str, data: &[u8]) -> Tree {
        let dir = tempfile::tempdir().expect("tempdir");
        let staging = dir.path().join("staging");
        let games = dir.path().join("games");
        let item = staging.join("0a0a").join(name);
        fs::create_dir_all(item.parent().expect("parent")).expect("mkdir");
        fs::write(&item, data).expect("write");
        Tree {
            _dir: dir,
            staging,
            games,
            item,
        }
    }

    fn roots(t: &Tree) -> Roots {
        let scratch = t.staging.join(".import/1");
        Roots::new(&t.staging, &t.item, &scratch, &t.games).expect("roots")
    }

    fn read(p: &Path) -> Vec<u8> {
        fs::read(p).expect("read")
    }

    #[test]
    fn unzip_extracts_one_member_into_scratch() {
        let mut buf = Vec::new();
        let mut z = zip::ZipWriter::new(Cursor::new(&mut buf));
        z.start_file("inner/a.bin", zip::write::SimpleFileOptions::default())
            .expect("start");
        z.write_all(b"payload").expect("write");
        z.finish().expect("finish");
        let t = tree("set.zip", &buf);
        let r = roots(&t);
        let step = Step::Unzip {
            member: "inner/a.bin".into(),
            to: "a.bin".into(),
        };
        apply_step(&step, &r).expect("unzip");
        assert_eq!(read(&r.scratch().join("a.bin")), b"payload");
        assert_eq!(
            r.read(Path::new("a.bin")).expect("read"),
            r.scratch().join("a.bin")
        );
        assert!(matches!(apply_step(&step, &r), Err(PlaceError::Exists(_))));
        let missing = Step::Unzip {
            member: "nope".into(),
            to: "b.bin".into(),
        };
        assert!(matches!(
            apply_step(&missing, &r),
            Err(PlaceError::Zip { .. })
        ));
    }

    #[test]
    fn zip_packs_a_staging_directory() {
        let t = tree("x.bin", b"x");
        let r = roots(&t);
        let set = t.item.with_file_name("set");
        fs::create_dir_all(set.join("sub")).expect("mkdir");
        fs::write(set.join("a.rom"), b"aa").expect("write");
        fs::write(set.join("sub/b.rom"), b"bbb").expect("write");
        let step = Step::Zip {
            from: "set".into(),
            to: "set.zip".into(),
        };
        apply_step(&step, &r).expect("zip");
        let file = File::open(r.scratch().join("set.zip")).expect("open");
        let members = mistarr_core::hash::zip_members(file).expect("members");
        let names: Vec<_> = members.iter().map(|m| (m.name.as_str(), m.size)).collect();
        assert_eq!(names, [("a.rom", 2), ("sub/b.rom", 3)]);
    }

    #[test]
    fn header_steps_write_to_scratch_and_leave_the_original() {
        let t = tree("g.bin", b"body");
        let r = roots(&t);
        let add = Step::AddHeader {
            file: "g.bin".into(),
            bytes: b"HEAD".to_vec(),
        };
        apply_step(&add, &r).expect("add");
        assert_eq!(read(&t.item), b"body");
        assert_eq!(read(&r.scratch().join("g.bin")), b"HEADbody");
        let strip = Step::StripHeader {
            file: "g.bin".into(),
            len: 4,
        };
        apply_step(&strip, &r).expect("strip");
        assert_eq!(read(&r.scratch().join("g.bin")), b"body");
        let too_long = Step::StripHeader {
            file: "g.bin".into(),
            len: 99,
        };
        assert!(apply_step(&too_long, &r).is_err());
        assert_eq!(fs::read_dir(r.scratch()).expect("list").count(), 1);
    }

    #[test]
    fn swap_byte_order_normalises_to_big_endian() {
        let t = tree("g.v64", &[0x37, 0x80, 0x40, 0x12, 1, 2, 9]);
        let r = roots(&t);
        let step = Step::SwapByteOrder {
            file: "g.v64".into(),
            from: ByteOrder::ByteSwapped,
        };
        apply_step(&step, &r).expect("swap");
        assert_eq!(
            read(&r.scratch().join("g.v64")),
            [0x80, 0x37, 0x12, 0x40, 2, 1, 9]
        );
        let mut out = Vec::new();
        swap_order(
            &mut Cursor::new([0x40, 0x12, 0x37, 0x80]),
            &mut out,
            ByteOrder::LittleEndian,
        )
        .expect("swap");
        assert_eq!(out, [0x80, 0x37, 0x12, 0x40]);
    }

    #[test]
    fn swap_spans_buffer_boundaries() {
        let data: Vec<u8> = (0..BUF_SIZE * 2 + 6)
            .map(|i| u8::try_from(i % 251).expect("below 256"))
            .collect();
        let mut out = Vec::new();
        swap_order(&mut Cursor::new(&data), &mut out, ByteOrder::ByteSwapped).expect("swap");
        assert_eq!(out.len(), data.len());
        assert!(out
            .chunks(2)
            .zip(data.chunks(2))
            .all(|(a, b)| a[0] == b[1] && a[1] == b[0]));
    }

    #[test]
    fn create_dir_and_rename_move_into_games() {
        let t = tree("t.bin", b"track");
        let r = roots(&t);
        r.check_same_filesystem().expect("same fs");
        apply(
            &[
                Step::CreateDir {
                    path: "PSX/Example Quest (USA)".into(),
                },
                Step::Rename {
                    from: "t.bin".into(),
                    to: "PSX/Example Quest (USA)/t.bin".into(),
                },
            ],
            &r,
        )
        .expect("apply");
        assert!(!t.item.exists());
        assert_eq!(
            read(&t.games.join("PSX/Example Quest (USA)/t.bin")),
            b"track"
        );
    }

    #[test]
    fn a_failed_prepare_rolls_back_and_touches_nothing() {
        let t = tree("g.nes", b"body");
        let r = roots(&t);
        let steps = [
            Step::AddHeader {
                file: "g.nes".into(),
                bytes: b"HEAD".to_vec(),
            },
            Step::StripHeader {
                file: "missing.nes".into(),
                len: 1,
            },
            Step::Rename {
                from: "g.nes".into(),
                to: "NES/g.nes".into(),
            },
        ];
        assert!(apply(&steps, &r).is_err());
        assert_eq!(read(&t.item), b"body");
        assert!(!r.scratch().exists());
        assert!(!t.games.join("NES").exists());
    }

    #[test]
    fn commit_reports_the_renames_that_landed() {
        let t = tree("a.bin", b"a");
        let r = roots(&t);
        let steps = [
            Step::Rename {
                from: "a.bin".into(),
                to: "PSX/Set/a.bin".into(),
            },
            Step::Rename {
                from: "b.bin".into(),
                to: "PSX/Set/b.bin".into(),
            },
        ];
        prepare(&steps, &r).expect("prepare");
        let partial = commit(&steps, &r).expect_err("b.bin is missing");
        assert_eq!(partial.landed, [PathBuf::from("PSX/Set/a.bin")]);
        assert!(matches!(partial.error, PlaceError::Io { .. }));
        assert!(t.games.join("PSX/Set/a.bin").is_file());
        assert!(is_library_step(&steps[0]));
    }

    #[test]
    fn paths_outside_staging_or_games_are_refused() {
        let t = tree("t.bin", b"x");
        let r = roots(&t);
        for bad in ["../t.bin", "/etc/passwd", "", "a/../../b"] {
            let step = Step::Rename {
                from: bad.into(),
                to: "NES/t.bin".into(),
            };
            assert!(
                matches!(apply_step(&step, &r), Err(PlaceError::Outside(_))),
                "{bad}"
            );
            let step = Step::CreateDir { path: bad.into() };
            assert!(
                matches!(apply_step(&step, &r), Err(PlaceError::Outside(_))),
                "{bad}"
            );
            let step = Step::AddHeader {
                file: bad.into(),
                bytes: vec![1],
            };
            assert!(
                matches!(apply_step(&step, &r), Err(PlaceError::Outside(_))),
                "{bad}"
            );
        }
        let elsewhere = t.games.join("x.bin");
        fs::create_dir_all(&t.games).expect("mkdir");
        fs::write(&elsewhere, b"x").expect("write");
        let scratch = t.staging.join(".import/2");
        assert!(matches!(
            Roots::new(&t.staging, &elsewhere, &scratch, &t.games),
            Err(PlaceError::Outside(_))
        ));
        assert!(matches!(
            Roots::new(&t.staging, &t.item, &t.games.join("s"), &t.games),
            Err(PlaceError::Outside(_))
        ));
        assert!(matches!(r.discard(&elsewhere), Err(PlaceError::Outside(_))));
        r.discard(&t.item).expect("discard");
        assert!(!t.item.exists());
        r.discard(&t.item).expect("already gone");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn different_filesystems_are_detected() {
        let t = tree("t.bin", b"x");
        assert!(same_filesystem(&t.staging, &t.item).expect("same"));
        assert!(!same_filesystem(Path::new("/proc"), &t.staging).expect("differ"));
    }

    #[test]
    fn library_renames_and_empty_dirs() {
        let t = tree("t.bin", b"x");
        let games = t.games.clone();
        fs::create_dir_all(games.join("NES")).expect("mkdir");
        fs::write(games.join("NES/old.nes"), b"x").expect("write");
        fs::write(games.join("NES/taken.nes"), b"y").expect("write");
        let dst = rename_in_library(&games, Path::new("NES/old.nes"), Path::new("NES/new.nes"))
            .expect("rename");
        assert_eq!(read(&dst), b"x");
        assert!(matches!(
            rename_in_library(&games, Path::new("NES/new.nes"), Path::new("NES/taken.nes")),
            Err(PlaceError::Exists(_))
        ));
        assert!(rename_in_library(&games, Path::new("NES/new.nes"), Path::new("../x")).is_err());
        let hash_dir = t.staging.join("0a0a");
        fs::create_dir_all(hash_dir.join("a/b")).expect("mkdir");
        remove_empty_dirs(&hash_dir);
        assert!(hash_dir.exists(), "still holds t.bin");
        fs::remove_file(&t.item).expect("rm");
        remove_empty_dirs(&hash_dir);
        assert!(!hash_dir.exists());
    }
}
