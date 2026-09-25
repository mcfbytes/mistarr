//! Work on a copy of the database in RAM, written back whole; see `docs/ARCHITECTURE.md` "DAT import in RAM".

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rusqlite::backup::{Backup, StepResult};
use rusqlite::{Connection, ErrorCode};

use super::{sibling, Db, HeldWriter};
use crate::error::{Error, Result};

/// Bytes per `write` when the copy goes back to the card; a sync mount flushes each once.
pub const CHUNK_BYTES: usize = 1024 * 1024;

/// Pages the copy into RAM takes per backup step, 1 MiB of 4 KiB pages.
const COPY_PAGES: std::ffi::c_int = 256;

/// Room the copy needs beyond its own size and half again for growth and its WAL.
const MARGIN_BYTES: u64 = 32 * 1024 * 1024;

/// Suffix of the copy written beside the database and then renamed over it.
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
    Fallback(String),
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

/// Bytes a copy of a `size`-byte database needs in RAM: the copy, half again for what
/// the import adds and its WAL, and [`MARGIN_BYTES`] for temporary files.
///
/// ```
/// assert_eq!(mistarr_server::db::ram::need(64 << 20), (96 + 32) << 20);
/// ```
#[must_use]
pub fn need(size: u64) -> u64 {
    size.saturating_add(size / 2).saturating_add(MARGIN_BYTES)
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

/// Why a `size`-byte database cannot be copied into RAM under `budget` keeping `floor`
/// bytes available, or `None` when it can.
///
/// ```
/// use mistarr_server::db::ram::{refusal, Budget};
/// let roomy = Budget { available: Some(1 << 30), ram_room: Some(1 << 30), card_room: Some(1 << 30) };
/// assert_eq!(refusal(&roomy, 40 << 20, 128 << 20), None);
/// assert!(refusal(&roomy, 40 << 20, 1 << 30).is_some());
/// ```
#[must_use]
pub fn refusal(budget: &Budget, size: u64, floor: u64) -> Option<String> {
    let need = need(size);
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

/// Copies the database `held` writes into RAM, runs `work` on the copy and, when `work`
/// asks for it by returning `true` beside its value, writes the copy back beside the
/// database in [`CHUNK_BYTES`] writes, syncs it and swaps it in with
/// [`HeldWriter::replace_file`]. Until the swap the card file is never written, except
/// by the checkpoint that empties its WAL first. The working directory goes on every
/// path out. Memory or room short at the start, or running out on the way, returns
/// [`Ram::Fallback`] with the card file as it was.
///
/// # Errors
///
/// Whatever `watch` or `work` fails with, such as [`Error::Cancelled`], or the I/O or
/// SQLite failure that stopped a step; the card file is untouched unless the swap failed
/// after its rename.
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
        return Ok(Ram::Fallback(
            "a reader kept the database's WAL from being emptied".to_owned(),
        ));
    }
    let size = fs::metadata(&path)?.len();
    fs::create_dir_all(&plan.dir).ok();
    if let Some(reason) = refusal(&Budget::read(&plan.dir, &path), size, plan.floor) {
        return Ok(Ram::Fallback(reason));
    }
    let work_dir = match WorkDir::create(&plan.dir, &path, plan.job) {
        Ok(d) => d,
        Err(e) => {
            return Ok(Ram::Fallback(format!(
                "cannot make the memory directory: {e}"
            )))
        }
    };
    let copy = work_dir.0.join(COPY_NAME);

    watch.phase(Phase::Copying);
    let started = Instant::now();
    if let Err(reason) = full_as_reason(copy_in(held.conn(), &copy, watch), "copying")? {
        return Ok(Ram::Fallback(reason));
    }
    // The backup filled the idle writer's cache; the import's own cache needs the room.
    held.conn().execute_batch("PRAGMA shrink_memory")?;
    report.copy_in = started.elapsed();

    watch.phase(Phase::Importing);
    let started = Instant::now();
    let done = Db::open(&copy).and_then(|db| {
        let out = work(&db);
        let closed = db.close();
        let v = out?;
        closed.map(|()| v)
    });
    let (value, write) = match full_as_reason(done, "importing")? {
        Ok(v) => v,
        Err(reason) => return Ok(Ram::Fallback(reason)),
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
        return Ok(Ram::Fallback(format!(
            "the card has no room for a second copy of the database, {} MiB",
            bytes.div_ceil(MIB)
        )));
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
            return Ok(Ram::Fallback(SHARED.to_owned()));
        }
        Err(e) => {
            remove_new(&new);
            if storage_full(&e) {
                return Ok(Ram::Fallback(format!(
                    "the card ran out of room writing the database: {e}"
                )));
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
/// from `/proc/<pid>/fd`; a process whose descriptors cannot be read is not counted.
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
/// the card file untouched unless the swap failed after its rename.
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
    let size = fs::metadata(db)?.len();
    fs::create_dir_all(&plan.dir).ok();
    if let Some(reason) = refusal(&Budget::read(&plan.dir, db), size, plan.floor) {
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
    if let Err(reason) = full_as_reason(
        match steps {
            Some(s) => Db::open_counting(&copy, s),
            None => Db::open(&copy),
        }
        .and_then(Db::close),
        "migrating",
    )? {
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

/// True for failures that mean the RAM directory, the memory or the card is full.
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

/// Removes what an earlier run left: `db`'s `.new` copy, never trusted since a power
/// loss may have cut it short, and working directories of `db` under `dir`. Run before
/// the database opens, under the data directory's lock. Returns how many it removed.
///
/// ```
/// let dir = tempfile::tempdir().unwrap();
/// let db = dir.path().join("m.db");
/// std::fs::write(dir.path().join("m.db.new"), b"partial").unwrap();
/// assert_eq!(mistarr_server::db::ram::clean_stale(&db, &dir.path().join("ram")), 1);
/// ```
#[must_use]
pub fn clean_stale(db: &Path, dir: &Path) -> usize {
    let mut removed = 0;
    let new = sibling(db, NEW_SUFFIX);
    match fs::remove_file(&new) {
        Ok(()) => {
            tracing::info!(file = %new.display(), "removed an unfinished database copy");
            removed += 1;
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!(error = %e, "cannot remove an unfinished database copy"),
    }
    let prefix = work_prefix(db);
    let Ok(entries) = fs::read_dir(dir) else {
        return removed;
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
    removed
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
