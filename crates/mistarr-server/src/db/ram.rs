//! Work on a copy of the database in RAM, written back whole; see `docs/ARCHITECTURE.md` "DAT import in RAM".

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rusqlite::backup::{Backup, StepResult};
use rusqlite::{Connection, ErrorCode};

use super::ids::JobId;
use super::{sibling, Db, HeldWriter};
use crate::error::{Error, Result};
use crate::status;
use swap::{OLD_SUFFIX, SWAP_SUFFIX};

pub mod swap;

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

/// The compacted copy's file name inside the working directory.
const COMPACT_NAME: &str = "compact.db";

/// Memory beyond the twin's pages that a compaction needs: the page caches of the
/// `VACUUM` connection and slack. The copy is not counted: it is in `MemAvailable` already.
const COMPACT_MARGIN_BYTES: u64 = 16 * MIB;

/// Free pages, in percent of the copy's pages, above which the copy is compacted.
const COMPACT_ABOVE_PERCENT: u64 = 25;

/// Where the copy goes and how much memory must stay available beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The RAM-backed directory the working directory is made in.
    pub dir: PathBuf,
    /// Bytes of `MemAvailable` kept free on top of what the copy needs.
    pub floor: u64,
    /// Names the working directory: the job's id, `None` for a migration.
    pub job: Option<JobId>,
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
    /// The copy is rewritten without its free pages, before the write-back.
    Compacting,
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
            Self::Compacting => "compacting the database",
            Self::Writing => "writing the database to the card",
        }
    }
}

impl Phase {
    /// The phase as a nonzero byte, 0 being no phase, for a counter shared between threads.
    pub(crate) fn code(self) -> u8 {
        match self {
            Self::Copying => 1,
            Self::Importing => 2,
            Self::Compacting => 3,
            Self::Writing => 4,
        }
    }

    /// The phase [`Phase::code`] gave `code`, or `None` for 0 and anything unknown.
    pub(crate) fn from_code(code: u8) -> Option<Self> {
        [
            Self::Copying,
            Self::Importing,
            Self::Compacting,
            Self::Writing,
        ]
        .into_iter()
        .find(|p| p.code() == code)
    }
}

/// What [`run`] tells its caller as it goes, and asks between steps.
pub trait Watch {
    /// Called as `phase` starts.
    fn phase(&mut self, phase: Phase);

    /// Called when the work that announced phases is over, whatever came of it.
    fn finished(&mut self) {}

    /// The counter a connection of the work bumps as it runs, for a caller whose progress
    /// report moves with it.
    fn steps(&self) -> Option<&crate::migrating::Steps> {
        None
    }

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
        Self {
            available: status::mem_available_bytes(),
            ram_room: status::free_bytes(dir),
            card_room: db.parent().and_then(status::free_bytes),
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
    refusal_cause(budget, size, input, floor).map(|r| r.reason)
}

/// What kept a copy from being made, and the words for the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    /// The shortage.
    pub cause: Cause,
    /// The measurements behind it.
    pub reason: String,
}

/// What a copy in RAM ran short of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Cause {
    /// `MemAvailable` over the floor.
    Memory,
    /// Free bytes in the memory directory.
    MemoryDir,
    /// Room beside the database for the second copy.
    Card,
}

/// [`refusal`] with its cause, checked in the order memory, memory directory, card.
///
/// ```
/// use mistarr_server::db::ram::{refusal_cause, Budget, Cause};
/// let roomy = Budget { available: Some(1 << 30), ram_room: Some(1 << 30), card_room: Some(1 << 30) };
/// assert_eq!(refusal_cause(&roomy, 40 << 20, 0, 128 << 20), None);
/// let card = Budget { card_room: Some(0), ..roomy };
/// assert_eq!(refusal_cause(&card, 40 << 20, 0, 128 << 20).map(|r| r.cause), Some(Cause::Card));
/// let dir = Budget { ram_room: Some(0), ..roomy };
/// assert_eq!(refusal_cause(&dir, 40 << 20, 0, 128 << 20).map(|r| r.cause), Some(Cause::MemoryDir));
/// assert_eq!(refusal_cause(&roomy, 40 << 20, 0, 1 << 30).map(|r| r.cause), Some(Cause::Memory));
/// ```
#[must_use]
pub fn refusal_cause(budget: &Budget, size: u64, input: u64, floor: u64) -> Option<Refused> {
    let need = need(size, input);
    let mib = |b: u64| b.div_ceil(MIB);
    let Some(available) = budget.available else {
        return Some(refused(
            Cause::Memory,
            "the memory available cannot be read".to_owned(),
        ));
    };
    if available < need.saturating_add(floor) {
        return Some(refused(
            Cause::Memory,
            format!(
                "{} MiB of memory available, {} MiB needed: {} for the copy and {} kept free",
                mib(available),
                mib(need.saturating_add(floor)),
                mib(need),
                mib(floor)
            ),
        ));
    }
    match budget.ram_room {
        None => {
            let why = "the free space in the memory directory cannot be read".to_owned();
            return Some(refused(Cause::MemoryDir, why));
        }
        Some(room) if room < need => {
            return Some(refused(
                Cause::MemoryDir,
                format!(
                    "{} MiB free in the memory directory, {} MiB needed",
                    mib(room),
                    mib(need)
                ),
            ));
        }
        Some(_) => {}
    }
    match budget.card_room {
        Some(room) if room < size.saturating_add(CHUNK_BYTES as u64) => Some(refused(
            Cause::Card,
            format!(
                "{} MiB free on the card, {} MiB needed for a second copy of the database",
                mib(room),
                mib(size) + 1
            ),
        )),
        _ => None,
    }
}

fn refused(cause: Cause, reason: String) -> Refused {
    Refused { cause, reason }
}

/// How many of the first `inputs` (uncompressed DAT bytes of each file, in order) one
/// copy of a `size`-byte database can load together under `budget` keeping `floor` bytes
/// available: the most whose summed input [`refusal`] accepts, and at least 1, since a
/// single file the copy cannot hold is left to [`run`] to fall back on the card.
///
/// ```
/// use mistarr_server::db::ram::{fit, Budget};
/// let budget = Budget { available: Some(400 << 20), ram_room: Some(400 << 20), card_room: Some(1 << 30) };
/// let sizes = [10 << 20; 8];
/// assert_eq!(fit(&budget, 40 << 20, 128 << 20, &sizes), 3);
/// assert_eq!(fit(&budget, 40 << 20, 128 << 20, &[500 << 20]), 1);
/// assert_eq!(fit(&budget, 40 << 20, 128 << 20, &[]), 0);
/// ```
#[must_use]
pub fn fit(budget: &Budget, size: u64, floor: u64, inputs: &[u64]) -> usize {
    let mut total = 0u64;
    let mut count = 0;
    for &input in inputs {
        total = total.saturating_add(input);
        if count > 0 && refusal(budget, size, total, floor).is_some() {
            break;
        }
        count += 1;
    }
    count
}

/// [`fit`] under the memory and room [`Budget::read`] finds for a copy in `dir` of the
/// database file `db`.
///
/// # Errors
///
/// [`Error::Io`] when `db` cannot be measured.
///
/// ```
/// let dir = tempfile::tempdir().unwrap();
/// let db = dir.path().join("m.db");
/// std::fs::write(&db, b"x").unwrap();
/// let n = mistarr_server::db::ram::fit_now(dir.path(), &db, u64::MAX, &[1, 1]).unwrap();
/// assert_eq!(n, 1, "no memory is enough above the floor, so only the first file");
/// ```
pub fn fit_now(dir: &Path, db: &Path, floor: u64, inputs: &[u64]) -> Result<usize> {
    let size = file_len(db)?;
    Ok(fit(&Budget::read(dir, db), size, floor, inputs))
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
    if let Err(e) = super::tempdir::private_dir(dir) {
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
    match status::mem_available_bytes() {
        Some(a) if a < floor => Err(Error::NoRoom(format!(
            "memory available fell to {} MiB, under the {} MiB kept free",
            a.div_ceil(MIB),
            floor.div_ceil(MIB)
        ))),
        _ => Ok(()),
    }
}

/// Copies the database `held` writes into RAM, runs `work` on the copy and, when `work`
/// asks for it by returning `true` beside its value, writes the copy, compacted when it
/// holds many free pages and memory allows, back beside the
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
/// [`Error::Reopen`] or [`Error::Poisoned`] when a swap, the first or that of a later
/// compaction, left no connection open, never a fallback. A compaction after the swap
/// that stops any other way leaves the written-back file and no error.
pub fn run<T>(
    held: &mut HeldWriter<'_>,
    plan: &Plan,
    watch: &mut dyn Watch,
    work: impl FnOnce(&Db) -> Result<(T, bool)>,
) -> Result<Ram<T>> {
    run_with(held, plan, watch, work, &mut live_budget)
}

/// [`run`] reading the memory for a compaction through `probe`, which tests script.
fn run_with<T>(
    held: &mut HeldWriter<'_>,
    plan: &Plan,
    watch: &mut dyn Watch,
    work: impl FnOnce(&Db) -> Result<(T, bool)>,
    probe: Probe<'_>,
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
    let size = file_len(&path)?;
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

    let started = Instant::now();
    let (source, deferred) = compact_with(&copy, &work_dir.0, plan.floor, probe, &mut || {
        watch.phase(Phase::Compacting);
    });
    watch.phase(Phase::Writing);
    let bytes = file_len(&source)?;
    if let Some(reason) = card_refusal(&path, bytes) {
        return Ok(Ram::fallback(why::CARD, reason));
    }
    let new = sibling(&path, NEW_SUFFIX);
    let (written, back) = counted(|| {
        write_swap(
            &path,
            &source,
            &new,
            &mut || watch.between(Phase::Writing),
            &mut |n| held.replace_file(n),
        )
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
    report.bytes = bytes;
    report.card_writes = first.zip(back).map(|(a, b)| a + b);
    if deferred {
        remove_files(&copy);
        compact_held(held, &work_dir.0, plan.floor, probe, watch, &mut report)?;
    }
    report.write_back = started.elapsed();
    drop(work_dir);
    Ok(Ram::Done(value, report))
}

/// Why the card beside `path` cannot take a second copy of `bytes`, or `None` when it can.
fn card_refusal(path: &Path, bytes: u64) -> Option<String> {
    let room = path.parent().and_then(status::free_bytes)?;
    (room < bytes.saturating_add(CHUNK_BYTES as u64)).then(|| {
        format!(
            "the card has no room for a second copy of the database, {} MiB",
            bytes.div_ceil(MIB)
        )
    })
}

/// Compacts the card file `held` writes after the swap and swaps the twin in, adding
/// its size and writes to `report`; any refusal or failure leaves the file as it is,
/// except [`Error::Reopen`] and [`Error::Poisoned`], which leave no usable connection.
fn compact_held(
    held: &mut HeldWriter<'_>,
    work_dir: &Path,
    floor: u64,
    probe: Probe<'_>,
    watch: &mut dyn Watch,
    report: &mut Report,
) -> Result<()> {
    let path = held.path().to_path_buf();
    let twin = twin_of(held.conn(), work_dir, floor, probe, &mut || {
        watch.phase(Phase::Compacting);
    });
    let Twin::Made(twin, before, after) = twin else {
        return Ok(());
    };
    watch.phase(Phase::Writing);
    let (swapped, extra) = counted(|| {
        swap_twin(
            &path,
            &twin,
            &mut || watch.between(Phase::Writing),
            &mut |n| held.replace_file(n),
        )
    });
    if let Some(bytes) = swapped? {
        log_compacted(
            before,
            after,
            "compacted the database after writing it back",
        );
        report.bytes = bytes;
        report.card_writes = report.card_writes.zip(extra).map(|(a, b)| a + b);
    }
    Ok(())
}

/// The memory and room a compaction may use, read afresh on each call.
type Probe<'a> = &'a mut dyn FnMut(&Path) -> Budget;

/// Reads `MemAvailable` and the room in `dir`, the real [`Probe`].
fn live_budget(dir: &Path) -> Budget {
    Budget {
        available: status::mem_available_bytes(),
        ram_room: status::free_bytes(dir),
        card_room: None,
    }
}

/// Why no twin was made.
enum Declined {
    /// Memory or room in the memory directory cannot hold it.
    Short(String),
    /// A step failed or a check did not hold.
    Failed(String),
}

impl From<String> for Declined {
    fn from(reason: String) -> Self {
        Self::Failed(reason)
    }
}

impl Declined {
    fn reason(self) -> String {
        match self {
            Self::Short(r) | Self::Failed(r) => r,
        }
    }
}

/// What [`twin_of`] came to.
enum Twin {
    /// A verified twin in the working directory, with the sizes before and after.
    Made(PathBuf, u64, u64),
    /// Too few free pages to be worth it.
    Few,
    /// Memory or room fell short.
    Short,
    /// A step or a check failed.
    Failed,
}

/// Compacts the database `conn` reads into the working directory, logging why not.
fn twin_of(
    conn: &Connection,
    work_dir: &Path,
    floor: u64,
    probe: Probe<'_>,
    started: &mut dyn FnMut(),
) -> Twin {
    let out = work_dir.join(COMPACT_NAME);
    let budget = probe(work_dir);
    match compacted_from(conn, &out, floor, &budget, started) {
        Ok(Some((before, after))) => Twin::Made(out, before, after),
        Ok(None) => Twin::Few,
        Err(why) => {
            let short = matches!(why, Declined::Short(_));
            tracing::info!(reason = why.reason(), "not compacting the database");
            let _ = fs::remove_file(&out);
            if short {
                Twin::Short
            } else {
                Twin::Failed
            }
        }
    }
}

fn log_compacted(before: u64, after: u64, message: &str) {
    tracing::info!(
        before_mib = before.div_ceil(MIB),
        after_mib = after.div_ceil(MIB),
        "{message}"
    );
}

/// Removes `file` and the journal, WAL and shared-memory files beside it.
fn remove_files(file: &Path) {
    for suffix in ["", "-journal", "-wal", "-shm"] {
        let _ = fs::remove_file(sibling(file, suffix));
    }
}

/// The file to write back for the finished `copy` in `work_dir`: a compacted twin when
/// the copy's free pages exceed [`COMPACT_ABOVE_PERCENT`] and memory and room cover it,
/// else `copy` itself, beside whether the twin was refused for memory or room, which a
/// compaction from the card after the swap may still make. `started` is called just
/// before the compaction runs. Any failure leaves `copy` as the answer, so compacting
/// never fails a write-back.
fn compact_with(
    copy: &Path,
    work_dir: &Path,
    floor: u64,
    probe: Probe<'_>,
    started: &mut dyn FnMut(),
) -> (PathBuf, bool) {
    let twin = match Connection::open(copy) {
        Ok(conn) => twin_of(&conn, work_dir, floor, probe, started),
        Err(e) => {
            tracing::info!(error = %e, "cannot open the copy to compact it");
            Twin::Failed
        }
    };
    match twin {
        Twin::Made(out, before, after) => {
            log_compacted(
                before,
                after,
                "compacted the database before writing it back",
            );
            // The twin is whole and closed; the copy would only hold RAM through the write-back.
            remove_files(copy);
            (out, false)
        }
        Twin::Short => (copy.to_path_buf(), true),
        Twin::Few | Twin::Failed => (copy.to_path_buf(), false),
    }
}

/// Compacts the database `conn` reads into `out` when worth it and affordable under
/// `budget`, returning the sizes before and after, or `None` when the free pages are few.
fn compacted_from(
    conn: &Connection,
    out: &Path,
    floor: u64,
    budget: &Budget,
    started: &mut dyn FnMut(),
) -> std::result::Result<Option<(u64, u64)>, Declined> {
    let pragma = |name: &str| -> std::result::Result<i64, String> {
        conn.pragma_query_value(None, name, |r| r.get(0))
            .map_err(|e| format!("cannot read {name}: {e}"))
    };
    let (pages, free, size) = (
        u64::try_from(pragma("page_count")?).unwrap_or(0),
        u64::try_from(pragma("freelist_count")?).unwrap_or(0),
        u64::try_from(pragma("page_size")?).unwrap_or(0),
    );
    if free.saturating_mul(100) <= pages.saturating_mul(COMPACT_ABOVE_PERCENT) {
        tracing::info!(
            free,
            pages,
            "the database has too few free pages to compact"
        );
        return Ok(None);
    }
    let live = (pages - free).saturating_mul(size);
    let before = pages.saturating_mul(size);
    compaction_refusal(budget, floor, live).map_err(Declined::Short)?;
    let want = ["user_version", "application_id", "page_size", "auto_vacuum"];
    let mut kept = Vec::new();
    for name in want {
        kept.push(pragma(name)?);
    }
    started();
    let target = out
        .to_str()
        .ok_or_else(|| "the working directory is not text".to_owned())?;
    conn.execute("VACUUM INTO ?1", [target])
        .map_err(|e| format!("VACUUM failed: {e}"))?;
    // The source is in WAL mode, so the card file opens without a write.
    let twin = Connection::open(out).map_err(|e| format!("cannot open the compacted copy: {e}"))?;
    twin.pragma_update_and_check(None, "journal_mode", "WAL", |_| Ok(()))
        .map_err(|e| format!("cannot set WAL mode on the compacted copy: {e}"))?;
    for (name, was) in want.iter().zip(&kept) {
        let now: i64 = twin
            .pragma_query_value(None, name, |r| r.get(0))
            .map_err(|e| format!("cannot read {name} of the compacted copy: {e}"))?;
        if now != *was {
            return Err(format!("{name} changed from {was} to {now}").into());
        }
    }
    twin.close()
        .map_err(|(_, e)| format!("cannot close the compacted copy: {e}"))?;
    let after = fs::metadata(out)
        .map_err(|e| format!("cannot measure it: {e}"))?
        .len();
    Ok(Some((before, after)))
}

/// Writes `source` beside `path` as `new`, syncs it, waits for other processes to let go
/// of the database and has `install` put it in place; false when one did not. The one
/// write-back both the copy and a later twin go through.
fn write_swap(
    path: &Path,
    source: &Path,
    new: &Path,
    between: &mut dyn FnMut() -> Result<()>,
    install: &mut dyn FnMut(&Path) -> Result<()>,
) -> Result<bool> {
    write_new(source, new, between)?;
    if !wait_unshared(path, between)? {
        return Ok(false);
    }
    install(new).map(|()| true)
}

/// Compacts the card file at `db` after its swap, as [`compact_held`] does where no
/// connection of ours exists; every failure, `Reopen` included, is a warning. `watch`
/// hears the phases and each written chunk, then [`Watch::finished`]; its
/// [`Watch::steps`] count the `VACUUM`'s progress. `message` is the line logged once
/// the twin is in place.
fn compact_card(
    db: &Path,
    work_dir: &Path,
    floor: u64,
    probe: Probe<'_>,
    watch: &mut dyn Watch,
    report: &mut Report,
    message: &str,
) {
    compact_card_phases(db, work_dir, floor, probe, watch, report, message);
    watch.finished();
}

fn compact_card_phases(
    db: &Path,
    work_dir: &Path,
    floor: u64,
    probe: Probe<'_>,
    watch: &mut dyn Watch,
    report: &mut Report,
    message: &str,
) {
    let steps = watch.steps().cloned();
    let Some((twin, before, after)) =
        twin_from_card(db, work_dir, floor, probe, steps.as_ref(), &mut || {
            watch.phase(Phase::Compacting);
        })
    else {
        return;
    };
    watch.phase(Phase::Writing);
    let (swapped, extra) = counted(|| {
        swap_twin(db, &twin, &mut || watch.between(Phase::Writing), &mut |n| {
            swap::install_file(db, n)
        })
    });
    match swapped {
        Ok(Some(bytes)) => {
            log_compacted(before, after, message);
            report.bytes = bytes;
            report.card_writes = report.card_writes.zip(extra).map(|(a, b)| a + b);
        }
        Ok(None) => {}
        Err(e) => tracing::warn!(error = %e, "not compacting the database"),
    }
}

/// Opens the card database `db`, compacts it into the working directory and closes it
/// again, so the swap finds no connection of ours.
fn twin_from_card(
    db: &Path,
    work_dir: &Path,
    floor: u64,
    probe: Probe<'_>,
    steps: Option<&crate::migrating::Steps>,
    started: &mut dyn FnMut(),
) -> Option<(PathBuf, u64, u64)> {
    let conn =
        match Connection::open(db).and_then(|c| c.busy_timeout(super::BUSY_TIMEOUT).map(|()| c)) {
            Ok(c) => c,
            Err(e) => {
                tracing::info!(error = %e, "cannot open the database to compact it");
                return None;
            }
        };
    if let Some(steps) = steps {
        if let Err(e) = super::count_steps(&conn, steps) {
            tracing::info!(error = %e, "cannot count the compaction's progress");
        }
    }
    let twin = twin_of(&conn, work_dir, floor, probe, started);
    if let Err(e) = super::close_connection(conn) {
        tracing::info!(error = %e, "not compacting the database");
        return None;
    }
    match twin {
        Twin::Made(out, before, after) => Some((out, before, after)),
        _ => None,
    }
}

/// Writes the verified `twin` of the database at `path` back and swaps it in through
/// `install`, returning its size. A refusal, a failure, or a stop by `between` such as a
/// cancel leaves the file at `path` as it is, logged at info, so a succeeded import or
/// migration is not undone.
///
/// # Errors
///
/// [`Error::Reopen`] and [`Error::Poisoned`], which leave connections that fail every
/// statement.
fn swap_twin(
    path: &Path,
    twin: &Path,
    between: &mut dyn FnMut() -> Result<()>,
    install: &mut dyn FnMut(&Path) -> Result<()>,
) -> Result<Option<u64>> {
    let Ok(bytes) = file_len(twin) else {
        return Ok(None);
    };
    if let Some(reason) = card_refusal(path, bytes) {
        tracing::info!(reason, "not compacting the database");
        return Ok(None);
    }
    let new = sibling(path, NEW_SUFFIX);
    match write_swap(path, twin, &new, between, install) {
        Ok(true) => Ok(Some(bytes)),
        Ok(false) => {
            remove_new(&new);
            tracing::info!(reason = SHARED, "not compacting the database");
            Ok(None)
        }
        Err(e) => {
            remove_new(&new);
            if matches!(e, Error::Reopen(_) | Error::Poisoned) {
                return Err(e);
            }
            tracing::info!(error = %e, "not compacting the database");
            Ok(None)
        }
    }
}

/// Why `budget` cannot hold a compacted twin of `live` bytes while keeping `floor` bytes
/// of memory available, or `Ok` when it can. The copy, already in RAM, is not charged.
fn compaction_refusal(budget: &Budget, floor: u64, live: u64) -> std::result::Result<(), String> {
    let Some(available) = budget.available else {
        return Err("the memory available cannot be read".to_owned());
    };
    let want = floor
        .saturating_add(live)
        .saturating_add(COMPACT_MARGIN_BYTES);
    if available < want {
        return Err(format!(
            "{} MiB of memory available, {} MiB needed to compact",
            available / MIB,
            want.div_ceil(MIB)
        ));
    }
    match budget.ram_room {
        Some(room) if room >= live => Ok(()),
        Some(room) => Err(format!(
            "{} MiB free in the memory directory, {} MiB needed to compact",
            room / MIB,
            live.div_ceil(MIB)
        )),
        None => Err("the free space in the memory directory cannot be read".to_owned()),
    }
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

/// How a startup migration in RAM ended when it did not fail.
enum Outcome {
    /// Migrated in RAM and swapped in.
    Ran(Report),
    /// Nothing pending, or something other than memory stood in the way.
    Left,
    /// Memory or room in the memory directory was too short to copy the database.
    Short,
}

/// What a startup migration did, from [`migrate_at_start`].
#[derive(Debug)]
#[non_exhaustive]
pub enum Migrated {
    /// Nothing was migrated here; the server's open applies what is pending.
    Nothing,
    /// Migrated in RAM and written back.
    Ram(Report),
    /// Memory was too short for a copy, so the card file was migrated in place, and then
    /// compacted when worth it and affordable; `report.bytes` is its size afterwards.
    InPlace {
        /// Size of the migrated card file before any compaction.
        before: u64,
        /// What the migration and the compaction cost.
        report: Report,
    },
    /// Memory was too short for a copy and the migration of the card file failed. Opening
    /// the database would only run it again and fail the same way, so the start ends
    /// with this error.
    Failed(Error),
}

/// Applies pending migrations to a copy of `db` in RAM and swaps it in, as a DAT import
/// does; run at startup before any connection of the server opens, under the data
/// directory's lock. When only memory or room in the memory directory kept the copy from
/// being made, the card file is migrated in place, through a writer and a reader that
/// are closed again, and compacted as after a write-back: free pages above
/// [`COMPACT_ABOVE_PERCENT`], the budget read afresh, `VACUUM INTO` the working
/// directory, verification, write-back and swap. [`Migrated::Nothing`] when nothing is
/// pending, there is no database yet, or another process holding the file, a reader
/// keeping the WAL or the card's room stand in the way, for which it logs why; the
/// server's open then migrates the card file itself. `watch` hears the phases and each
/// chunk written by a compaction, and its [`Watch::steps`] move with the migration.
/// A refusal or failure of a compaction leaves the migrated file and is logged.
///
/// # Errors
///
/// [`Error::SchemaTooNew`] before anything is written, else the failure of a step of the
/// copy, with the card file untouched unless a failed swap left `.old`, which the next
/// start restores. A failed migration of the card file is [`Migrated::Failed`].
pub fn migrate_at_start(db: &Path, plan: &Plan, watch: &mut dyn Watch) -> Result<Migrated> {
    migrate_at_start_with(db, plan, watch, &mut live_budget)
}

/// [`migrate_at_start`] reading the memory for a compaction through `probe`.
fn migrate_at_start_with(
    db: &Path,
    plan: &Plan,
    watch: &mut dyn Watch,
    probe: Probe<'_>,
) -> Result<Migrated> {
    match migrate_outcome(db, plan, watch, probe)? {
        Outcome::Ran(report) => Ok(Migrated::Ram(report)),
        Outcome::Left => Ok(Migrated::Nothing),
        Outcome::Short => Ok(migrate_on_card(db, plan, watch, probe)),
    }
}

/// Migrates the card file `db` through a writer and a reader, closes them and compacts it.
fn migrate_on_card(db: &Path, plan: &Plan, watch: &mut dyn Watch, probe: Probe<'_>) -> Migrated {
    let started = Instant::now();
    let (opened, first) = counted(|| match watch.steps() {
        Some(steps) => Db::open_counting(db, steps),
        None => Db::open(db),
    });
    let opened = match opened {
        Ok(opened) => opened,
        Err(e) => return Migrated::Failed(e),
    };
    if let Err(e) = opened.close() {
        tracing::warn!(error = %e, "migrating the database in place");
        return Migrated::Nothing;
    }
    let before = file_len(db).unwrap_or(0);
    let mut report = Report {
        bytes: before,
        card_writes: first,
        work: started.elapsed(),
        ..Report::default()
    };
    let started = Instant::now();
    match WorkDir::create(&plan.dir, db, plan.job) {
        Ok(work_dir) => {
            let message = "compacted the database migrated in place";
            compact_card(
                db,
                &work_dir.0,
                plan.floor,
                probe,
                watch,
                &mut report,
                message,
            );
        }
        Err(e) => tracing::info!(error = %e, "not compacting the database"),
    }
    report.write_back = started.elapsed();
    Migrated::InPlace { before, report }
}

/// The copy-in-RAM half of [`migrate_at_start`], saying whether memory was what stopped it.
fn migrate_outcome(
    db: &Path,
    plan: &Plan,
    watch: &mut dyn Watch,
    probe: Probe<'_>,
) -> Result<Outcome> {
    let steps = watch.steps().cloned();
    if !db.is_file() {
        return Ok(Outcome::Left);
    }
    let conn = Connection::open(db)?;
    conn.busy_timeout(super::BUSY_TIMEOUT)?;
    let found = super::migrate::check_supported(&conn)?;
    if found == 0 || found >= super::migrate::latest() {
        return Ok(Outcome::Left);
    }
    let mut report = Report::default();
    let (emptied, first) = counted(|| super::wal_emptied(&conn));
    let skip = |reason: &str| {
        tracing::info!(reason, "migrating the database in place");
        Ok(Outcome::Left)
    };
    let short = |reason: &str| {
        tracing::info!(reason, "migrating the database in place");
        Ok(Outcome::Short)
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
    let size = file_len(db)?;
    let budget = Budget::read(&plan.dir, db);
    if let Some(refused) = refusal_cause(&budget, size, 0, plan.floor) {
        return match refused.cause {
            Cause::Card => skip(&refused.reason),
            Cause::Memory | Cause::MemoryDir => short(&refused.reason),
        };
    }
    let Ok(work_dir) = WorkDir::create(&plan.dir, db, plan.job) else {
        return skip("the memory directory cannot be made");
    };
    let copy = work_dir.0.join(COPY_NAME);
    let started = Instant::now();
    let copied = copy_in(&conn, &copy, &mut ());
    super::close_connection(conn)?;
    if let Err(reason) = full_as_reason(copied, "copying")? {
        return short(&reason);
    }
    report.copy_in = started.elapsed();
    let started = Instant::now();
    if let Err(reason) = full_as_reason(
        Db::open_copy(&copy, steps.as_ref()).and_then(Db::close),
        "migrating",
    )? {
        return short(&reason);
    }
    report.work = started.elapsed();
    let started = Instant::now();
    let (source, deferred) = compact_with(&copy, &work_dir.0, plan.floor, probe, &mut || {});
    let bytes = file_len(&source)?;
    if status::free_bytes(db.parent().unwrap_or(Path::new(".")))
        .is_some_and(|room| room < bytes.saturating_add(CHUNK_BYTES as u64))
    {
        return skip("the card has no room for a second copy of the database");
    }
    let new = sibling(db, NEW_SUFFIX);
    let (written, back) = counted(|| {
        write_swap(db, &source, &new, &mut || Ok(()), &mut |n| {
            swap::install_file(db, n)
        })
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
    report.bytes = bytes;
    report.card_writes = first.zip(back).map(|(a, b)| a + b);
    if deferred {
        remove_files(&copy);
        let message = "compacted the database after writing it back";
        let floor = plan.floor;
        compact_card(db, &work_dir.0, floor, probe, watch, &mut report, message);
    }
    report.write_back = started.elapsed();
    Ok(Outcome::Ran(report))
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
    if let Some(io) = e.io() {
        return matches!(io.raw_os_error(), Some(12 | 28 | 122))
            || matches!(
                io.kind(),
                io::ErrorKind::StorageFull | io::ErrorKind::OutOfMemory
            );
    }
    match e {
        Error::Db(rusqlite::Error::SqliteFailure(f, _)) => {
            matches!(f.code, ErrorCode::DiskFull | ErrorCode::OutOfMemory)
        }
        Error::NoRoom(_) => true,
        _ => false,
    }
}

/// The length of the file at `path`.
fn file_len(path: &Path) -> Result<u64> {
    Ok(fs::metadata(path).map_err(Error::io_at(path))?.len())
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
    let mut from = File::open(src).map_err(Error::io_at(src))?;
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
/// [`Error::Io`] or [`Error::File`] when a swap cut short cannot be finished; the
/// database cannot open.
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

/// The step of [`swap::install_file`] a crash stopped at, from the files present.
/// Beside `db`, `.old` is removed. Without `db`, a `.new` beside `.old` or the `.swap`
/// marker that passes SQLite's `quick_check` is renamed in and `.old` removed, since
/// `.new` is synced before the swap begins and a rename on exFAT may leave neither of
/// its names readable; else `.old` is renamed back. The marker goes once `db` is back.
/// Returns how many files it removed.
///
/// # Errors
///
/// [`Error::Io`] naming `.new` when it fails the check or stands alone without the
/// marker, which only a stale copy does; [`Error::File`] naming the file a rename or
/// removal failed on.
fn finish_swap(db: &Path) -> Result<usize> {
    let (old, new) = (sibling(db, OLD_SUFFIX), sibling(db, NEW_SUFFIX));
    let marker = sibling(db, SWAP_SUFFIX);
    let mut removed = 0;
    if db.exists() {
        if old.exists() {
            fs::remove_file(&old).map_err(Error::io_at(&old))?;
            tracing::info!(file = %old.display(), "removed the database a finished swap replaced");
            removed += 1;
        }
    } else if new.exists() {
        if !old.exists() && !marker.exists() {
            return Err(io::Error::other(format!(
                "{} is missing and {} stands alone without a swap under way, so it is a \
                 stale copy; move it aside to start afresh, or restore mistarr.db.prev",
                db.display(),
                new.display()
            ))
            .into());
        }
        check_whole(&new)?;
        fs::rename(&new, db).map_err(Error::io_at(&new))?;
        swap::sync_parent(db);
        if old.exists() {
            fs::remove_file(&old).map_err(Error::io_at(&old))?;
            removed += 1;
        }
        tracing::info!("finished swapping in the database written from RAM");
    } else if old.exists() {
        fs::rename(&old, db).map_err(Error::io_at(&old))?;
        tracing::warn!("put the old database back; the swap had lost its new file");
    }
    swap::sync_parent(db);
    if db.exists() && marker.exists() {
        fs::remove_file(&marker).map_err(Error::io_at(&marker))?;
        swap::sync_parent(db);
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
    fn create(dir: &Path, db: &Path, job: Option<JobId>) -> io::Result<Self> {
        let name = job.map_or_else(|| "migration".to_owned(), |j| j.to_string());
        let path = dir.join(format!("{}{name}", work_prefix(db)));
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
