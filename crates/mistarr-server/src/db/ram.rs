//! Work on a copy of the database in RAM, written back whole; see `docs/ARCHITECTURE.md` "DAT import in RAM".

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rusqlite::backup::{Backup, StepResult};
use rusqlite::{Connection, ErrorCode};

use super::{sibling, Db, HeldWriter, OLD_SUFFIX, SWAP_SUFFIX};
use crate::error::{Error, Result};

/// Bytes per `write` when the copy goes back to the card; a sync mount flushes each once.
pub const CHUNK_BYTES: usize = 1024 * 1024;

/// Pages the copy into RAM takes per backup step, 1 MiB of 4 KiB pages.
const COPY_PAGES: std::ffi::c_int = 256;

/// Room the copy needs beyond its own size, its growth and its journal.
const MARGIN_BYTES: u64 = 32 * 1024 * 1024;

/// Bytes the copy and SQLite's temporary files, in the same tmpfs on the board, may
/// take per byte of DAT loaded: its rows, their indexes and the stage; `docs/ARCHITECTURE.md`
/// "DAT import in RAM" has the measurement.
const INPUT_FACTOR: u64 = 6;

/// `statfs` magic numbers of the file systems a copy may be made on.
const TMPFS_MAGIC: u64 = 0x0102_1994;
const RAMFS_MAGIC: u64 = 0x8584_58f6;

/// Suffix of the copy written beside the database and then swapped in.
pub const NEW_SUFFIX: &str = ".new";

/// The copy's file name inside its working directory.
const COPY_NAME: &str = "mistarr.db";

/// How long a backup step that found the source busy waits before the next.
const BUSY_WAIT: Duration = Duration::from_millis(50);

const MIB: u64 = 1024 * 1024;

/// Where the copy goes and how much memory must stay available beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The RAM-backed directory the working directory is made in.
    pub dir: PathBuf,
    /// Bytes of `MemAvailable` kept free on top of what the copy needs.
    pub floor: u64,
    /// Names the working directory; the job's id.
    pub job: i64,
    /// Uncompressed bytes of the DATs the work loads, 0 for a migration.
    pub input: u64,
}

/// A step of [`run`], reported as it starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Phase {
    /// The database is copied into RAM.
    Copying,
    /// The work runs on the copy.
    Importing,
    /// The copy is written back to the card and swapped in.
    Writing,
}

impl Phase {
    /// The phase as the job's progress names it.
    ///
    /// ```
    /// use mistarr_server::db::ram::Phase;
    /// assert_eq!(Phase::Writing.label(), "writing the database to the card");
    /// ```
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Copying => "copying the database to memory",
            Self::Importing => "importing",
            Self::Writing => "writing the database to the card",
        }
    }
}

/// What [`run`] tells its caller as it goes, and asks between steps.
pub trait Watch {
    /// Called as `phase` starts.
    fn phase(&mut self, phase: Phase);

    /// Called between copy steps and written chunks of `phase`; an error stops [`run`]
    /// there, with the card file untouched unless the swap already ran.
    ///
    /// # Errors
    ///
    /// Whatever the caller wants [`run`] to stop with, such as [`Error::Cancelled`].
    fn between(&mut self, phase: Phase) -> Result<()>;
}

/// A watch that reports nothing and never stops the run.
impl Watch for () {
    fn phase(&mut self, _: Phase) {}

    fn between(&mut self, _: Phase) -> Result<()> {
        Ok(())
    }
}

/// How [`run`] ended when it did not fail.
#[derive(Debug)]
pub enum Ram<T> {
    /// The work ran on the copy, which is now the database when it asked for that.
    Done(T, Report),
    /// The copy was not used, for the reason given; the card file is as it was.
    Fallback(Fallback),
}

/// Why [`run`] left the work to the card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fallback {
    /// A few words for the job's progress.
    pub summary: &'static str,
    /// The measurements behind it, for the log.
    pub detail: String,
}

/// Summaries of [`Fallback`]; a few words each, shown as the job's progress.
pub mod why {
    /// The WAL could not be emptied first.
    pub const BUSY: &str = "the database was busy";
    /// Another process has the database open.
    pub const SHARED: &str = "another process has the database open";
    /// `[memory] import_dir` is not usable.
    pub const DIR: &str = "the memory directory cannot be used";
    /// Memory or room was short before the copy.
    pub const SHORT: &str = "not enough free memory for a copy";
    /// Memory or the memory directory ran short on the way.
    pub const RAN_SHORT: &str = "memory ran short during the import";
    /// The card has no room for the copy it would write back.
    pub const CARD: &str = "not enough room on the card";
}

impl<T> Ram<T> {
    fn fallback(summary: &'static str, detail: impl Into<String>) -> Self {
        Self::Fallback(Fallback {
            summary,
            detail: detail.into(),
        })
    }
}

/// What a [`run`] cost.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Report {
    /// Size of the file written back, 0 when nothing was.
    pub bytes: u64,
    /// Write syscalls to the card: the first checkpoint, the write-back and the swap.
    /// `None` where the kernel does not count them per thread.
    pub card_writes: Option<u64>,
    /// Time copying the database into RAM.
    pub copy_in: Duration,
    /// Time the work took on the copy.
    pub work: Duration,
    /// Time writing the copy back and swapping it in.
    pub write_back: Duration,
}

/// Memory and room the check reads; `None` where a value cannot be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// `MemAvailable` from `/proc/meminfo`, in bytes.
    pub available: Option<u64>,
    /// Free bytes in the RAM directory.
    pub ram_room: Option<u64>,
    /// Free bytes beside the database on the card.
    pub card_room: Option<u64>,
}

impl Budget {
    /// Reads the memory available, and the free space in `dir` and beside `db`.
    #[must_use]
    pub fn read(dir: &Path, db: &Path) -> Self {
        let available = fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|t| mem_available(&t));
        Self {
            available,
            ram_room: free_bytes(dir),
            card_room: db.parent().and_then(free_bytes),
        }
    }
}

/// Bytes a copy of a `size`-byte database needs in RAM while it loads `input` bytes of
/// DAT: the copy, half again for its journal and freed pages, [`INPUT_FACTOR`] times the
/// input for the rows it keeps and the stage in SQLite's temporary files, and
/// [`MARGIN_BYTES`].
///
/// ```
/// assert_eq!(mistarr_server::db::ram::need(64 << 20, 0), (96 + 32) << 20);
/// assert_eq!(mistarr_server::db::ram::need(64 << 20, 10 << 20), (96 + 60 + 32) << 20);
/// ```
#[must_use]
pub fn need(size: u64, input: u64) -> u64 {
    size.saturating_add(size / 2)
        .saturating_add(input.saturating_mul(INPUT_FACTOR))
        .saturating_add(MARGIN_BYTES)
}

/// `MemAvailable` of a `/proc/meminfo` text, in bytes.
///
/// ```
/// let text = "MemTotal: 498 kB\nMemAvailable:     387072 kB\n";
/// assert_eq!(mistarr_server::db::ram::mem_available(text), Some(387_072 * 1024));
/// ```
#[must_use]
pub fn mem_available(meminfo: &str) -> Option<u64> {
    let line = meminfo
        .lines()
        .find_map(|l| l.strip_prefix("MemAvailable:"))?;
    let kib: u64 = line.split_whitespace().next()?.parse().ok()?;
    kib.checked_mul(1024)
}

/// Why a `size`-byte database loading `input` bytes of DAT cannot be copied into RAM
/// under `budget` keeping `floor` bytes available, or `None` when it can.
///
/// ```
/// use mistarr_server::db::ram::{refusal, Budget};
/// let roomy = Budget { available: Some(1 << 30), ram_room: Some(1 << 30), card_room: Some(1 << 30) };
/// assert_eq!(refusal(&roomy, 40 << 20, 0, 128 << 20), None);
/// assert!(refusal(&roomy, 40 << 20, 0, 1 << 30).is_some());
/// assert!(refusal(&roomy, 40 << 20, 400 << 20, 128 << 20).is_some());
/// ```
#[must_use]
pub fn refusal(budget: &Budget, size: u64, input: u64, floor: u64) -> Option<String> {
    let need = need(size, input);
    let mib = |b: u64| b.div_ceil(MIB);
    let Some(available) = budget.available else {
        return Some("the memory available cannot be read".to_owned());
    };
    if available < need.saturating_add(floor) {
        return Some(format!(
            "{} MiB of memory available, {} MiB needed: {} for the copy and {} kept free",
            mib(available),
            mib(need.saturating_add(floor)),
            mib(need),
            mib(floor)
        ));
    }
    match budget.ram_room {
        None => return Some("the free space in the memory directory cannot be read".to_owned()),
        Some(room) if room < need => {
            return Some(format!(
                "{} MiB free in the memory directory, {} MiB needed",
                mib(room),
                mib(need)
            ))
        }
        Some(_) => {}
    }
    match budget.card_room {
        Some(room) if room < size.saturating_add(CHUNK_BYTES as u64) => Some(format!(
            "{} MiB free on the card, {} MiB needed for a second copy of the database",
            mib(room),
            mib(size) + 1
        )),
        _ => None,
    }
}

/// Free bytes for an unprivileged writer on the filesystem holding `dir`.
fn free_bytes(dir: &Path) -> Option<u64> {
    let st = rustix::fs::statvfs(dir).ok()?;
    st.f_bavail.checked_mul(st.f_frsize)
}

/// Why `dir` cannot hold a copy of the database `db`, or `None` when it can: it must be,
/// or become, a private directory as SQLite's temporary one is, on tmpfs or ramfs, and
/// not on the file system holding `db`.
///
/// ```
/// let dir = tempfile::tempdir().unwrap();
/// let db = dir.path().join("m.db");
/// let why = mistarr_server::db::ram::dir_refusal(&dir.path().join("ram"), &db);
/// assert!(why.is_some(), "beside the database");
/// ```
#[must_use]
pub fn dir_refusal(dir: &Path, db: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    if let Err(e) = super::private_dir(dir) {
        return Some(format!("the memory directory cannot be used: {e}"));
    }
    let kind = rustix::fs::statfs(dir)
        .ok()
        .and_then(|s| u64::try_from(s.f_type).ok());
    if !matches!(kind, Some(TMPFS_MAGIC | RAMFS_MAGIC)) {
        return Some(format!("{} is not in RAM (tmpfs)", dir.display()));
    }
    let parent = db.parent().filter(|p| !p.as_os_str().is_empty());
    let db_dev = fs::metadata(parent.unwrap_or(Path::new("."))).map(|m| m.dev());
    let dir_dev = fs::metadata(dir).map(|m| m.dev());
    match (db_dev, dir_dev) {
        (Ok(a), Ok(b)) if a != b => None,
        (Ok(_), Ok(_)) => Some(format!(
            "{} is on the same file system as the database",
            dir.display()
        )),
        (Err(e), _) | (_, Err(e)) => Some(format!("cannot read the memory directory: {e}")),
    }
}

/// [`Error::NoRoom`] when `MemAvailable` has fallen below `floor`, so an import in RAM
/// drops its copy and runs on the card; `Ok` when it cannot be read.
///
/// # Errors
///
/// [`Error::NoRoom`] naming both amounts.
///
/// ```
/// assert!(mistarr_server::db::ram::memory_left(0).is_ok());
/// assert!(mistarr_server::db::ram::memory_left(u64::MAX).is_err());
/// ```
pub fn memory_left(floor: u64) -> Result<()> {
    let available = fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|t| mem_available(&t));
    match available {
        Some(a) if a < floor => Err(Error::NoRoom(format!(
            "memory available fell to {} MiB, under the {} MiB kept free",
            a.div_ceil(MIB),
            floor.div_ceil(MIB)
        ))),
        _ => Ok(()),
    }
}

/// Copies the database `held` writes into RAM, runs `work` on the copy and, when `work`
/// asks for it by returning `true` beside its value, writes the copy back beside the
/// database in [`CHUNK_BYTES`] writes, syncs it and swaps it in with
/// [`HeldWriter::replace_file`]. Until the swap the card file is never written, except
/// by the checkpoint that empties its WAL first. The working directory goes on every
/// path out. Memory or room short at the start, or running out on the way, which `work`
/// reports as [`Error::NoRoom`] or SQLite's full error, returns [`Ram::Fallback`] with the
/// card file as it was.
///
/// # Errors
///
/// Whatever `watch` or `work` fails with, such as [`Error::Cancelled`], or the I/O or
/// SQLite failure that stopped a step, with the card file untouched;
/// [`Error::Reopen`] when the swap left no connection open, never a fallback.
pub fn run<T>(
    held: &mut HeldWriter<'_>,
    plan: &Plan,
    watch: &mut dyn Watch,
    work: impl FnOnce(&Db) -> Result<(T, bool)>,
) -> Result<Ram<T>> {
    let path = held.path().to_path_buf();
    let mut report = Report::default();
    let (emptied, first) = counted(|| super::wal_emptied(held.conn()));
    if !emptied? {
        return Ok(Ram::fallback(
            why::BUSY,
            "a reader kept the database's WAL from being emptied",
        ));
    }
    if held_elsewhere(&path) {
        return Ok(Ram::fallback(why::SHARED, SHARED));
    }
    if let Some(reason) = dir_refusal(&plan.dir, &path) {
        return Ok(Ram::fallback(why::DIR, reason));
    }
    let size = fs::metadata(&path)?.len();
    let budget = Budget::read(&plan.dir, &path);
    if let Some(reason) = refusal(&budget, size, plan.input, plan.floor) {
        return Ok(Ram::fallback(why::SHORT, reason));
    }
    let work_dir = match WorkDir::create(&plan.dir, &path, plan.job) {
        Ok(d) => d,
        Err(e) => {
            return Ok(Ram::fallback(
                why::DIR,
                format!("cannot make the memory directory: {e}"),
            ))
        }
    };
    let copy = work_dir.0.join(COPY_NAME);

    watch.phase(Phase::Copying);
    let started = Instant::now();
    if let Err(reason) = full_as_reason(copy_in(held.conn(), &copy, watch), "copying")? {
        return Ok(Ram::fallback(why::RAN_SHORT, reason));
    }
    // The backup filled the idle writer's cache; the import's own cache needs the room.
    held.conn().execute_batch("PRAGMA shrink_memory")?;
    report.copy_in = started.elapsed();

    watch.phase(Phase::Importing);
    let started = Instant::now();
    let done = Db::open_copy(&copy, None).and_then(|db| {
        let out = work(&db);
        let closed = db.close();
        let v = out?;
        closed.map(|()| v)
    });
    let (value, write) = match full_as_reason(done, "importing")? {
        Ok(v) => v,
        Err(reason) => return Ok(Ram::fallback(why::RAN_SHORT, reason)),
    };
    report.work = started.elapsed();
    if !write {
        report.card_writes = first;
        return Ok(Ram::Done(value, report));
    }

    watch.phase(Phase::Writing);
    let started = Instant::now();
    let bytes = fs::metadata(&copy)?.len();
    let card_room = path.parent().and_then(free_bytes);
    if card_room.is_some_and(|room| room < bytes.saturating_add(CHUNK_BYTES as u64)) {
        return Ok(Ram::fallback(
            why::CARD,
            format!(
                "the card has no room for a second copy of the database, {} MiB",
                bytes.div_ceil(MIB)
            ),
        ));
    }
    let new = sibling(&path, NEW_SUFFIX);
    let (written, back) = counted(|| {
        write_new(&copy, &new, &mut || watch.between(Phase::Writing))?;
        if !wait_unshared(&path, &mut || watch.between(Phase::Writing))? {
            return Ok(false);
        }
        held.replace_file(&new).map(|()| true)
    });
    match written {
        Ok(true) => {}
        Ok(false) => {
            remove_new(&new);
            return Ok(Ram::fallback(why::SHARED, SHARED));
        }
        Err(e) => {
            remove_new(&new);
            if storage_full(&e) {
                return Ok(Ram::fallback(
                    why::CARD,
                    format!("the card ran out of room writing the database: {e}"),
                ));
            }
            return Err(e);
        }
    }
    report.write_back = started.elapsed();
    report.bytes = bytes;
    report.card_writes = first.zip(back).map(|(a, b)| a + b);
    drop(work_dir);
    Ok(Ram::Done(value, report))
}

/// Why a swap did not happen while another process kept the database open.
const SHARED: &str = "another process kept the database open";

/// How long a swap waits for other processes to close the database, and how often it looks.
const SHARED_WAIT: Duration = Duration::from_secs(30);
const SHARED_POLL: Duration = Duration::from_millis(250);

/// Removes the `.new` copy a failed or abandoned swap left.
fn remove_new(new: &Path) {
    if let Err(e) = fs::remove_file(new) {
        if e.kind() != io::ErrorKind::NotFound {
            tracing::warn!(error = %e, "cannot remove the partial database copy");
        }
    }
}

/// Waits until no other process holds `db` open, asking `between` each time; false when
/// one still does after [`SHARED_WAIT`]. SQLite in such a process, closing its last
/// connection to the old file, would remove the swapped file's `-wal` and `-shm` by name.
fn wait_unshared(db: &Path, between: &mut dyn FnMut() -> Result<()>) -> Result<bool> {
    let started = Instant::now();
    while held_elsewhere(db) {
        if started.elapsed() >= SHARED_WAIT {
            return Ok(false);
        }
        between()?;
        std::thread::sleep(SHARED_POLL);
    }
    Ok(true)
}

/// Whether a process other than this one has `db`, its `-wal` or its `-shm` open, read
/// from `/proc/<pid>/fd`. Blind to processes whose descriptors it cannot read (another
/// user's) and to the file opened through another path, such as a bind mount.
///
/// ```
/// let dir = tempfile::tempdir().unwrap();
/// let db = dir.path().join("m.db");
/// std::fs::write(&db, b"x").unwrap();
/// let _mine = std::fs::File::open(&db).unwrap();
/// assert!(!mistarr_server::db::ram::held_elsewhere(&db));
/// ```
#[must_use]
pub fn held_elsewhere(db: &Path) -> bool {
    let (Some(dir), Some(name)) = (db.parent(), db.file_name()) else {
        return false;
    };
    let Ok(dir) = dir.canonicalize() else {
        return false;
    };
    let canon = dir.join(name);
    let targets = ["", "-wal", "-shm"].map(|s| sibling(&canon, s));
    let me = std::process::id().to_string();
    let Ok(procs) = fs::read_dir("/proc") else {
        return false;
    };
    for p in procs.flatten() {
        let pid = p.file_name();
        let pid = pid.to_string_lossy();
        if pid == me || !pid.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(fds) = fs::read_dir(p.path().join("fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            if fs::read_link(fd.path()).is_ok_and(|t| targets.contains(&t)) {
                return true;
            }
        }
    }
    false
}

/// Applies pending migrations to a copy of `db` in RAM and swaps it in, as a DAT import
/// does; run at startup before any connection of the server opens, under the data
/// directory's lock. `None` when nothing is pending, there is no database yet, or memory,
/// room or another process holding the file stand in the way, for which it logs why; the
/// server's open then migrates the card file itself.
///
/// # Errors
///
/// [`Error::SchemaTooNew`] before anything is written, else the failure of a step, with
/// the card file untouched unless a failed swap left `.old`, which the next start restores.
pub fn migrate_in_ram(
    db: &Path,
    plan: &Plan,
    steps: Option<&crate::migrating::Steps>,
) -> Result<Option<Report>> {
    if !db.is_file() {
        return Ok(None);
    }
    let conn = Connection::open(db)?;
    conn.busy_timeout(Duration::from_secs(5))?;
    let found = super::migrate::check_supported(&conn)?;
    if found == 0 || found >= super::migrate::latest() {
        return Ok(None);
    }
    let mut report = Report::default();
    let (emptied, first) = counted(|| super::wal_emptied(&conn));
    let skip = |reason: &str| {
        tracing::info!(reason, "migrating the database in place");
        Ok(None)
    };
    if !emptied? {
        return skip("a reader kept the database's WAL from being emptied");
    }
    if held_elsewhere(db) {
        return skip(SHARED);
    }
    if let Some(reason) = dir_refusal(&plan.dir, db) {
        return skip(&reason);
    }
    let size = fs::metadata(db)?.len();
    if let Some(reason) = refusal(&Budget::read(&plan.dir, db), size, 0, plan.floor) {
        return skip(&reason);
    }
    let Ok(work_dir) = WorkDir::create(&plan.dir, db, plan.job) else {
        return skip("the memory directory cannot be made");
    };
    let copy = work_dir.0.join(COPY_NAME);
    let started = Instant::now();
    let copied = copy_in(&conn, &copy, &mut ());
    super::close_connection(conn)?;
    if let Err(reason) = full_as_reason(copied, "copying")? {
        return skip(&reason);
    }
    report.copy_in = started.elapsed();
    let started = Instant::now();
    if let Err(reason) =
        full_as_reason(Db::open_copy(&copy, steps).and_then(Db::close), "migrating")?
    {
        return skip(&reason);
    }
    report.work = started.elapsed();
    let started = Instant::now();
    let bytes = fs::metadata(&copy)?.len();
    if free_bytes(db.parent().unwrap_or(Path::new(".")))
        .is_some_and(|room| room < bytes.saturating_add(CHUNK_BYTES as u64))
    {
        return skip("the card has no room for a second copy of the database");
    }
    let new = sibling(db, NEW_SUFFIX);
    let (written, back) = counted(|| {
        write_new(&copy, &new, &mut || Ok(()))?;
        if !wait_unshared(db, &mut || Ok(()))? {
            return Ok(false);
        }
        super::install_file(db, &new)?;
        Ok(true)
    });
    match written {
        Ok(true) => {}
        Ok(false) => {
            remove_new(&new);
            return skip(SHARED);
        }
        Err(e) => {
            remove_new(&new);
            if storage_full(&e) {
                return skip(&format!("the card ran out of room: {e}"));
            }
            return Err(e);
        }
    }
    report.write_back = started.elapsed();
    report.bytes = bytes;
    report.card_writes = first.zip(back).map(|(a, b)| a + b);
    Ok(Some(report))
}

/// `r`, with running out of memory or room turned into the reason for a fallback.
fn full_as_reason<T>(r: Result<T>, during: &str) -> Result<std::result::Result<T, String>> {
    match r {
        Ok(v) => Ok(Ok(v)),
        Err(e) if storage_full(&e) => Ok(Err(format!("memory ran out {during} in RAM: {e}"))),
        Err(e) => Err(e),
    }
}

/// True for failures that mean the RAM directory, the memory or the card is full;
/// never for [`Error::Reopen`], whatever it wraps.
///
/// ```
/// use mistarr_server::db::ram::storage_full;
/// let full = std::io::Error::from_raw_os_error(28);
/// assert!(storage_full(&full.into()));
/// assert!(!storage_full(&mistarr_server::Error::Cancelled));
/// ```
#[must_use]
pub fn storage_full(e: &Error) -> bool {
    match e {
        Error::Io(io) => {
            matches!(io.raw_os_error(), Some(12 | 28 | 122))
                || matches!(
                    io.kind(),
                    io::ErrorKind::StorageFull | io::ErrorKind::OutOfMemory
                )
        }
        Error::Db(rusqlite::Error::SqliteFailure(f, _)) => {
            matches!(f.code, ErrorCode::DiskFull | ErrorCode::OutOfMemory)
        }
        Error::NoRoom(_) => true,
        _ => false,
    }
}

/// Copies the database `src` reads into a new file at `dst` through SQLite's backup,
/// 1 MiB per step, asking `watch` between steps. SQLite reads the source, so no other
/// descriptor of it is opened and closed under the connections' locks.
fn copy_in(src: &Connection, dst: &Path, watch: &mut dyn Watch) -> Result<()> {
    let mut to = Connection::open(dst)?;
    to.pragma_update(None, "cache_size", -1024)?;
    {
        let backup = Backup::new(src, &mut to)?;
        loop {
            match backup.step(COPY_PAGES)? {
                StepResult::Done => break,
                StepResult::More => {}
                _ => std::thread::sleep(BUSY_WAIT),
            }
            watch.between(Phase::Copying)?;
        }
    }
    to.close().map_err(|(_, e)| Error::Db(e))
}

/// A destination [`copy_durable`] writes in chunks and then makes durable.
pub trait Durable: Write {
    /// Flushes what was written to stable storage.
    ///
    /// # Errors
    ///
    /// The I/O failure of the flush.
    fn sync(&mut self) -> io::Result<()>;
}

impl Durable for File {
    fn sync(&mut self) -> io::Result<()> {
        self.sync_all()
    }
}

/// Copies `src` to `dst` in writes of exactly [`CHUNK_BYTES`], the last shorter, calls
/// `between` after each full chunk and syncs `dst` at the end. Returns the bytes copied.
///
/// # Errors
///
/// An I/O failure reading or writing, or whatever `between` returns.
///
/// ```
/// use mistarr_server::db::ram::{copy_durable, Durable};
/// struct Sink(Vec<u8>);
/// impl std::io::Write for Sink {
///     fn write(&mut self, b: &[u8]) -> std::io::Result<usize> { self.0.extend_from_slice(b); Ok(b.len()) }
///     fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
/// }
/// impl Durable for Sink { fn sync(&mut self) -> std::io::Result<()> { Ok(()) } }
/// let mut sink = Sink(Vec::new());
/// let n = copy_durable(&mut &b"abc"[..], &mut sink, &mut || Ok(())).unwrap();
/// assert_eq!((n, sink.0.as_slice()), (3, &b"abc"[..]));
/// ```
pub fn copy_durable(
    src: &mut impl Read,
    dst: &mut impl Durable,
    between: &mut dyn FnMut() -> Result<()>,
) -> Result<u64> {
    let mut buf = vec![0u8; CHUNK_BYTES];
    let mut total = 0u64;
    loop {
        let n = fill(src, &mut buf)?;
        if n > 0 {
            dst.write_all(&buf[..n])?;
            total += n as u64;
        }
        if n < CHUNK_BYTES {
            break;
        }
        between()?;
    }
    dst.sync()?;
    Ok(total)
}

/// Reads into `buf` until it is full or `src` ends; returns the bytes read.
fn fill(src: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match src.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

/// Writes `src` to a new file `new` with [`copy_durable`] and returns its size.
///
/// # Errors
///
/// An I/O failure, or whatever `between` returns; `new` is then left for the caller.
///
/// ```
/// let dir = tempfile::tempdir().unwrap();
/// let (src, new) = (dir.path().join("a"), dir.path().join("a.new"));
/// std::fs::write(&src, b"page").unwrap();
/// assert_eq!(mistarr_server::db::ram::write_new(&src, &new, &mut || Ok(())).unwrap(), 4);
/// assert_eq!(std::fs::read(&new).unwrap(), b"page");
/// ```
pub fn write_new(src: &Path, new: &Path, between: &mut dyn FnMut() -> Result<()>) -> Result<u64> {
    let mut from = File::open(src)?;
    let mut to = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(new)?;
    copy_durable(&mut from, &mut to, between)
}

/// Finishes or undoes a swap a crash cut short, then removes what an earlier run left:
/// `db`'s `.new` copy, never trusted once the swap has not begun since a power loss may
/// have cut it short, and working directories of `db` under `dir`. Run before the
/// database opens, under the data directory's lock. Returns how many files it removed.
///
/// # Errors
///
/// [`Error::Io`] when a swap cut short cannot be finished; the database cannot open.
///
/// ```
/// let dir = tempfile::tempdir().unwrap();
/// let db = dir.path().join("m.db");
/// std::fs::write(&db, b"the database").unwrap();
/// std::fs::write(dir.path().join("m.db.new"), b"partial").unwrap();
/// assert_eq!(mistarr_server::db::ram::clean_stale(&db, &dir.path().join("ram")).unwrap(), 1);
/// ```
pub fn clean_stale(db: &Path, dir: &Path) -> Result<usize> {
    let mut removed = finish_swap(db)?;
    warn_on_twins(db);
    let new = sibling(db, NEW_SUFFIX);
    // Beside no database, `.new` is the only copy there is; `finish_swap` has refused it.
    if db.exists() {
        match fs::remove_file(&new) {
            Ok(()) => {
                tracing::info!(file = %new.display(), "removed an unfinished database copy");
                removed += 1;
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!(error = %e, "cannot remove an unfinished database copy"),
        }
    }
    let prefix = work_prefix(db);
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(removed);
    };
    for entry in entries.flatten() {
        if !entry.file_name().to_string_lossy().starts_with(&prefix) {
            continue;
        }
        match fs::remove_dir_all(entry.path()) {
            Ok(()) => {
                tracing::info!(dir = %entry.path().display(), "removed a stale import copy");
                removed += 1;
            }
            Err(e) => tracing::warn!(error = %e, "cannot remove a stale import copy"),
        }
    }
    Ok(removed)
}

/// The step of [`super::install_file`] a crash stopped at, from the files present.
/// Beside `db`, `.old` is removed. Without `db`, a `.new` that passes SQLite's
/// `quick_check` is renamed in and `.old` removed, since `.new` is synced before the
/// swap begins and a rename on exFAT may leave neither of its names readable; else
/// `.old` is renamed back. The `.swap` marker goes once `db` is back. Returns how many
/// files it removed.
///
/// # Errors
///
/// [`Error::Io`] naming `.new` when it fails the check, or a rename or removal failing.
fn finish_swap(db: &Path) -> Result<usize> {
    let (old, new) = (sibling(db, OLD_SUFFIX), sibling(db, NEW_SUFFIX));
    let mut removed = 0;
    if db.exists() {
        if old.exists() {
            fs::remove_file(&old)?;
            tracing::info!(file = %old.display(), "removed the database a finished swap replaced");
            removed += 1;
        }
    } else if new.exists() {
        check_whole(&new)?;
        fs::rename(&new, db)?;
        super::sync_parent(db);
        if old.exists() {
            fs::remove_file(&old)?;
            removed += 1;
        }
        tracing::info!("finished swapping in the database written from RAM");
    } else if old.exists() {
        fs::rename(&old, db)?;
        tracing::warn!("put the old database back; the swap had lost its new file");
    }
    super::sync_parent(db);
    let marker = sibling(db, SWAP_SUFFIX);
    if db.exists() && marker.exists() {
        fs::remove_file(&marker)?;
        super::sync_parent(db);
    }
    Ok(removed)
}

/// Runs SQLite's `quick_check` on the database file `path`, which has no `-wal`.
fn check_whole(path: &Path) -> Result<()> {
    let refuse = |why: String| -> Result<()> {
        Err(io::Error::other(format!(
            "{} is the only copy of the database a swap left and {why}; move it aside to start \
             afresh, or restore mistarr.db.prev",
            path.display()
        ))
        .into())
    };
    let conn = match Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)
    {
        Ok(c) => c,
        Err(e) => return refuse(format!("cannot be opened: {e}")),
    };
    let verdict: std::result::Result<String, _> =
        conn.query_row("PRAGMA quick_check", [], |r| r.get(0));
    super::close_connection(conn)?;
    match verdict {
        Ok(v) if v == "ok" => Ok(()),
        Ok(v) => refuse(format!("failed SQLite's check: {v}")),
        Err(e) => refuse(format!("failed SQLite's check: {e}")),
    }
}

/// Whether a swap's files, `.old`, `.new` or the `.swap` marker, sit beside `db`; a
/// start that saw any never creates a database under the name.
///
/// ```
/// let dir = tempfile::tempdir().unwrap();
/// let db = dir.path().join("m.db");
/// assert!(!mistarr_server::db::ram::swap_files(&db));
/// std::fs::write(dir.path().join("m.db.swap"), b"").unwrap();
/// assert!(mistarr_server::db::ram::swap_files(&db));
/// ```
#[must_use]
pub fn swap_files(db: &Path) -> bool {
    [OLD_SUFFIX, NEW_SUFFIX, SWAP_SUFFIX]
        .iter()
        .any(|s| sibling(db, s).exists())
}

/// Warns when the database's directory lists its name more than once, which only a
/// damaged file system does.
fn warn_on_twins(db: &Path) {
    let (Some(dir), Some(name)) = (db.parent(), db.file_name()) else {
        return;
    };
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let seen = entries.flatten().filter(|e| e.file_name() == name).count();
    if seen > 1 {
        tracing::warn!(
            dir = %dir.display(),
            seen,
            "the data directory lists the database more than once; check the card's file system"
        );
    }
}

/// `import-<key>-`, where the key is a stable hash of `db`'s path, so servers of two
/// data directories sharing one RAM directory never clean each other's copies.
fn work_prefix(db: &Path) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in db.as_os_str().as_encoded_bytes() {
        h = (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3);
    }
    format!("import-{h:016x}-")
}

/// A working directory for one import, removed with everything in it when dropped.
struct WorkDir(PathBuf);

impl WorkDir {
    fn create(dir: &Path, db: &Path, job: i64) -> io::Result<Self> {
        let path = dir.join(format!("{}{job}", work_prefix(db)));
        if path.exists() {
            fs::remove_dir_all(&path)?;
        }
        fs::create_dir_all(&path)?;
        Ok(Self(path))
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        if let Err(e) = fs::remove_dir_all(&self.0) {
            if e.kind() != io::ErrorKind::NotFound {
                tracing::warn!(error = %e, dir = %self.0.display(), "cannot remove the import copy");
            }
        }
    }
}

/// Runs `f` and counts the write syscalls it made on this thread.
fn counted<T>(f: impl FnOnce() -> T) -> (T, Option<u64>) {
    let before = thread_writes();
    let out = f();
    let after = thread_writes();
    (out, before.zip(after).map(|(b, a)| a.saturating_sub(b)))
}

/// Write syscalls this thread made, from `/proc/thread-self/io`.
fn thread_writes() -> Option<u64> {
    let io = fs::read_to_string("/proc/thread-self/io").ok()?;
    io.lines()
        .find_map(|l| l.strip_prefix("syscw:"))
        .and_then(|v| v.trim().parse().ok())
}

#[cfg(test)]
mod tests;
