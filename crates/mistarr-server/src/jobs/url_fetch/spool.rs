//! The temporary files a fetch writes: in RAM when memory allows, else on the card.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::error::{Error, Result};
use crate::threads::{blocking, label};

/// Bytes gathered before each write, and copied per write onto the card.
pub const CHUNK_BYTES: usize = crate::db::ram::CHUNK_BYTES;

/// Room assumed for a body of unknown length when choosing where it goes.
const UNKNOWN_GUESS: u64 = 16 * 1024 * 1024;

/// Bytes written between checks that memory, or the card's free space, still allows more.
pub const RECHECK_BYTES: u64 = 8 * 1024 * 1024;

/// Bytes a fetch leaves free on the card beyond what it writes, for the database and the core.
pub const CARD_SPARE: u64 = 32 * 1024 * 1024;

/// Prefix of a fetch's temporary files, removed at startup when one is left over.
pub const PART_PREFIX: &str = "fetch-";

/// Where a fetch's files may go and what must stay free if they go to RAM.
#[derive(Debug, Clone)]
pub struct Places {
    /// mistarr's own directory under SQLite's temporary one, when that is in RAM.
    pub ram: Option<PathBuf>,
    /// The directory on the card beside the data, used otherwise.
    pub card: PathBuf,
    /// `MemAvailable` bytes kept free beyond the file, `[memory] import_floor_mib`.
    pub floor: u64,
}

/// The rest after a write to the card, given how long it took; zero unless a core runs.
pub type Pace = Arc<dyn Fn(Duration) -> Duration + Send + Sync>;

/// Whether the work should stop, asked between writes.
pub type Stop = Arc<dyn Fn() -> bool + Send + Sync>;

fn rest(pace: &dyn Fn(Duration) -> Duration, took: Duration) {
    let pause = pace(took);
    if !pause.is_zero() {
        std::thread::sleep(pause);
    }
}

/// Whether memory and the RAM directory's room allow `need` more bytes there.
fn ram_allows(dir: &Path, need: u64, floor: u64) -> bool {
    let available = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|t| crate::db::ram::mem_available(&t));
    let room = rustix::fs::statvfs(dir)
        .ok()
        .and_then(|s| s.f_bavail.checked_mul(s.f_frsize));
    matches!((available, room), (Some(a), Some(r)) if a >= need.saturating_add(floor) && r >= need)
}

/// Whether `room` free bytes hold `need` more and still leave [`CARD_SPARE`]; an unknown
/// room is taken as enough, since the write itself then reports a full card.
fn fits(room: Option<u64>, need: u64) -> bool {
    room.is_none_or(|r| r >= need.saturating_add(CARD_SPARE))
}

/// Whether the card's file system holding `dir` has room for `need` more bytes.
fn card_allows(dir: &Path, need: u64) -> bool {
    let room = rustix::fs::statvfs(dir)
        .ok()
        .and_then(|s| s.f_bavail.checked_mul(s.f_frsize));
    fits(room, need)
}

/// The error of a write the card has no room for.
fn no_room() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::StorageFull,
        "the card has too little free space for the file",
    )
}

/// A temporary file being filled with a fetched body, removed when dropped unless placed.
pub struct Spool {
    path: PathBuf,
    file: Option<File>,
    buf: Vec<u8>,
    written: u64,
    in_ram: bool,
    places: Places,
    checked_at: u64,
    pace: Pace,
}

impl Spool {
    /// Creates the file named for `token`: in RAM when a RAM directory is known and
    /// memory allows `length`, or a guess when it is unknown; else on the card if it has
    /// room, where every write rests for what `pace` returns.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when no file can be created, of kind `StorageFull` when the card
    /// lacks room for `length`.
    pub async fn create(
        places: Places,
        token: u64,
        length: Option<u64>,
        pace: Pace,
    ) -> Result<Self> {
        let expected = length.unwrap_or(UNKNOWN_GUESS);
        blocking(label::FETCH, move || {
            let name = format!("{PART_PREFIX}{}-{token}.part", std::process::id());
            let ram = places
                .ram
                .as_ref()
                .filter(|dir| std::fs::create_dir_all(dir).is_ok())
                .filter(|dir| ram_allows(dir, expected, places.floor));
            let in_ram = ram.is_some();
            let dir = ram.cloned().unwrap_or_else(|| places.card.clone());
            std::fs::create_dir_all(&dir)?;
            if !in_ram && !card_allows(&dir, expected) {
                return Err(no_room().into());
            }
            let path = dir.join(name);
            let file = File::create(&path)?;
            Ok(Self {
                path,
                file: Some(file),
                buf: Vec::with_capacity(CHUNK_BYTES),
                written: 0,
                in_ram,
                places,
                checked_at: 0,
                pace,
            })
        })
        .await
        .map_err(|e| Error::Task(e.to_string()))?
    }

    /// Whether the file is in RAM.
    #[must_use]
    pub fn in_ram(&self) -> bool {
        self.in_ram
    }

    /// The file's path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Adds `bytes`, writing whenever [`CHUNK_BYTES`] have gathered.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when a write fails.
    pub async fn push(&mut self, bytes: &[u8]) -> Result<()> {
        self.buf.extend_from_slice(bytes);
        if self.buf.len() >= CHUNK_BYTES {
            self.flush().await?;
        }
        Ok(())
    }

    /// Writes what has gathered. Every [`RECHECK_BYTES`] a file in RAM moves to the card
    /// when memory or the RAM directory's room falls short, and a file on the card stops
    /// when the card's room does. Writes to the card are paced; the caller's reading waits.
    async fn flush(&mut self) -> Result<()> {
        let buf = std::mem::replace(&mut self.buf, Vec::with_capacity(CHUNK_BYTES));
        let len = buf.len() as u64;
        let mut file = self
            .file
            .take()
            .ok_or_else(|| Error::Fetch("the download was closed".into()))?;
        let recheck = self.written + len >= self.checked_at + RECHECK_BYTES;
        let (path, places) = (self.path.clone(), self.places.clone());
        let (pace, mut on_card, written) = (Arc::clone(&self.pace), !self.in_ram, self.written);
        let moved = blocking(label::FETCH, move || -> Result<(File, Option<PathBuf>)> {
            let mut moved = None;
            if recheck && on_card && !card_allows(&places.card, RECHECK_BYTES) {
                return Err(no_room().into());
            }
            if recheck && !on_card {
                let dir = path.parent().unwrap_or(Path::new("/"));
                if !ram_allows(dir, len.max(CHUNK_BYTES as u64), places.floor) {
                    drop(file);
                    if !card_allows(&places.card, written + RECHECK_BYTES) {
                        return Err(no_room().into());
                    }
                    let to = places.card.join(path.file_name().unwrap_or_default());
                    file = migrate(&path, &to, &places.card, &*pace)?;
                    file.seek(SeekFrom::End(0))?;
                    moved = Some(to);
                    on_card = true;
                }
            }
            let started = Instant::now();
            file.write_all(&buf)?;
            if on_card {
                rest(&*pace, started.elapsed());
            }
            Ok((file, moved))
        })
        .await
        .map_err(|e| Error::Task(e.to_string()))??;
        let (file, moved) = moved;
        if let Some(to) = moved {
            tracing::info!("memory ran short during a fetch; it continues on the card");
            self.path = to;
            self.in_ram = false;
        }
        if recheck {
            self.checked_at = self.written + len;
        }
        self.file = Some(file);
        self.written += len;
        Ok(())
    }

    /// Writes the rest and closes the file; returns its size.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when a write fails.
    pub async fn finish(&mut self) -> Result<u64> {
        if !self.buf.is_empty() {
            self.flush().await?;
        }
        self.file = None;
        Ok(self.written)
    }

    /// Where mistarr's rewrite of this file may go: in RAM only while this file is,
    /// limited to `limit` bytes.
    #[must_use]
    pub fn target(&self, limit: u64) -> Target {
        let stem = self
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        Target {
            places: self.places.clone(),
            name: format!("{stem}-out.part"),
            ram: self.in_ram,
            pace: Arc::clone(&self.pace),
            limit,
        }
    }

    /// Replaces this spool's file with the finished file at `path`, in RAM or not.
    pub fn adopt(&mut self, path: PathBuf, in_ram: bool) {
        self.file = None;
        let _ = std::fs::remove_file(&self.path);
        self.path = path;
        self.in_ram = in_ram;
    }

    /// Moves the finished file to `to`: renamed when it is on the card's file system,
    /// else copied in paced [`CHUNK_BYTES`] writes, asking `stop` between them, and the
    /// original removed. The spool no longer owns it.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when it cannot be moved, of kind `StorageFull` when the card lacks
    /// room for the copy, [`Error::Cancelled`] when `stop` said so; nothing is left at `to` then.
    pub async fn place(mut self, to: PathBuf, stop: Stop) -> Result<()> {
        self.file = None;
        let from = std::mem::take(&mut self.path);
        let (in_ram, pace) = (self.in_ram, Arc::clone(&self.pace));
        blocking(label::FETCH, move || {
            let renamed = !in_ram && std::fs::rename(&from, &to).is_ok();
            if !renamed {
                let size = std::fs::metadata(&from).map_or(0, |m| m.len());
                let dir = to.parent().unwrap_or(Path::new("/"));
                let copied = if card_allows(dir, size) {
                    copy_chunked(&from, &to, &*pace, &*stop)
                } else {
                    Err(no_room())
                };
                let _ = std::fs::remove_file(&from);
                if let Err(e) = copied {
                    let _ = std::fs::remove_file(&to);
                    if e.kind() == std::io::ErrorKind::Interrupted {
                        return Err(Error::Cancelled);
                    }
                    return Err(e.into());
                }
            }
            Ok(())
        })
        .await
        .map_err(|e| Error::Task(e.to_string()))?
    }
}

impl Drop for Spool {
    fn drop(&mut self) {
        self.file = None;
        if !self.path.as_os_str().is_empty() {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Copies the RAM file `from` to `to` in `card` with paced writes, opens `to` for reading
/// and writing and removes `from`; on any failure `to` is removed and `from` kept.
fn migrate(
    from: &Path,
    to: &Path,
    card: &Path,
    pace: &dyn Fn(Duration) -> Duration,
) -> std::io::Result<File> {
    let moved = (|| -> std::io::Result<File> {
        std::fs::create_dir_all(card)?;
        copy_chunked(from, to, pace, &|| false)?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(to)?;
        std::fs::remove_file(from)?;
        Ok(file)
    })();
    moved.inspect_err(|_| {
        let _ = std::fs::remove_file(to);
    })
}

/// Where a [`Spill`] may go.
#[derive(Clone)]
pub struct Target {
    /// The directories and memory floor.
    pub places: Places,
    /// The file name, the same in either directory.
    pub name: String,
    /// Whether the file may start in RAM.
    pub ram: bool,
    /// The rest after each write to the card.
    pub pace: Pace,
    /// Most bytes the file may take.
    pub limit: u64,
}

/// A file mistarr writes from a fetch, readable and seekable: in RAM while memory allows,
/// checked every [`RECHECK_BYTES`], moving to the card when it no longer does; on the card
/// its writes are paced and the card's room is checked as often. Removed when dropped
/// unless finished.
pub struct Spill {
    file: Option<File>,
    path: PathBuf,
    in_ram: bool,
    target: Target,
    written: u64,
    checked_at: u64,
    done: bool,
}

impl Spill {
    /// Creates the file where `target` allows, expecting about `expect` bytes.
    ///
    /// # Errors
    ///
    /// When it cannot be created, of kind `StorageFull` when it would go on a card
    /// without room for `expect`.
    pub fn create(target: Target, expect: u64) -> std::io::Result<Self> {
        let places = &target.places;
        let ram = places
            .ram
            .as_ref()
            .filter(|_| target.ram)
            .filter(|dir| ram_allows(dir, expect, places.floor));
        let in_ram = ram.is_some();
        let dir = ram.cloned().unwrap_or_else(|| places.card.clone());
        std::fs::create_dir_all(&dir)?;
        if !in_ram && !card_allows(&dir, expect) {
            return Err(no_room());
        }
        let path = dir.join(&target.name);
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)?;
        Ok(Self {
            file: Some(file),
            path,
            in_ram,
            target,
            written: 0,
            checked_at: 0,
            done: false,
        })
    }

    /// Flushes the file and hands it over: its path and whether it is in RAM.
    ///
    /// # Errors
    ///
    /// When the flush fails; the file is then removed.
    pub fn finish(mut self) -> std::io::Result<(PathBuf, bool)> {
        if let Some(f) = self.file.as_mut() {
            f.flush()?;
        }
        self.done = true;
        self.file = None;
        Ok((self.path.clone(), self.in_ram))
    }

    fn file(&mut self) -> std::io::Result<&mut File> {
        self.file
            .as_mut()
            .ok_or_else(|| std::io::Error::other("the file was closed"))
    }

    /// Moves the file to the card if memory ran short, or fails if the card's room did.
    fn recheck(&mut self) -> std::io::Result<()> {
        let places = self.target.places.clone();
        if !self.in_ram {
            return if card_allows(&places.card, RECHECK_BYTES) {
                Ok(())
            } else {
                Err(no_room())
            };
        }
        let dir = self.path.parent().unwrap_or(Path::new("/")).to_path_buf();
        if ram_allows(&dir, RECHECK_BYTES, places.floor) {
            return Ok(());
        }
        let mut file = self
            .file
            .take()
            .ok_or_else(|| std::io::Error::other("closed"))?;
        let at = file.stream_position()?;
        let len = file.metadata()?.len();
        file.flush()?;
        drop(file);
        if !card_allows(&places.card, len + RECHECK_BYTES) {
            return Err(no_room());
        }
        let to = places.card.join(&self.target.name);
        let mut moved = migrate(&self.path, &to, &places.card, &*self.target.pace)?;
        moved.seek(SeekFrom::Start(at))?;
        tracing::info!("memory ran short while rewriting a fetched DAT; it continues on the card");
        self.file = Some(moved);
        self.path = to;
        self.in_ram = false;
        Ok(())
    }
}

impl Write for Spill {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.written + buf.len() as u64 > self.target.limit {
            return Err(std::io::Error::new(
                std::io::ErrorKind::FileTooLarge,
                "the rewritten DAT is too large",
            ));
        }
        if self.written >= self.checked_at + RECHECK_BYTES {
            self.recheck()?;
            self.checked_at = self.written;
        }
        let started = Instant::now();
        let n = self.file()?.write(buf)?;
        if !self.in_ram {
            rest(&*self.target.pace, started.elapsed());
        }
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file()?.flush()
    }
}

impl Seek for Spill {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.file()?.seek(pos)
    }
}

impl Drop for Spill {
    fn drop(&mut self) {
        self.file = None;
        if !self.done {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Copies `from` to a new file `to` in writes of [`CHUNK_BYTES`], resting after each
/// for what `pace` returns given the time it took; `Interrupted` once `stop` says so.
fn copy_chunked(
    from: &Path,
    to: &Path,
    pace: &dyn Fn(Duration) -> Duration,
    stop: &dyn Fn() -> bool,
) -> std::io::Result<()> {
    let mut src = File::open(from)?;
    let mut dst = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)?;
    let mut buf = vec![0u8; CHUNK_BYTES];
    loop {
        let mut filled = 0;
        while filled < buf.len() {
            let n = src.read(&mut buf[filled..])?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        if filled == 0 {
            return Ok(());
        }
        if stop() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "stopped",
            ));
        }
        let started = Instant::now();
        dst.write_all(&buf[..filled])?;
        rest(pace, started.elapsed());
        if filled < buf.len() {
            return Ok(());
        }
    }
}

/// Removes the temporary files fetches left in `dir`, mistarr's own, as after a power cut.
pub fn clean_stale(dir: &Path) {
    clean_parts(dir, PART_PREFIX);
}

/// Removes the files in `dir` named `{prefix}*.part`, left by a move that a power cut
/// or shutdown broke off.
pub fn clean_parts(dir: &Path, prefix: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with(prefix) && Path::new(&name).extension().is_some_and(|e| e == "part") {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

#[cfg(test)]
mod tests;
