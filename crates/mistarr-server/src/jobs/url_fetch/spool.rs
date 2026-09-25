//! The temporary file a fetch streams into: in RAM when memory allows, else on the card.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::error::{Error, Result};
use crate::threads::{blocking, label};

/// Bytes gathered before each write, and copied per write onto the card.
pub const CHUNK_BYTES: usize = crate::db::ram::CHUNK_BYTES;

/// Room assumed for a body of unknown length when choosing where it goes.
const UNKNOWN_GUESS: u64 = 16 * 1024 * 1024;

/// Bytes written between checks that memory still allows a file in RAM.
const RECHECK_BYTES: u64 = 8 * 1024 * 1024;

/// Prefix of a fetch's temporary file, removed at startup when one is left over.
pub const PART_PREFIX: &str = "fetch-";

/// Where a spool may go and what must stay free if it goes to RAM.
#[derive(Debug, Clone)]
pub struct Places {
    /// The RAM directory, SQLite's temporary one, when it is in RAM.
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

/// A writer that rests for what `pace` returns after each write, each of which its
/// caller keeps near [`CHUNK_BYTES`] with a buffer.
pub struct Paced<W> {
    inner: W,
    pace: Pace,
}

impl<W> Paced<W> {
    /// Wraps `inner`.
    pub fn new(inner: W, pace: Pace) -> Self {
        Self { inner, pace }
    }
}

impl<W: Write> Write for Paced<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let started = Instant::now();
        let n = self.inner.write(buf)?;
        rest(&*self.pace, started.elapsed());
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

impl<W: std::io::Seek> std::io::Seek for Paced<W> {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

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
    /// memory allows `length`, or a guess when it is unknown; else on the card, where
    /// every write rests for what `pace` returns.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when no file can be created.
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
                .filter(|dir| ram_allows(dir, expected, places.floor));
            let in_ram = ram.is_some();
            let dir = ram.cloned().unwrap_or_else(|| places.card.clone());
            std::fs::create_dir_all(&dir)?;
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

    /// Writes what has gathered; a file in RAM moves to the card first when memory
    /// or the RAM directory's room falls short. Writes to the card are paced; the caller's
    /// reading waits for them.
    async fn flush(&mut self) -> Result<()> {
        let buf = std::mem::replace(&mut self.buf, Vec::with_capacity(CHUNK_BYTES));
        let len = buf.len() as u64;
        let mut file = self
            .file
            .take()
            .ok_or_else(|| Error::Fetch("the download was closed".into()))?;
        let recheck = self.in_ram && self.written + len >= self.checked_at + RECHECK_BYTES;
        let (path, places) = (self.path.clone(), self.places.clone());
        let (pace, mut on_card) = (Arc::clone(&self.pace), !self.in_ram);
        let moved = blocking(label::FETCH, move || -> Result<(File, Option<PathBuf>)> {
            let mut moved = None;
            if recheck {
                let dir = path.parent().unwrap_or(Path::new("/"));
                if !ram_allows(dir, len.max(CHUNK_BYTES as u64), places.floor) {
                    drop(file);
                    let to = places.card.join(path.file_name().unwrap_or_default());
                    file = migrate(&path, &to, &places.card, &*pace)?;
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

    /// Where a second file as large as this one may go, and whether that is RAM: beside
    /// this one while it is in RAM and memory allows another copy, else on the card.
    #[must_use]
    pub fn beside(&self) -> (PathBuf, bool) {
        let stem = self
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let name = format!("{stem}-pack.part");
        let dir = self
            .path
            .parent()
            .filter(|d| self.in_ram && ram_allows(d, self.written, self.places.floor));
        match dir {
            Some(d) => (d.join(name), true),
            None => (self.places.card.join(name), false),
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
    /// [`Error::Io`] when it cannot be moved, [`Error::Cancelled`] when `stop` said so;
    /// nothing is left at `to` then.
    pub async fn place(mut self, to: PathBuf, stop: Stop) -> Result<()> {
        self.file = None;
        let from = std::mem::take(&mut self.path);
        let (in_ram, pace) = (self.in_ram, Arc::clone(&self.pace));
        blocking(label::FETCH, move || {
            let renamed = !in_ram && std::fs::rename(&from, &to).is_ok();
            if !renamed {
                let copied = copy_chunked(&from, &to, &*pace, &*stop);
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

/// Copies the RAM file `from` to `to` in `card` with paced writes, opens `to` for
/// appending and removes `from`; on any failure `to` is removed and `from` kept.
fn migrate(
    from: &Path,
    to: &Path,
    card: &Path,
    pace: &dyn Fn(Duration) -> Duration,
) -> Result<File> {
    let moved = (|| -> std::io::Result<File> {
        std::fs::create_dir_all(card)?;
        copy_chunked(from, to, pace, &|| false)?;
        let file = std::fs::OpenOptions::new().append(true).open(to)?;
        std::fs::remove_file(from)?;
        Ok(file)
    })();
    moved.map_err(|e| {
        let _ = std::fs::remove_file(to);
        e.into()
    })
}

impl Drop for Spool {
    fn drop(&mut self) {
        self.file = None;
        if !self.path.as_os_str().is_empty() {
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

/// Removes the temporary files fetches left in `dir`, as after a power cut.
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
mod tests {
    use super::*;

    fn places(dir: &Path, ram: bool) -> Places {
        Places {
            ram: ram.then(|| dir.join("ram")),
            card: dir.join("card"),
            floor: 0,
        }
    }

    fn no_rest() -> Pace {
        Arc::new(|_| Duration::ZERO)
    }

    fn never() -> Stop {
        Arc::new(|| false)
    }

    /// A pace that counts its calls and never rests.
    fn counted() -> (Pace, Arc<std::sync::atomic::AtomicUsize>) {
        let n = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c = Arc::clone(&n);
        let pace: Pace = Arc::new(move |_| {
            c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Duration::ZERO
        });
        (pace, n)
    }

    fn body(len: usize) -> Vec<u8> {
        (0..len)
            .map(|i| u8::try_from(i % 251).unwrap_or(0))
            .collect()
    }

    fn files_in(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir).map_or_else(
            |_| Vec::new(),
            |d| {
                d.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            },
        )
    }

    /// A spool in RAM whose floor is then raised so the next recheck moves it.
    async fn squeezed(dir: &Path, pace: Pace) -> Spool {
        std::fs::create_dir_all(dir.join("ram")).expect("mkdir");
        let mut spool = Spool::create(places(dir, true), 11, Some(10), pace)
            .await
            .expect("create");
        assert!(spool.in_ram());
        spool.places.floor = u64::MAX / 2;
        spool
    }

    #[tokio::test]
    async fn memory_running_short_moves_the_file_to_the_card_with_paced_writes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (pace, rests) = counted();
        let mut spool = squeezed(dir.path(), pace).await;
        let data = body(usize::try_from(RECHECK_BYTES).expect("fits") + CHUNK_BYTES + 3);
        for part in data.chunks(CHUNK_BYTES) {
            spool.push(part).await.expect("push");
        }
        spool.finish().await.expect("finish");
        assert!(!spool.in_ram());
        assert!(spool.path().starts_with(dir.path().join("card")));
        assert!(files_in(&dir.path().join("ram")).is_empty());
        assert_eq!(std::fs::read(spool.path()).expect("read"), data);
        let before = rests.load(std::sync::atomic::Ordering::Relaxed);
        assert!(before >= 9, "the move and later card writes rest, {before}");
    }

    #[tokio::test]
    async fn a_failed_move_leaves_no_partial_copy() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let mut spool = squeezed(dir.path(), no_rest()).await;
        let ram = dir.path().join("ram");
        let data = body(CHUNK_BYTES);
        let mut failed = None;
        for _ in 0..8 {
            if failed.is_none() && spool.written + CHUNK_BYTES as u64 * 2 >= RECHECK_BYTES {
                std::fs::set_permissions(&ram, std::fs::Permissions::from_mode(0o500))
                    .expect("chmod");
            }
            if let Err(e) = spool.push(&data).await {
                failed = Some(e);
                break;
            }
        }
        std::fs::set_permissions(&ram, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        assert!(failed.is_some(), "removing the RAM file failed");
        assert!(files_in(&dir.path().join("card")).is_empty());
        let path = spool.path().to_path_buf();
        assert!(path.starts_with(&ram));
        drop(spool);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn a_spool_fills_and_moves_in_chunks() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("ram")).expect("mkdir");
        let (pace, rests) = counted();
        let mut spool = Spool::create(places(dir.path(), true), 7, Some(10), pace)
            .await
            .expect("create");
        assert!(spool.in_ram());
        let body = body(CHUNK_BYTES * 2 + 5);
        for part in body.chunks(100_000) {
            spool.push(part).await.expect("push");
        }
        assert_eq!(spool.finish().await.expect("finish"), body.len() as u64);
        assert_eq!(
            rests.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "RAM writes do not rest"
        );
        let from = spool.path().to_path_buf();
        let to = dir.path().join("placed.dat");
        spool.place(to.clone(), never()).await.expect("place");
        assert_eq!(std::fs::read(&to).expect("read"), body);
        assert!(!from.exists());
        assert_eq!(
            rests.load(std::sync::atomic::Ordering::Relaxed),
            3,
            "one per MiB written"
        );
    }

    #[tokio::test]
    async fn a_stop_while_placing_leaves_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("ram")).expect("mkdir");
        let mut spool = Spool::create(places(dir.path(), true), 12, Some(10), no_rest())
            .await
            .expect("create");
        spool.push(&body(CHUNK_BYTES * 3)).await.expect("push");
        spool.finish().await.expect("finish");
        let from = spool.path().to_path_buf();
        let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = Arc::clone(&asked);
        let stop: Stop =
            Arc::new(move || seen.fetch_add(1, std::sync::atomic::Ordering::Relaxed) >= 1);
        let to = dir.path().join("placed.dat");
        let e = spool.place(to.clone(), stop).await.expect_err("stopped");
        assert!(matches!(e, Error::Cancelled), "{e:?}");
        assert!(!to.exists() && !from.exists());
    }

    #[tokio::test]
    async fn a_rebuilt_file_is_adopted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut spool = Spool::create(places(dir.path(), false), 13, Some(3), no_rest())
            .await
            .expect("create");
        spool.push(b"old").await.expect("push");
        spool.finish().await.expect("finish");
        let old = spool.path().to_path_buf();
        let (beside, in_ram) = spool.beside();
        assert!(!in_ram);
        assert!(beside.starts_with(dir.path().join("card")));
        std::fs::write(&beside, b"new").expect("write");
        spool.adopt(beside.clone(), false);
        assert!(!old.exists());
        let to = dir.path().join("p.zip");
        spool.place(to.clone(), never()).await.expect("place");
        assert_eq!(std::fs::read(&to).expect("read"), b"new");
        let mut sink = Paced::new(Vec::new(), counted().0);
        sink.write_all(b"x").expect("write");
        sink.flush().expect("flush");
    }

    #[tokio::test]
    async fn a_dropped_spool_leaves_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut spool = Spool::create(places(dir.path(), false), 8, None, no_rest())
            .await
            .expect("create");
        assert!(!spool.in_ram());
        spool.push(b"abc").await.expect("push");
        let path = spool.path().to_path_buf();
        assert!(path.starts_with(dir.path().join("card")));
        drop(spool);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn a_file_on_the_card_is_renamed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut spool = Spool::create(places(dir.path(), false), 9, Some(3), no_rest())
            .await
            .expect("create");
        spool.push(b"xyz").await.expect("push");
        spool.finish().await.expect("finish");
        let to = dir.path().join("x.torrent");
        spool.place(to.clone(), never()).await.expect("place");
        assert_eq!(std::fs::read(&to).expect("read"), b"xyz");
    }

    #[test]
    fn stale_parts_are_removed() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("fetch-1-2.part"), b"x").expect("write");
        std::fs::write(dir.path().join("etilqs_1"), b"x").expect("write");
        std::fs::write(dir.path().join(".upload-1-2.part"), b"x").expect("write");
        clean_stale(dir.path());
        assert!(!dir.path().join("fetch-1-2.part").exists());
        assert!(dir.path().join("etilqs_1").exists());
        clean_parts(dir.path(), ".upload-");
        assert!(!dir.path().join(".upload-1-2.part").exists());
        clean_stale(&dir.path().join("none"));
    }

    #[test]
    fn memory_short_of_the_floor_keeps_a_file_off_ram() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(ram_allows(dir.path(), 1, 0));
        assert!(!ram_allows(dir.path(), 1, u64::MAX / 2));
    }
}
