//! SQLite access; the schema is `docs/DATA-MODEL.md`. All SQL lives in this module's children.

pub mod arcade;
pub mod candidates;
pub mod chd;
pub mod dat_stage;
pub mod dats;
pub mod deferred;
pub mod downloads;
pub mod downloads_import;
pub mod files;
pub mod groups;
pub mod imports;
pub mod jobs;
pub mod launch;
pub mod migrate;
pub mod platforms;
pub mod ram;
pub mod settings;
pub mod sources;
pub mod system;
pub mod titles;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rusqlite::{Connection, Transaction};

use crate::error::{Error, Result};

/// Page cache per connection in KiB; two connections share the 2 MiB budget.
const CACHE_KIB: i64 = 1024;

/// The writer's page cache in KiB while a bulk write holds it: a DAT load's dirty pages
/// stay in memory until its commit writes each once; `docs/ARCHITECTURE.md` "Writes on
/// a sync mount" has the measurement.
const BULK_CACHE_KIB: i64 = 8 * 1024;

/// WAL pages written before an automatic checkpoint, about 1 MiB of 4 KiB pages.
const WAL_AUTOCHECKPOINT: i64 = 256;

/// Size the WAL file is cut back to after a checkpoint, in bytes.
const JOURNAL_SIZE_LIMIT: i64 = 1024 * 1024;

/// Heap SQLite tries to stay under, process-wide, by shedding cached pages.
const SOFT_HEAP_LIMIT: i64 = 8 * 1024 * 1024;

/// [`SOFT_HEAP_LIMIT`] while a bulk write holds the writer: room for its cache on top.
const BULK_HEAP_LIMIT: i64 = SOFT_HEAP_LIMIT + BULK_CACHE_KIB * 1024;

/// How long a statement waits on a lock held by the other connection.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// The database: one writer and one reader connection, each behind a mutex.
/// WAL lets the reader run while the writer holds a transaction.
#[derive(Clone)]
pub struct Db {
    inner: Arc<Inner>,
}

struct Inner {
    /// One permit, taken before a write reaches the blocking pool, so writers waiting
    /// their turn hold no blocking thread and reads keep running.
    write_turn: Arc<tokio::sync::Semaphore>,
    writer: Mutex<Connection>,
    reader: Mutex<Connection>,
    path: PathBuf,
    /// A copy in RAM: rollback journal, no syncs; [`Db::close`] leaves it in WAL mode.
    scratch: bool,
}

impl Db {
    /// Opens or creates the database at `path` and applies pending migrations.
    ///
    /// # Errors
    ///
    /// [`Error::Db`] when the file cannot be opened or configured,
    /// [`Error::SchemaTooNew`] when a newer mistarr migrated it, and
    /// [`Error::Migration`] when a migration fails.
    ///
    /// ```
    /// let dir = std::env::temp_dir().join(format!("mistarr-doc-db-open-{}", std::process::id()));
    /// std::fs::create_dir_all(&dir).unwrap();
    /// let db = mistarr_server::db::Db::open(&dir.join("t.db")).unwrap();
    /// assert!(db.path().ends_with("t.db"));
    /// ```
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with(path, None)
    }

    /// [`Db::open`], bumping `steps` every [`crate::migrating::STEP_OPS`] SQLite
    /// instructions while it migrates, so a progress report moves only with the migration.
    ///
    /// # Errors
    ///
    /// As [`Db::open`].
    ///
    /// ```
    /// let dir = tempfile::tempdir().unwrap();
    /// let steps = mistarr_server::migrating::Steps::default();
    /// mistarr_server::db::Db::open_counting(&dir.path().join("c.db"), &steps).unwrap();
    /// ```
    pub fn open_counting(path: &Path, steps: &crate::migrating::Steps) -> Result<Self> {
        Self::open_with(path, Some(steps))
    }

    /// [`Db::open`] for a copy in RAM that is thrown away on failure: a rollback journal,
    /// which holds only the pages a transaction overwrites, and no syncs. [`Db::close`]
    /// puts it back in WAL mode, so the card file it becomes opens without a write.
    ///
    /// # Errors
    ///
    /// As [`Db::open`].
    ///
    /// ```
    /// let dir = tempfile::tempdir().unwrap();
    /// let db = mistarr_server::db::Db::open_copy(&dir.path().join("c.db"), None).unwrap();
    /// let mode: String = db
    ///     .read_blocking(|c| Ok(c.pragma_query_value(None, "journal_mode", |r| r.get(0))?))
    ///     .unwrap();
    /// assert_eq!(mode, "delete");
    /// ```
    pub fn open_copy(path: &Path, steps: Option<&crate::migrating::Steps>) -> Result<Self> {
        let (writer, reader) = open_pair(path, steps, true)?;
        Ok(Self::from_pair(path, writer, reader, true))
    }

    fn open_with(path: &Path, steps: Option<&crate::migrating::Steps>) -> Result<Self> {
        let (writer, reader) = open_pair(path, steps, false)?;
        Ok(Self::from_pair(path, writer, reader, false))
    }

    fn from_pair(path: &Path, writer: Connection, reader: Connection, scratch: bool) -> Self {
        Self {
            inner: Arc::new(Inner {
                write_turn: Arc::new(tokio::sync::Semaphore::new(1)),
                writer: Mutex::new(writer),
                reader: Mutex::new(reader),
                path: path.to_path_buf(),
                scratch,
            }),
        }
    }

    /// The database file.
    ///
    /// ```
    /// # let dir = std::env::temp_dir().join(format!("mistarr-doc-db-path-{}", std::process::id()));
    /// # std::fs::create_dir_all(&dir).unwrap();
    /// let db = mistarr_server::db::Db::open(&dir.join("p.db")).unwrap();
    /// assert!(db.path().is_file());
    /// ```
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// Runs `f` on the writer connection on the calling thread.
    ///
    /// # Errors
    ///
    /// Whatever `f` returns, or [`Error::Poisoned`].
    ///
    /// ```
    /// # let dir = std::env::temp_dir().join(format!("mistarr-doc-db-wb-{}", std::process::id()));
    /// # std::fs::create_dir_all(&dir).unwrap();
    /// # let db = mistarr_server::db::Db::open(&dir.join("w.db")).unwrap();
    /// use mistarr_server::db::settings;
    /// db.write_blocking(|c| settings::set(c, "k", "v")).unwrap();
    /// ```
    pub fn write_blocking<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        let mut conn = self.inner.writer.lock().map_err(|_| Error::Poisoned)?;
        let out = f(&mut conn);
        // A failed settle leaves the groups dirty for the next commit; `f`'s own result stands.
        if let Err(e) = settle(&mut conn) {
            tracing::error!(error = %e, "title groups not refreshed after a write");
        }
        out
    }

    /// [`Db::write_blocking`] with the writer's page cache raised while `f` runs; see
    /// [`bulk`].
    ///
    /// # Errors
    ///
    /// Whatever `f` returns, [`Error::Poisoned`], or [`Error::Db`] when the cache
    /// cannot be set or restored.
    ///
    /// ```
    /// # let dir = std::env::temp_dir().join(format!("mistarr-doc-db-bulk-{}", std::process::id()));
    /// # std::fs::create_dir_all(&dir).unwrap();
    /// # let db = mistarr_server::db::Db::open(&dir.join("b.db")).unwrap();
    /// use mistarr_server::db::settings;
    /// db.write_bulk_blocking(|c| settings::set(c, "k", "v")).unwrap();
    /// ```
    pub fn write_bulk_blocking<T>(
        &self,
        f: impl FnOnce(&mut Connection) -> Result<T>,
    ) -> Result<T> {
        self.write_blocking(|c| bulk(c, f))
    }

    /// Runs `f` on the read-only connection on the calling thread.
    ///
    /// # Errors
    ///
    /// Whatever `f` returns, or [`Error::Poisoned`].
    ///
    /// ```
    /// # let dir = std::env::temp_dir().join(format!("mistarr-doc-db-rb-{}", std::process::id()));
    /// # std::fs::create_dir_all(&dir).unwrap();
    /// # let db = mistarr_server::db::Db::open(&dir.join("r.db")).unwrap();
    /// let n = db.read_blocking(mistarr_server::db::migrate::current_version).unwrap();
    /// assert!(n >= 1);
    /// ```
    pub fn read_blocking<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let conn = self.inner.reader.lock().map_err(|_| Error::Poisoned)?;
        f(&conn)
    }

    /// [`Db::write_blocking`] on tokio's blocking pool, once no other async write is running.
    ///
    /// # Errors
    ///
    /// Whatever `f` returns, [`Error::Poisoned`] or [`Error::Task`].
    pub async fn write<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        let turn = Arc::clone(&self.inner.write_turn)
            .acquire_owned()
            .await
            .map_err(|_| Error::Poisoned)?;
        let db = self.clone();
        crate::threads::blocking(crate::threads::label::DB_WRITE, move || {
            let _turn = turn;
            db.write_blocking(f)
        })
        .await
        .map_err(|e| Error::Task(e.to_string()))?
    }

    /// [`Db::write_bulk_blocking`] on tokio's blocking pool, once no other async write
    /// is running.
    ///
    /// # Errors
    ///
    /// Whatever `f` returns, [`Error::Poisoned`], [`Error::Task`] or [`Error::Db`].
    pub async fn write_bulk<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        self.write(move |c| bulk(c, f)).await
    }

    /// [`Db::read_blocking`] on tokio's blocking pool.
    ///
    /// # Errors
    ///
    /// Whatever `f` returns, [`Error::Poisoned`] or [`Error::Task`].
    pub async fn read<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
    {
        let db = self.clone();
        crate::threads::blocking(crate::threads::label::DB_READ, move || db.read_blocking(f))
            .await
            .map_err(|e| Error::Task(e.to_string()))?
    }

    /// Runs `f` with the writer held for all of it, so no other write reaches the file
    /// until `f` returns; reads keep running. [`HeldWriter::replace_file`] swaps the file.
    ///
    /// # Errors
    ///
    /// Whatever `f` returns, or [`Error::Poisoned`].
    ///
    /// ```
    /// # let dir = tempfile::tempdir().unwrap();
    /// let db = mistarr_server::db::Db::open(&dir.path().join("h.db")).unwrap();
    /// let v = db.hold_writer_blocking(|h| Ok(h.path().to_path_buf())).unwrap();
    /// assert_eq!(v, db.path());
    /// ```
    pub fn hold_writer_blocking<T>(
        &self,
        f: impl FnOnce(&mut HeldWriter<'_>) -> Result<T>,
    ) -> Result<T> {
        let conn = self.inner.writer.lock().map_err(|_| Error::Poisoned)?;
        let mut held = HeldWriter {
            inner: &self.inner,
            conn,
        };
        f(&mut held)
    }

    /// [`Db::hold_writer_blocking`] on tokio's blocking pool under `label`, once no other
    /// async write is running; async writes queued meanwhile wait until `f` returns.
    ///
    /// # Errors
    ///
    /// Whatever `f` returns, [`Error::Poisoned`] or [`Error::Task`].
    pub async fn hold_writer<T, F>(&self, label: crate::threads::Label, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut HeldWriter<'_>) -> Result<T> + Send + 'static,
    {
        let turn = Arc::clone(&self.inner.write_turn)
            .acquire_owned()
            .await
            .map_err(|_| Error::Poisoned)?;
        let db = self.clone();
        crate::threads::blocking(label, move || {
            let _turn = turn;
            db.hold_writer_blocking(f)
        })
        .await
        .map_err(|e| Error::Task(e.to_string()))?
    }

    /// Empties the WAL into the file and closes both connections, so the file stands
    /// alone and its `-wal` is gone. Every clone of this `Db` must be dropped first.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when a clone is still alive or the WAL could not be emptied,
    /// [`Error::Poisoned`], or [`Error::Db`] when a connection fails to close.
    ///
    /// ```
    /// # let dir = tempfile::tempdir().unwrap();
    /// let path = dir.path().join("c.db");
    /// mistarr_server::db::Db::open(&path).unwrap().close().unwrap();
    /// assert!(!dir.path().join("c.db-wal").exists());
    /// ```
    pub fn close(self) -> Result<()> {
        let inner = Arc::try_unwrap(self.inner)
            .map_err(|_| std::io::Error::other("the database is still in use"))?;
        let writer = inner.writer.into_inner().map_err(|_| Error::Poisoned)?;
        let reader = inner.reader.into_inner().map_err(|_| Error::Poisoned)?;
        close_connection(reader)?;
        let emptied = wal_emptied(&writer)?;
        close_connection(writer)?;
        if !emptied {
            return Err(std::io::Error::other("the database's WAL could not be emptied").into());
        }
        if inner.scratch {
            // A connection with no statement of its own may change the journal mode.
            let conn = Connection::open(&inner.path)?;
            conn.pragma_update_and_check(None, "journal_mode", "WAL", |_| Ok(()))?;
            close_connection(conn)?;
        }
        Ok(())
    }
}

/// The writer connection held by [`Db::hold_writer_blocking`], with the file it writes.
pub struct HeldWriter<'a> {
    inner: &'a Inner,
    conn: MutexGuard<'a, Connection>,
}

impl HeldWriter<'_> {
    /// The held writer connection.
    pub fn conn(&mut self) -> &mut Connection {
        &mut self.conn
    }

    /// The database file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// Puts `new`, a complete database file already synced beside this one, in its place
    /// with [`install_file`] and reopens both connections on whichever file then has the
    /// name. The reader is held throughout, so a read sees the old file or the new one.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the WAL cannot be emptied or a file cannot be removed or renamed,
    /// with the old file reopened; [`Error::Reopen`] when no file could be reopened, which
    /// leaves connections that fail every statement until a restart.
    pub fn replace_file(&mut self, new: &Path) -> Result<()> {
        self.replace_with(new, reopen_pair)
    }

    /// [`HeldWriter::replace_file`] reopening through `reopen`, which tests make fail.
    fn replace_with(
        &mut self,
        new: &Path,
        reopen: impl FnOnce(&Path) -> Result<(Connection, Connection)>,
    ) -> Result<()> {
        let mut reader = self.inner.reader.lock().map_err(|_| Error::Poisoned)?;
        if !wal_emptied(&self.conn)? {
            return Err(std::io::Error::other("the database's WAL could not be emptied").into());
        }
        let (hold_writer, hold_reader) = (placeholder()?, placeholder()?);
        let path = self.inner.path.clone();
        let writer = std::mem::replace(&mut *self.conn, hold_writer);
        let old_reader = std::mem::replace(&mut *reader, hold_reader);
        let closed = close_connection(old_reader).and(close_connection(writer));
        let swapped = closed.and_then(|()| Ok(install_file(&path, new)?));
        let (w, r) = reopen(&path).map_err(|e| {
            tracing::error!(error = %e, "cannot reopen the database; restart mistarr");
            Error::Reopen(Box::new(e))
        })?;
        *self.conn = w;
        *reader = r;
        swapped
    }
}

/// `path` with `suffix` appended to its file name, as SQLite names `-wal` and `-shm`.
///
/// ```
/// let p = mistarr_server::db::sibling(std::path::Path::new("/d/m.db"), "-wal");
/// assert_eq!(p, std::path::Path::new("/d/m.db-wal"));
/// ```
#[must_use]
pub fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Suffix the old database file takes while [`install_file`] puts a new one in its place.
pub const OLD_SUFFIX: &str = ".old";

/// Suffix of the empty marker [`install_file`] keeps beside the database for the length
/// of its renames, so a start after a crash that left no readable name never creates one.
pub const SWAP_SUFFIX: &str = ".swap";

/// Puts `new`, a complete database file synced beside `path`, in its place once no
/// connection has `path` open: removes the old file's `-wal` and `-shm`, which must hold
/// nothing unwritten, writes the `.swap` marker, renames `path` to `.old`, `new` to
/// `path`, and removes `.old` and the marker, syncing the directory after each.
/// [`ram::clean_stale`] finishes a swap a crash cut short; `docs/ARCHITECTURE.md` "DAT
/// import in RAM" lists every crash point.
///
/// # Errors
///
/// The I/O failure of a removal or a rename; the old file is then back under `path`,
/// unless renaming it back failed too, which the error names.
pub(crate) fn install_file(path: &Path, new: &Path) -> std::io::Result<()> {
    for suffix in ["-wal", "-shm"] {
        remove_if_present(&sibling(path, suffix))?;
    }
    let old = sibling(path, OLD_SUFFIX);
    remove_if_present(&old)?;
    let marker = sibling(path, SWAP_SUFFIX);
    std::fs::File::create(&marker)?.sync_all()?;
    sync_parent(path);
    if let Err(e) = std::fs::rename(path, &old) {
        remove_if_present(&marker)?;
        return Err(e);
    }
    sync_parent(path);
    if let Err(e) = std::fs::rename(new, path) {
        let back = std::fs::rename(&old, path);
        sync_parent(path);
        return Err(match back {
            Ok(()) => {
                remove_if_present(&marker)?;
                e
            }
            Err(b) => std::io::Error::other(format!(
                "{e}; the old database stays at {}: {b}",
                old.display()
            )),
        });
    }
    sync_parent(path);
    for leftover in [&old, &marker] {
        if let Err(e) = std::fs::remove_file(leftover) {
            tracing::warn!(error = %e, "cannot remove a file of the swap; the next start does");
        }
        sync_parent(path);
    }
    Ok(())
}

/// Syncs the directory holding `path`, making a rename in it durable where the mount is
/// not `dirsync`.
fn sync_parent(path: &Path) {
    if let Some(dir) = path.parent() {
        if let Err(e) = std::fs::File::open(dir).and_then(|d| d.sync_all()) {
            tracing::warn!(error = %e, "cannot sync the database directory");
        }
    }
}

/// Opens both connections on the existing file `path`, never creating one.
fn reopen_pair(path: &Path) -> Result<(Connection, Connection)> {
    if !path.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("no database at {}", path.display()),
        )
        .into());
    }
    open_pair(path, None, false)
}

/// Removes `path`, treating a file already gone as removed.
fn remove_if_present(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Checkpoints `conn`'s WAL with TRUNCATE and says whether it is now empty; true when
/// the connection is not in WAL mode.
fn wal_emptied(conn: &Connection) -> Result<bool> {
    let (busy, log, done): (i64, i64, i64) =
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
    Ok(busy == 0 && log == done)
}

fn close_connection(conn: Connection) -> Result<()> {
    conn.close().map_err(|(_, e)| Error::Db(e))
}

/// A connection that fails every statement, held while the real ones are swapped.
fn placeholder() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    conn.pragma_update(None, "query_only", true)?;
    Ok(conn)
}

/// Opens the writer, applying migrations, and the read-only reader on `path`.
fn open_pair(
    path: &Path,
    steps: Option<&crate::migrating::Steps>,
    scratch: bool,
) -> Result<(Connection, Connection)> {
    if !path.exists() && ram::swap_files(path) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "{} is missing beside the files of a swap cut short; not creating a new one",
                path.display()
            ),
        )
        .into());
    }
    let mut writer = Connection::open(path)?;
    // Checked before `configure`, whose pragmas may write to the file.
    migrate::check_supported(&writer)?;
    configure(&writer, scratch)?;
    if let Some(steps) = steps {
        count_steps(&writer, steps)?;
    }
    // A migration that rebuilds an index writes each page once with the bulk cache.
    bulk(&mut writer, |c| migrate::apply(c).map(drop))?;
    writer.progress_handler(0, None::<fn() -> bool>)?;
    let reader = Connection::open(path)?;
    configure(&reader, scratch)?;
    reader.pragma_update(None, "query_only", true)?;
    Ok((writer, reader))
}

/// Commits `tx` after bringing `title_groups` up to date with the writes it holds, so
/// readers never see the one without the other. Every write transaction commits here.
///
/// # Errors
///
/// [`Error::Db`] when the refresh or the commit fails; the transaction is then rolled back.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let tx = conn.transaction().unwrap();
/// tx.execute("UPDATE titles SET wanted = 1", []).unwrap();
/// mistarr_server::db::commit(tx).unwrap();
/// ```
pub fn commit(tx: Transaction<'_>) -> Result<()> {
    groups::flush(&tx)?;
    tx.commit()?;
    Ok(())
}

/// Refreshes groups an autocommit write left dirty, in a transaction of their own, and
/// warns, since a writer that commits through [`commit`] never leaves any.
fn settle(conn: &mut Connection) -> Result<()> {
    if !conn.is_autocommit() || !groups::pending(conn)? {
        return Ok(());
    }
    tracing::warn!("title groups refreshed after an autocommit write");
    commit(conn.transaction()?)
}

/// Runs `f` on `conn` with its page cache at 8 MiB and the soft heap limit raised to
/// match, then puts both back and frees the extra pages, whether `f` succeeded or not.
/// A write transaction inside `f` whose dirty pages fit then writes each once, at its
/// commit, instead of spilling pages early and writing them again.
///
/// # Errors
///
/// Whatever `f` returns, else [`Error::Db`] when a pragma fails.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// let cache: i64 = mistarr_server::db::bulk(&mut conn, |c| {
///     Ok(c.pragma_query_value(None, "cache_size", |r| r.get(0))?)
/// }).unwrap();
/// assert_eq!(cache, -8 * 1024);
/// ```
pub fn bulk<T>(conn: &mut Connection, f: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
    let mut open = Bulk::enter(conn)?;
    let out = f(open.conn);
    match (out, open.leave()) {
        (Ok(v), Ok(())) => Ok(v),
        (Ok(_), Err(e)) => Err(e),
        (Err(e), restored) => {
            if let Err(r) = restored {
                tracing::error!(error = %r, "writer cache not restored after a failed bulk write");
            }
            Err(e)
        }
    }
}

/// An open bulk write: restores the cache and the heap limit when left, or when dropped
/// on any other path, a panic included.
struct Bulk<'c> {
    conn: &'c mut Connection,
    open: bool,
}

impl<'c> Bulk<'c> {
    fn enter(conn: &'c mut Connection) -> Result<Self> {
        conn.pragma_update(None, "cache_size", -BULK_CACHE_KIB)?;
        let open = Self { conn, open: true };
        heap_limit(open.conn, 1)?;
        Ok(open)
    }

    fn leave(&mut self) -> Result<()> {
        if !std::mem::replace(&mut self.open, false) {
            return Ok(());
        }
        // `and` takes its argument eagerly, so the count drops whatever the cache pragma did.
        self.conn
            .pragma_update(None, "cache_size", -CACHE_KIB)
            .map_err(Error::from)
            .and(heap_limit(self.conn, -1))
            .and_then(|()| Ok(self.conn.execute_batch("PRAGMA shrink_memory")?))
    }
}

impl Drop for Bulk<'_> {
    fn drop(&mut self) {
        if let Err(e) = self.leave() {
            tracing::error!(error = %e, "writer cache not restored after a bulk write");
        }
    }
}

/// Bulk writes open in this process; the soft heap limit is raised while any is.
static BULK_OPEN: Mutex<usize> = Mutex::new(0);

/// Counts `delta` bulk writes in or out and sets the process-wide soft heap limit to
/// match, so a connection opened meanwhile never lowers it under an open bulk write.
fn heap_limit(conn: &Connection, delta: isize) -> Result<()> {
    let mut open = BULK_OPEN.lock().unwrap_or_else(PoisonError::into_inner);
    *open = open.saturating_add_signed(delta);
    let limit = if *open > 0 {
        BULK_HEAP_LIMIT
    } else {
        SOFT_HEAP_LIMIT
    };
    conn.pragma_update_and_check(None, "soft_heap_limit", limit, |_| Ok(()))?;
    Ok(())
}

/// Makes `conn` bump `steps` every [`crate::migrating::STEP_OPS`] instructions it runs.
///
/// # Errors
///
/// [`Error::Db`] when the handler cannot be set.
pub fn count_steps(conn: &Connection, steps: &crate::migrating::Steps) -> Result<()> {
    let steps = Arc::clone(steps);
    conn.progress_handler(
        crate::migrating::STEP_OPS,
        Some(move || {
            steps.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            false
        }),
    )?;
    Ok(())
}

/// The environment variable SQLite reads for its temporary file directory.
pub const SQLITE_TMPDIR: &str = "SQLITE_TMPDIR";

/// The RAM-backed directory for SQLite's temporary files, used when it can be written.
pub const RAM_TEMP_DIR: &str = "/tmp/mistarr";

/// The environment variable that names another directory in place of [`RAM_TEMP_DIR`],
/// so tests and side-by-side servers each get their own.
pub const TEMP_DIR_ENV: &str = "MISTARR_TEMP_DIR";

/// Where SQLite's temporary files go, and why the RAM directory was refused when it was.
#[derive(Debug)]
pub struct TempDir {
    /// The directory chosen.
    pub dir: PathBuf,
    /// Why the RAM directory could not be used; `None` when it is `dir`.
    pub refused: Option<Error>,
}

/// Returns `ram` when it is, or can be made, a directory of mode 0700 that this user owns,
/// is not a symlink, and takes a file, having emptied it with [`prepare_temp_dir`]; else
/// prepares and returns `fallback`. On the card every temporary page would be written
/// through its `sync` mount; see `docs/ARCHITECTURE.md` "Writes on a sync mount".
///
/// # Errors
///
/// [`Error::Io`] when `fallback` cannot be prepared either.
///
/// ```
/// let dir = tempfile::tempdir().unwrap();
/// let (ram, disk) = (dir.path().join("ram"), dir.path().join("disk"));
/// let chosen = mistarr_server::db::choose_temp_dir(&ram, &disk).unwrap();
/// assert_eq!(chosen.dir, ram);
/// assert!(chosen.refused.is_none());
/// ```
pub fn choose_temp_dir(ram: &Path, fallback: &Path) -> Result<TempDir> {
    let usable = |dir: &Path| -> Result<()> {
        private_dir(dir)?;
        prepare_temp_dir(dir)?;
        let probe = dir.join(format!(".probe-{}", std::process::id()));
        std::fs::write(&probe, b"x")?;
        std::fs::remove_file(&probe)?;
        Ok(())
    };
    match usable(ram) {
        Ok(()) => Ok(TempDir {
            dir: ram.to_path_buf(),
            refused: None,
        }),
        Err(e) => {
            prepare_temp_dir(fallback)?;
            Ok(TempDir {
                dir: fallback.to_path_buf(),
                refused: Some(e),
            })
        }
    }
}

/// Creates `dir` with mode 0700, or checks the one there is a real directory this user
/// owns and narrows it to 0700, so no other user can read or plant temporary files.
pub(crate) fn private_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    let refuse = |why: &str| -> Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("{} {why}", dir.display()),
        )
        .into())
    };
    let meta = match std::fs::symlink_metadata(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::DirBuilder::new()
                .mode(0o700)
                .recursive(true)
                .create(dir)?;
            std::fs::symlink_metadata(dir)?
        }
        other => other?,
    };
    if meta.file_type().is_symlink() {
        return refuse("is a symlink");
    }
    if !meta.is_dir() {
        return refuse("is not a directory");
    }
    if meta.uid() != rustix::process::geteuid().as_raw() {
        return refuse("belongs to another user");
    }
    if meta.mode() & 0o777 != 0o700 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Creates `dir` for SQLite's temporary files and removes the files a previous run left
/// there; the caller then points [`SQLITE_TMPDIR`] at it before any connection opens.
///
/// # Errors
///
/// [`Error::Io`] when `dir` cannot be created or listed.
///
/// ```
/// let dir = std::env::temp_dir().join("mistarr-doc-sqlite-tmp");
/// mistarr_server::db::prepare_temp_dir(&dir).unwrap();
/// assert!(dir.is_dir());
/// ```
pub fn prepare_temp_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    for entry in std::fs::read_dir(dir)?.flatten() {
        // The frozen client's record outlives a restart so the client is resumed.
        if entry.file_name() == crate::freeze::FROZEN_NAME {
            continue;
        }
        if entry.file_type().is_ok_and(|t| t.is_file()) {
            if let Err(e) = std::fs::remove_file(entry.path()) {
                tracing::warn!(error = %e, "cannot remove a stale SQLite temporary file");
            }
        }
    }
    Ok(())
}

/// Opens `path` read-only with the reader's memory settings, for measuring queries on a
/// database a server may be using; it never writes or migrates.
///
/// # Errors
///
/// [`Error::Db`] when the file cannot be opened.
///
/// ```
/// let dir = tempfile::tempdir().unwrap();
/// let path = dir.path().join("ro.db");
/// mistarr_server::db::Db::open(&path).unwrap();
/// let conn = mistarr_server::db::open_read_only(&path).unwrap();
/// assert!(conn.execute("CREATE TABLE x (y)", []).is_err());
/// ```
pub fn open_read_only(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.pragma_update(None, "query_only", true)?;
    conn.pragma_update(None, "cache_size", -CACHE_KIB)?;
    conn.pragma_update(None, "mmap_size", 0)?;
    Ok(conn)
}

/// Whether the database has a table named `name`.
///
/// # Errors
///
/// [`Error::Db`] on SQLite failure.
///
/// ```
/// let conn = rusqlite::Connection::open_in_memory().unwrap();
/// assert!(!mistarr_server::db::has_table(&conn, "titles").unwrap());
/// ```
pub fn has_table(conn: &Connection, name: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [name],
        |r| r.get(0),
    )?)
}

/// Applies the connection pragmas every connection shares; the memory-related ones are
/// listed in `docs/ARCHITECTURE.md` "Resource budgets".
fn configure(conn: &Connection, scratch: bool) -> Result<()> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    let want = if scratch { "delete" } else { "wal" };
    let mode: String =
        conn.pragma_update_and_check(None, "journal_mode", want, |row| row.get(0))?;
    if !mode.eq_ignore_ascii_case(want) {
        tracing::warn!(mode, want, "database is not in its journal mode");
    }
    conn.pragma_update(None, "synchronous", if scratch { "OFF" } else { "NORMAL" })?;
    conn.pragma_update(None, "foreign_keys", true)?;
    conn.pragma_update(None, "cache_size", -CACHE_KIB)?;
    conn.pragma_update(None, "mmap_size", 0)?;
    conn.pragma_update(None, "temp_store", "FILE")?;
    conn.pragma_update(None, "wal_autocheckpoint", WAL_AUTOCHECKPOINT)?;
    conn.pragma_update_and_check(None, "journal_size_limit", JOURNAL_SIZE_LIMIT, |_| Ok(()))?;
    heap_limit(conn, 0)
}

#[cfg(test)]
mod plans;

#[cfg(test)]
pub(crate) mod testutil {
    use super::Db;

    /// A fresh migrated database in a temporary directory kept alive by the guard.
    pub fn db() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Db::open(&dir.path().join("test.db")).expect("open");
        (dir, db)
    }

    /// A directory for a copy in RAM, on the tmpfs of `/dev/shm`, apart from the
    /// temporary directory the test's database is in, which may be on disk or tmpfs.
    pub fn ram_dir() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("mistarr-ram-")
            .tempdir_in("/dev/shm")
            .expect("a directory in /dev/shm")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_held_writer_keeps_async_writes_waiting_and_reads_running() {
        let (_dir, db) = testutil::db();
        let (held, is_held) = tokio::sync::oneshot::channel();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let holding = db.clone();
        let holder = tokio::spawn(async move {
            let label = crate::threads::label::DAT_IMPORT;
            holding
                .hold_writer(label, move |h| {
                    let _ = held.send(());
                    let _ = released.recv();
                    settings::set(h.conn(), "k", "held")?;
                    Ok(h.path().to_path_buf())
                })
                .await
        });
        is_held.await.expect("held");
        let writing = db.clone();
        let write =
            tokio::spawn(async move { writing.write(|c| settings::set(c, "k", "queued")).await });
        let read = db
            .read(migrate::current_version)
            .await
            .expect("a read runs");
        assert!(read >= 1);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!write.is_finished(), "an async write waits for the hold");
        release.send(()).expect("release");
        assert_eq!(holder.await.expect("join").expect("hold"), db.path());
        write.await.expect("join").expect("write");
        let k = db.read(|c| settings::get(c, "k")).await.expect("read");
        assert_eq!(k.as_deref(), Some("queued"), "the queued write ran after");
    }

    #[test]
    fn a_copy_uses_a_rollback_journal_and_closes_in_wal_mode() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("copy.db");
        let copy = Db::open_copy(&path, None).expect("open");
        copy.write_blocking(|c| settings::set(c, "k", "v"))
            .expect("write");
        copy.close().expect("close");
        for suffix in ["-journal", "-wal", "-shm"] {
            assert!(!sibling(&path, suffix).exists(), "{suffix} left");
        }
        let c = Connection::open(&path).expect("open");
        let mode: String = c
            .pragma_query_value(None, "journal_mode", |r| r.get(0))
            .expect("mode");
        assert_eq!(mode, "wal");
    }

    #[test]
    fn connections_use_wal_and_the_cache_budget() {
        let (_dir, db) = testutil::db();
        let (mode, cache): (String, i64) = db
            .read_blocking(|c| {
                let mode = c.pragma_query_value(None, "journal_mode", |r| r.get(0))?;
                let cache = c.pragma_query_value(None, "cache_size", |r| r.get(0))?;
                Ok((mode, cache))
            })
            .expect("pragmas");
        assert_eq!(mode, "wal");
        assert_eq!(cache, -CACHE_KIB);
    }

    #[test]
    fn connections_keep_memory_and_the_wal_small() {
        let (_dir, db) = testutil::db();
        for read in [true, false] {
            let pragma = |c: &Connection, name: &str| -> Result<i64> {
                Ok(c.pragma_query_value(None, name, |r| r.get(0))?)
            };
            let values = |c: &Connection| -> Result<[i64; 5]> {
                Ok([
                    pragma(c, "mmap_size")?,
                    pragma(c, "temp_store")?,
                    pragma(c, "wal_autocheckpoint")?,
                    pragma(c, "journal_size_limit")?,
                    pragma(c, "soft_heap_limit")?,
                ])
            };
            let got = if read {
                db.read_blocking(values)
            } else {
                db.write_blocking(|c| values(c))
            }
            .expect("pragmas");
            assert_eq!(
                got[..4],
                [0, 1, WAL_AUTOCHECKPOINT, JOURNAL_SIZE_LIMIT],
                "read {read}"
            );
            // Process-wide: another test's bulk write may hold it raised.
            assert!(
                [SOFT_HEAP_LIMIT, BULK_HEAP_LIMIT].contains(&got[4]),
                "{got:?}"
            );
        }
    }

    #[test]
    fn a_bulk_write_raises_the_writer_cache_and_restores_it_on_error() {
        let (_dir, db) = testutil::db();
        let cache = |c: &Connection| -> Result<i64> {
            Ok(c.pragma_query_value(None, "cache_size", |r| r.get(0))?)
        };
        let inside = db
            .write_bulk_blocking(|c| {
                let heap: i64 = c.pragma_query_value(None, "soft_heap_limit", |r| r.get(0))?;
                Ok((cache(c)?, heap))
            })
            .expect("bulk");
        assert_eq!(inside, (-BULK_CACHE_KIB, BULK_HEAP_LIMIT));
        assert_eq!(db.write_blocking(|c| cache(c)).expect("after"), -CACHE_KIB);
        let failed: Result<()> = db.write_bulk_blocking(|c| {
            c.execute("INSERT INTO no_such_table VALUES (1)", [])?;
            Ok(())
        });
        assert!(failed.is_err());
        assert_eq!(
            db.write_blocking(|c| cache(c)).expect("after error"),
            -CACHE_KIB
        );
        assert_eq!(db.read_blocking(|c| cache(c)).expect("reader"), -CACHE_KIB);
    }

    #[test]
    fn a_panic_inside_a_bulk_write_still_restores_the_cache() {
        let mut conn = Connection::open_in_memory().expect("open");
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            bulk(&mut conn, |_| -> Result<()> {
                panic!("inside a bulk write")
            })
        }));
        assert!(caught.is_err());
        let cache: i64 = conn
            .pragma_query_value(None, "cache_size", |r| r.get(0))
            .expect("cache");
        assert_eq!(cache, -CACHE_KIB);
    }

    #[test]
    fn temp_files_go_to_ram_when_it_can_be_written() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let disk = dir.path().join("data/tmp");
        let ram = dir.path().join("ram");
        let chosen = choose_temp_dir(&ram, &disk).expect("ram");
        assert_eq!(chosen.dir, ram);
        let mode = std::fs::metadata(&ram).expect("ram").permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        std::fs::write(dir.path().join("file"), b"x").expect("write");
        let blocked = dir.path().join("file/sub");
        let chosen = choose_temp_dir(&blocked, &disk).expect("disk");
        assert_eq!(chosen.dir, disk);
        assert!(chosen.refused.is_some());
        assert!(disk.is_dir());
    }

    #[test]
    fn a_temp_dir_that_is_a_symlink_or_open_to_others_is_refused_or_narrowed() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let disk = dir.path().join("data/tmp");
        let target = dir.path().join("elsewhere");
        std::fs::create_dir(&target).expect("mkdir");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        let chosen = choose_temp_dir(&link, &disk).expect("disk");
        assert_eq!(chosen.dir, disk);
        let why = chosen.refused.expect("refused").to_string();
        assert!(why.contains("is a symlink"), "{why}");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o777)).expect("chmod");
        assert_eq!(choose_temp_dir(&target, &disk).expect("ram").dir, target);
        let mode = std::fs::metadata(&target)
            .expect("meta")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[test]
    fn the_temp_dir_is_created_and_emptied_of_stale_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tmp = dir.path().join("data/tmp");
        prepare_temp_dir(&tmp).expect("create");
        std::fs::write(tmp.join("etilqs_stale"), b"x").expect("write");
        std::fs::create_dir(tmp.join("keep")).expect("mkdir");
        prepare_temp_dir(&tmp).expect("clear");
        let left: Vec<_> = std::fs::read_dir(&tmp)
            .expect("list")
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(left, ["keep"]);
    }

    #[test]
    fn reader_cannot_write() {
        let (_dir, db) = testutil::db();
        let r = db.read_blocking(|c| Ok(settings::set(c, "k", "v")));
        assert!(r.expect("lock").is_err());
    }

    #[test]
    fn reads_run_while_writers_queue_behind_a_long_write() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(2)
            .enable_time()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let (_dir, db) = testutil::db();
            let (held, holding) = std::sync::mpsc::channel();
            let (release, released) = std::sync::mpsc::channel::<()>();
            let long = tokio::spawn({
                let db = db.clone();
                async move {
                    db.write(move |_| {
                        held.send(()).ok();
                        released.recv().ok();
                        Ok(())
                    })
                    .await
                }
            });
            while holding.try_recv().is_err() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let queued: Vec<_> = (0..4)
                .map(|i| {
                    let db = db.clone();
                    tokio::spawn(async move {
                        db.write(move |c| settings::set(c, "k", &i.to_string()))
                            .await
                    })
                })
                .collect();
            tokio::time::sleep(Duration::from_millis(20)).await;
            let read =
                tokio::time::timeout(Duration::from_secs(5), db.read(migrate::current_version))
                    .await;
            assert!(
                read.is_ok_and(|r| r.is_ok()),
                "a read waited on queued writers"
            );
            release.send(()).expect("release");
            long.await.expect("join").expect("long write");
            for q in queued {
                q.await.expect("join").expect("queued write");
            }
        });
    }

    #[tokio::test]
    async fn async_wrappers_reach_both_connections() {
        let (_dir, db) = testutil::db();
        db.write(|c| settings::set(c, "a", "1"))
            .await
            .expect("write");
        let v = db.read(|c| settings::get(c, "a")).await.expect("read");
        assert_eq!(v.as_deref(), Some("1"));
        assert!(db.path().is_file());
    }
}
