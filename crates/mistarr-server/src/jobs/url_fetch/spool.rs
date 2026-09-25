//! The temporary file a fetch streams into: in RAM when memory allows, else on the card.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
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
}

impl Spool {
    /// Creates the file named for `token`: in RAM when a RAM directory is known and
    /// memory allows `length`, or a guess when it is unknown; else on the card.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when no file can be created.
    pub async fn create(places: Places, token: u64, length: Option<u64>) -> Result<Self> {
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
    /// or the RAM directory's room falls short.
    async fn flush(&mut self) -> Result<()> {
        let buf = std::mem::replace(&mut self.buf, Vec::with_capacity(CHUNK_BYTES));
        let len = buf.len() as u64;
        let mut file = self
            .file
            .take()
            .ok_or_else(|| Error::Fetch("the download was closed".into()))?;
        let recheck = self.in_ram && self.written + len >= self.checked_at + RECHECK_BYTES;
        let (path, places) = (self.path.clone(), self.places.clone());
        let moved = blocking(label::FETCH, move || -> Result<(File, Option<PathBuf>)> {
            let mut moved = None;
            if recheck {
                let dir = path.parent().unwrap_or(Path::new("/"));
                if !ram_allows(dir, len.max(CHUNK_BYTES as u64), places.floor) {
                    drop(file);
                    let to = places.card.join(path.file_name().unwrap_or_default());
                    std::fs::create_dir_all(&places.card)?;
                    copy_chunked(&path, &to, &|_| Duration::ZERO)?;
                    std::fs::remove_file(&path)?;
                    file = std::fs::OpenOptions::new().append(true).open(&to)?;
                    moved = Some(to);
                }
            }
            file.write_all(&buf)?;
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

    /// Moves the finished file to `to`: renamed when it is on the card's file system,
    /// else copied in [`CHUNK_BYTES`] writes, each followed by the rest `rest` returns
    /// for how long it took, and the original removed. The spool no longer owns it.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when it cannot be moved; nothing is left at `to` then.
    pub async fn place(
        mut self,
        to: PathBuf,
        rest: impl Fn(Duration) -> Duration + Send + 'static,
    ) -> Result<()> {
        self.file = None;
        let from = std::mem::take(&mut self.path);
        let in_ram = self.in_ram;
        blocking(label::FETCH, move || {
            let renamed = !in_ram && std::fs::rename(&from, &to).is_ok();
            if !renamed {
                let copied = copy_chunked(&from, &to, &rest);
                let _ = std::fs::remove_file(&from);
                if let Err(e) = copied {
                    let _ = std::fs::remove_file(&to);
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

/// Copies `from` to a new file `to` in writes of [`CHUNK_BYTES`], sleeping after each
/// for what `rest` returns given the time the write took.
fn copy_chunked(
    from: &Path,
    to: &Path,
    rest: &dyn Fn(Duration) -> Duration,
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
        let started = Instant::now();
        dst.write_all(&buf[..filled])?;
        let pause = rest(started.elapsed());
        if !pause.is_zero() {
            std::thread::sleep(pause);
        }
        if filled < buf.len() {
            return Ok(());
        }
    }
}

/// Removes the temporary files fetches left in `dir`, as after a power cut.
pub fn clean_stale(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with(PART_PREFIX)
            && Path::new(&name).extension().is_some_and(|e| e == "part")
        {
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

    #[tokio::test]
    async fn a_spool_fills_and_moves_in_chunks() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("ram")).expect("mkdir");
        let mut spool = Spool::create(places(dir.path(), true), 7, Some(10))
            .await
            .expect("create");
        assert!(spool.in_ram());
        let body: Vec<u8> = (0..CHUNK_BYTES * 2 + 5)
            .map(|i| u8::try_from(i % 251).unwrap_or(0))
            .collect();
        for part in body.chunks(100_000) {
            spool.push(part).await.expect("push");
        }
        assert_eq!(spool.finish().await.expect("finish"), body.len() as u64);
        let from = spool.path().to_path_buf();
        let to = dir.path().join("placed.dat");
        let rests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = std::sync::Arc::clone(&rests);
        spool
            .place(to.clone(), move |_| {
                counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Duration::ZERO
            })
            .await
            .expect("place");
        assert_eq!(std::fs::read(&to).expect("read"), body);
        assert!(!from.exists());
        assert_eq!(
            rests.load(std::sync::atomic::Ordering::Relaxed),
            3,
            "one per MiB written"
        );
    }

    #[tokio::test]
    async fn a_dropped_spool_leaves_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut spool = Spool::create(places(dir.path(), false), 8, None)
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
        let mut spool = Spool::create(places(dir.path(), false), 9, Some(3))
            .await
            .expect("create");
        spool.push(b"xyz").await.expect("push");
        spool.finish().await.expect("finish");
        let to = dir.path().join("x.torrent");
        spool
            .place(to.clone(), |_| Duration::from_secs(60))
            .await
            .expect("place");
        assert_eq!(std::fs::read(&to).expect("read"), b"xyz");
    }

    #[test]
    fn stale_parts_are_removed() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("fetch-1-2.part"), b"x").expect("write");
        std::fs::write(dir.path().join("etilqs_1"), b"x").expect("write");
        clean_stale(dir.path());
        assert!(!dir.path().join("fetch-1-2.part").exists());
        assert!(dir.path().join("etilqs_1").exists());
        clean_stale(&dir.path().join("none"));
    }

    #[test]
    fn memory_short_of_the_floor_keeps_a_file_off_ram() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(ram_allows(dir.path(), 1, 0));
        assert!(!ram_allows(dir.path(), 1, u64::MAX / 2));
    }
}
