//! Logging to stderr and a size-rotated file; see `docs/DEPLOYMENT.md` "Starting it".

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// Size at which the log file rotates.
pub const MAX_BYTES: u64 = 2 * 1024 * 1024;

/// Rotated copies kept beside the current file, as `.1` and `.2`.
pub const GENERATIONS: u32 = 2;

/// A log file that renames itself to `<name>.1` once it would pass `max` bytes,
/// shifting older copies up to `<name>.<generations>` and dropping the oldest.
#[derive(Clone)]
pub struct RotatingFile {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    path: PathBuf,
    max: u64,
    generations: u32,
    file: File,
    len: u64,
}

impl RotatingFile {
    /// Opens `path` for appending.
    ///
    /// # Errors
    ///
    /// Any error opening the file.
    ///
    /// ```
    /// use std::io::Write;
    /// let path = std::env::temp_dir().join("mistarr-doc-rotating.log");
    /// let mut log = mistarr_server::logging::RotatingFile::open(&path, 1024, 2).unwrap();
    /// log.write_all(b"line\n").unwrap();
    /// ```
    pub fn open(path: &Path, max: u64, generations: u32) -> io::Result<Self> {
        let file = open_append(path)?;
        let len = file.metadata()?.len();
        Ok(Self {
            inner: Arc::new(Mutex::new(Inner {
                path: path.to_path_buf(),
                max,
                generations,
                file,
                len,
            })),
        })
    }
}

fn open_append(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

fn generation(path: &Path, n: u32) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{n}"));
    PathBuf::from(name)
}

impl Inner {
    fn rotate(&mut self) -> io::Result<()> {
        for n in (1..self.generations).rev() {
            let from = generation(&self.path, n);
            if from.exists() {
                std::fs::rename(&from, generation(&self.path, n + 1))?;
            }
        }
        if self.generations == 0 {
            std::fs::remove_file(&self.path)?;
        } else {
            std::fs::rename(&self.path, generation(&self.path, 1))?;
        }
        self.file = open_append(&self.path)?;
        self.len = 0;
        Ok(())
    }
}

impl Write for RotatingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let incoming = u64::try_from(buf.len()).unwrap_or(u64::MAX);
        if inner.len > 0 && inner.len.saturating_add(incoming) > inner.max {
            inner.rotate()?;
        }
        inner.file.write_all(buf)?;
        inner.len = inner.len.saturating_add(incoming);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .file
            .flush()
    }
}

impl<'a> MakeWriter<'a> for RotatingFile {
    type Writer = RotatingFile;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Installs the global subscriber: `RUST_LOG` or `info`, to stderr and to
/// `log` when given.
///
/// # Errors
///
/// Any error opening the log file.
pub fn init(log: Option<&Path>) -> io::Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let file = log
        .map(|p| RotatingFile::open(p, MAX_BYTES, GENERATIONS))
        .transpose()?;
    let registry = tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_writer(io::stderr));
    let installed = match file {
        Some(file) => registry
            .with(tracing_subscriber::fmt::layer().with_writer(file))
            .try_init(),
        None => registry.try_init(),
    };
    if installed.is_err() {
        tracing::debug!("a global subscriber was already installed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotates_at_the_limit_and_keeps_two_generations() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("t.log");
        let mut log = RotatingFile::open(&path, 10, 2).expect("open");
        for line in [b"aaaaaa\n", b"bbbbbb\n", b"cccccc\n", b"dddddd\n"] {
            log.write_all(line).expect("write");
        }
        log.flush().expect("flush");
        let read = |p: &Path| std::fs::read_to_string(p).expect("read");
        assert_eq!(read(&path), "dddddd\n");
        assert_eq!(read(&generation(&path, 1)), "cccccc\n");
        assert_eq!(read(&generation(&path, 2)), "bbbbbb\n");
        assert!(!generation(&path, 3).exists());
    }

    #[test]
    fn reopening_counts_existing_bytes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("t.log");
        std::fs::write(&path, "123456789\n").expect("seed");
        let mut log = RotatingFile::open(&path, 12, 1).expect("open");
        log.write_all(b"xyz\n").expect("write");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "xyz\n");
        assert!(generation(&path, 1).exists());
    }

    #[test]
    fn oversized_line_is_written_whole() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("t.log");
        let mut log = RotatingFile::open(&path, 4, 0).expect("open");
        log.write_all(b"longer than four\n").expect("write");
        log.write_all(b"next\n").expect("write");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "next\n");
        let maker = log.clone();
        maker.make_writer().flush().expect("flush");
    }

    #[test]
    fn init_accepts_a_second_call() {
        let dir = tempfile::tempdir().expect("tempdir");
        init(Some(&dir.path().join("a.log"))).expect("first");
        init(None).expect("second");
    }
}
