//! Thread names shown in `/proc/<pid>/task/*/comm`; see `docs/ARCHITECTURE.md` "Thread names".

use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::task::JoinHandle;

/// Longest thread name Linux keeps in `comm`, without the terminating NUL.
pub const MAX_NAME: usize = 15;

/// Prefix of the runtime's worker and blocking pool threads.
pub const RUNTIME_PREFIX: &str = "mistarr-rt-";

/// Name of the next runtime thread: [`RUNTIME_PREFIX`] and a count from 1 that wraps
/// after 9999, since idle blocking threads exit and are started again under new numbers.
///
/// ```
/// let name = mistarr_server::threads::runtime_thread_name();
/// assert!(name.starts_with("mistarr-rt-"));
/// ```
#[must_use]
pub fn runtime_thread_name() -> String {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed) % 9999 + 1;
    format!("{RUNTIME_PREFIX}{n}")
}

/// What a blocking thread is doing, at most [`MAX_NAME`] bytes, checked when the
/// constant is built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Label(&'static str);

impl Label {
    /// A label; fails to compile in a `const` when `name` is longer than [`MAX_NAME`].
    ///
    /// # Panics
    ///
    /// When `name` is longer than [`MAX_NAME`] and the call is not in a `const`.
    ///
    /// ```
    /// const HASH: mistarr_server::threads::Label = mistarr_server::threads::Label::new("hash");
    /// assert_eq!(HASH.as_str(), "hash");
    /// ```
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        assert!(name.len() <= MAX_NAME, "thread label over 15 bytes");
        Self(name)
    }

    /// The label text.
    ///
    /// ```
    /// assert_eq!(mistarr_server::threads::label::DB_READ.as_str(), "db-read");
    /// ```
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

/// The labels blocking work runs under.
pub mod label {
    use super::Label;

    /// A database read through `Db::read`.
    pub const DB_READ: Label = Label::new("db-read");
    /// A database write through `Db::write`.
    pub const DB_WRITE: Label = Label::new("db-write");
    /// Hashing a file or archive member.
    pub const HASH: Label = Label::new("hash");
    /// Listing a games directory or discovering scan units.
    pub const SCAN_LIST: Label = Label::new("scan-list");
    /// Listing a zip's members.
    pub const ZIP_LIST: Label = Label::new("zip-list");
    /// Loading a DAT into the catalogue.
    pub const DAT_IMPORT: Label = Label::new("dat-import");
    /// Saving an uploaded DAT.
    pub const DAT_SAVE: Label = Label::new("dat-save");
    /// Reading or placing a `.torrent` or `.magnet` file.
    pub const SOURCE_FILE: Label = Label::new("source-file");
    /// Finding, hashing, moving or quarantining a downloaded item.
    pub const IMPORT: Label = Label::new("import");
    /// Moving a file to its canonical name.
    pub const RENAME: Label = Label::new("rename");
    /// Arcade catalogue and presence work.
    pub const ARCADE: Label = Label::new("arcade");
    /// Starting a game, core or download client.
    pub const LAUNCH: Label = Label::new("launch");
    /// Looking for installed cores or clients.
    pub const DETECT: Label = Label::new("detect");
    /// Listing the incoming drop directory.
    pub const INCOMING: Label = Label::new("incoming");
    /// Polling `sources/` for new files.
    pub const SOURCE_WATCH: Label = Label::new("source-watch");
    /// Checking which romset archives a title's directory holds.
    pub const ROMSETS: Label = Label::new("romsets");
    /// Reading a CHD image's header and track list.
    pub const CHD_HEADER: Label = Label::new("chd-header");
    /// Decoding a slice of a CHD image's hunks.
    pub const CHD_DECODE: Label = Label::new("chd-decode");

    /// Every label, for tests and docs.
    pub const ALL: [Label; 18] = [
        DB_READ,
        DB_WRITE,
        HASH,
        SCAN_LIST,
        ZIP_LIST,
        DAT_IMPORT,
        DAT_SAVE,
        SOURCE_FILE,
        IMPORT,
        RENAME,
        ARCADE,
        LAUNCH,
        DETECT,
        INCOMING,
        SOURCE_WATCH,
        ROMSETS,
        CHD_HEADER,
        CHD_DECODE,
    ];
}

/// Runs `f` on tokio's blocking pool with the thread named `label` while it runs.
///
/// ```
/// let rt = mistarr_server::memory::runtime().unwrap();
/// let n = rt.block_on(async {
///     mistarr_server::threads::blocking(mistarr_server::threads::label::HASH, || 7).await
/// });
/// assert_eq!(n.unwrap(), 7);
/// ```
pub fn blocking<F, R>(label: Label, f: F) -> JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    tokio::task::spawn_blocking(move || named(label, f))
}

/// Runs `f` on the current thread with its `comm` set to `label`, then restores the
/// thread's own name, also when `f` panics. Off Linux, or without `/proc`, it only runs `f`.
///
/// ```
/// assert_eq!(mistarr_server::threads::named(mistarr_server::threads::label::HASH, || 3), 3);
/// ```
pub fn named<R>(label: Label, f: impl FnOnce() -> R) -> R {
    let _restore = Rename::to(label.as_str());
    f()
}

/// Restores the thread's name when dropped: its Rust name, or for an unnamed thread
/// the `comm` read before the rename.
struct Rename {
    thread: std::thread::Thread,
    saved: Option<String>,
}

impl Rename {
    fn to(name: &str) -> Self {
        let thread = std::thread::current();
        // Only unnamed threads pay the extra read; pool threads are always named.
        let saved = if thread.name().is_none() {
            get_comm()
        } else {
            None
        };
        set_comm(name);
        Self { thread, saved }
    }
}

impl Drop for Rename {
    fn drop(&mut self) {
        if let Some(name) = self.saved.as_deref().or(self.thread.name()) {
            set_comm(name);
        }
    }
}

#[cfg(target_os = "linux")]
fn get_comm() -> Option<String> {
    let s = std::fs::read_to_string("/proc/thread-self/comm").ok()?;
    Some(s.trim_end_matches('\n').to_owned())
}

#[cfg(not(target_os = "linux"))]
fn get_comm() -> Option<String> {
    None
}

#[cfg(target_os = "linux")]
fn set_comm(name: &str) {
    // procfs truncates past 15 bytes; a failure only leaves the old name.
    let _ = std::fs::write("/proc/thread-self/comm", name);
}

#[cfg(not(target_os = "linux"))]
fn set_comm(_name: &str) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn comm() -> String {
        std::fs::read_to_string("/proc/thread-self/comm")
            .map(|s| s.trim_end().to_owned())
            .unwrap_or_default()
    }

    #[test]
    fn every_label_fits_in_comm() {
        for l in label::ALL {
            assert!(l.as_str().len() <= MAX_NAME, "{l:?}");
            assert!(!l.as_str().is_empty());
        }
        assert!(format!("{RUNTIME_PREFIX}9999").len() <= MAX_NAME);
    }

    #[test]
    fn labels_are_distinct() {
        let mut names: Vec<_> = label::ALL.iter().map(|l| l.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), label::ALL.len());
    }

    #[test]
    fn runtime_threads_are_numbered() {
        let a = runtime_thread_name();
        let b = runtime_thread_name();
        assert_ne!(a, b);
        assert!(a.starts_with(RUNTIME_PREFIX) && b.starts_with(RUNTIME_PREFIX));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn blocking_work_carries_its_label_then_the_pool_name() {
        let rt = crate::memory::runtime().expect("runtime");
        let (before, during, after) = rt.block_on(async {
            tokio::task::spawn_blocking(|| {
                let before = comm();
                let during = named(label::DAT_IMPORT, comm);
                (before, during, comm())
            })
            .await
            .expect("join")
        });
        assert!(before.starts_with(RUNTIME_PREFIX), "{before}");
        assert_eq!(during, "dat-import");
        assert_eq!(after, before);
        let seen = rt
            .block_on(async { blocking(label::HASH, comm).await })
            .expect("join");
        assert_eq!(seen, "hash");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_unnamed_thread_gets_its_comm_back() {
        let t = std::thread::Builder::new()
            .spawn(|| {
                let before = comm();
                let during = named(label::HASH, comm);
                (before, during, comm())
            })
            .expect("spawn");
        let (before, during, after) = t.join().expect("join");
        assert_eq!(during, "hash");
        assert_eq!(after, before);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_panic_still_restores_the_name() {
        let t = std::thread::Builder::new()
            .name("restore-test".into())
            .spawn(|| {
                let caught = std::panic::catch_unwind(|| named(label::HASH, || panic!("boom")));
                assert!(caught.is_err());
                comm()
            })
            .expect("spawn");
        assert_eq!(t.join().expect("join"), "restore-test");
    }

    #[test]
    fn named_returns_the_closures_value() {
        assert_eq!(named(label::DB_READ, || 11), 11);
    }
}
