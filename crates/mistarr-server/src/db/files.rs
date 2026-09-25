//! The `files` table and the resumable `scan_progress` table it is scanned
//! against. See `docs/DATA-MODEL.md` "files" and "files.state".

use std::fmt;

use mistarr_core::PlatformId;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::Serialize;

use crate::error::Result;

/// A `files.id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct FileId(pub i64);

impl fmt::Display for FileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// `files.state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FileState {
    /// Seen, not yet hashed.
    Pending,
    /// Hash matches a rom and the filename is what the adapter expects.
    Verified,
    /// Hash matches a rom, name differs.
    Misnamed,
    /// No rom matches in any loaded DAT.
    Unverified,
    /// Matches a rom flagged `baddump`.
    Bad,
    /// A disc image whose tracks are not identified yet or cannot be; `reason` says why.
    Unidentified,
}

impl FileState {
    /// The column value.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Verified => "verified",
            Self::Misnamed => "misnamed",
            Self::Unverified => "unverified",
            Self::Bad => "bad",
            Self::Unidentified => "unidentified",
        }
    }

    /// Parses a column value.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        [
            Self::Pending,
            Self::Verified,
            Self::Misnamed,
            Self::Unverified,
            Self::Bad,
            Self::Unidentified,
        ]
        .into_iter()
        .find(|v| v.as_str() == s)
    }
}

/// One `files` row.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FileRow {
    /// Row id.
    pub id: FileId,
    /// Owning platform.
    pub platform_id: PlatformId,
    /// Relative to `games/`, including the top directory name; a zip member is
    /// `NES/a.zip#b.nes`.
    pub rel_path: String,
    /// Size in bytes on disk, or a zip member's uncompressed size, header included.
    pub size: i64,
    /// Filesystem mtime, Unix seconds.
    pub mtime: i64,
    /// CRC32 as lowercase hex, when hashed.
    pub crc32: Option<String>,
    /// MD5 as lowercase hex, when fully hashed.
    pub md5: Option<String>,
    /// SHA1 as lowercase hex, when fully hashed.
    pub sha1: Option<String>,
    /// The header rule the payload was hashed under, or last failed to hash under;
    /// NULL for a zip member known by its central-directory CRC32 alone.
    pub header_rule: Option<String>,
    /// The matched `roms.id`, when any.
    pub rom_id: Option<i64>,
    /// Lifecycle state.
    pub state: FileState,
    /// Unix seconds this row was last written by a scan.
    pub scanned_at: i64,
    /// Why an `unidentified` row is not identified, a code of `docs/VERIFICATION.md`
    /// "CHD images"; `None` in every other state.
    pub reason: Option<String>,
}

/// A file found on disk and hashed, ready to be matched and written by the
/// scan job. `rom_id` and `state` are already decided.
#[derive(Debug, Clone)]
pub struct NewFile {
    /// Relative to `games/`, including the top directory name; a zip member
    /// is `a.zip#b.nes`.
    pub rel_path: String,
    /// Size in bytes on disk, or a zip member's uncompressed size, header included.
    pub size: i64,
    /// Filesystem mtime, Unix seconds.
    pub mtime: i64,
    /// CRC32 as lowercase hex.
    pub crc32: Option<String>,
    /// MD5 as lowercase hex, when fully hashed.
    pub md5: Option<String>,
    /// SHA1 as lowercase hex, when fully hashed.
    pub sha1: Option<String>,
    /// The header rule the payload was hashed under, or last failed to hash under;
    /// `None` for a zip member known by its central-directory CRC32 alone.
    pub header_rule: Option<String>,
    /// The matched `roms.id`, when any.
    pub rom_id: Option<i64>,
    /// The decided state.
    pub state: FileState,
    /// Why an `unidentified` row is not identified; `None` in every other state.
    pub reason: Option<String>,
}

/// The last path component of a name that may use `/` as a separator, as
/// zip member names and DAT rom names for disc tracks do.
///
/// ```
/// use mistarr_server::db::files::basename;
/// assert_eq!(basename("dir/game.bin"), "game.bin");
/// assert_eq!(basename("game.bin"), "game.bin");
/// ```
#[must_use]
pub fn basename(name: &str) -> &str {
    name.rsplit(['/', '\\']).next().unwrap_or(name)
}

/// A rom a hashed file matched, per `docs/VERIFICATION.md` "Matching order".
#[derive(Debug, Clone, PartialEq)]
pub struct RomMatch {
    /// The matched `roms.id`.
    pub rom_id: i64,
    /// The rom's owning `titles.id`.
    pub title_id: i64,
    /// The filename the DAT expects, compared against the file's own name.
    pub name: String,
    /// `good`, `baddump`, `nodump` or `verified`.
    pub status: String,
}

pub(crate) const COLUMNS: &str =
    "id, platform_id, rel_path, size, mtime, crc32, md5, sha1, header_rule, rom_id, state, scanned_at, reason";

pub(crate) fn from_row(r: &Row<'_>) -> rusqlite::Result<FileRow> {
    let state: String = r.get(10)?;
    Ok(FileRow {
        id: FileId(r.get(0)?),
        platform_id: PlatformId(r.get(1)?),
        rel_path: r.get(2)?,
        size: r.get(3)?,
        mtime: r.get(4)?,
        crc32: r.get(5)?,
        md5: r.get(6)?,
        sha1: r.get(7)?,
        header_rule: r.get(8)?,
        rom_id: r.get(9)?,
        state: FileState::parse(&state).unwrap_or(FileState::Pending),
        scanned_at: r.get(11)?,
        reason: r.get(12)?,
    })
}

/// Looks up a file by its platform and relative path, for the scan's
/// size-and-mtime skip check.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn find_by_path(
    conn: &Connection,
    platform_id: &PlatformId,
    rel_path: &str,
) -> Result<Option<FileRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM files WHERE platform_id = ?1 AND rel_path = ?2"),
            params![platform_id.0, rel_path],
            from_row,
        )
        .optional()?)
}

/// Reads one file row.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn get(conn: &Connection, id: FileId) -> Result<Option<FileRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM files WHERE id = ?1"),
            [id.0],
            from_row,
        )
        .optional()?)
}

/// Moves a row to `rel_path` with a new state and mtime, after the file was
/// renamed on disk.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure, including a row already at `rel_path`.
pub fn move_to(
    conn: &Connection,
    id: FileId,
    rel_path: &str,
    state: FileState,
    mtime: i64,
    now: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE files SET rel_path = ?2, state = ?3, mtime = ?4, scanned_at = ?5 WHERE id = ?1",
        params![id.0, rel_path, state.as_str(), mtime, now],
    )?;
    Ok(())
}

/// Deletes one file row.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn delete(conn: &Connection, id: FileId) -> Result<()> {
    conn.execute("DELETE FROM files WHERE id = ?1", [id.0])?;
    Ok(())
}

/// The member rows kept for container `zip_rel`, a zip or a CHD, stored as `zip_rel#member`.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn zip_member_rows(
    conn: &Connection,
    platform_id: &PlatformId,
    zip_rel: &str,
) -> Result<Vec<FileRow>> {
    // `zip#` up to `zip$` (`$` follows `#`) is an index range over the unique key.
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM files WHERE platform_id = ?1 AND rel_path >= ?2 AND rel_path < ?3
         ORDER BY rel_path"
    ))?;
    let (from, to) = (format!("{zip_rel}#"), format!("{zip_rel}$"));
    let rows = stmt.query_map(params![platform_id.0, from, to], from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Rows of zip `zip_rel` matched ignoring ASCII case, as exFAT names files: its own
/// presence rows (`zip_rel`) and its member rows (`zip_rel#member`), in `rel_path` order.
/// Always uses the `files_rel_lower` index, never a scan of the platform's rows.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn zip_rows_nocase(
    conn: &Connection,
    platform_id: &PlatformId,
    zip_rel: &str,
) -> Result<(Vec<FileRow>, Vec<FileRow>)> {
    let key = zip_rel.to_ascii_lowercase();
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM files INDEXED BY files_rel_lower
         WHERE platform_id = ?1 AND lower(rel_path) = ?2 ORDER BY rel_path"
    ))?;
    let bare = stmt
        .query_map(params![platform_id.0, key], from_row)?
        .collect::<rusqlite::Result<_>>()?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM files INDEXED BY files_rel_lower
         WHERE platform_id = ?1 AND lower(rel_path) >= ?2 AND lower(rel_path) < ?3
         ORDER BY rel_path"
    ))?;
    let (from, to) = (format!("{key}#"), format!("{key}$"));
    let members = stmt
        .query_map(params![platform_id.0, from, to], from_row)?
        .collect::<rusqlite::Result<_>>()?;
    Ok((bare, members))
}

/// Up to `limit` `rel_path`s of `platform_id` under directory `dir` (`dir/...`) that sort
/// after `after`, in order; a keyset page for walking one directory's rows in bounded memory.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn paths_under(
    conn: &Connection,
    platform_id: &PlatformId,
    dir: &str,
    after: &str,
    limit: usize,
) -> Result<Vec<String>> {
    // `dir/` up to `dir0` (`0` follows `/`) is every path inside `dir`.
    let (from, to) = (format!("{dir}/"), format!("{dir}0"));
    let after = if after < from.as_str() {
        from.as_str()
    } else {
        after
    };
    let mut stmt = conn.prepare_cached(
        "SELECT rel_path FROM files WHERE platform_id = ?1 AND rel_path > ?2 AND rel_path < ?3
         ORDER BY rel_path LIMIT ?4",
    )?;
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let rows = stmt.query_map(params![platform_id.0, after, to, limit], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Keeps a row whose file changed on disk but whose hashes still apply: only its
/// `mtime` and `scanned_at` move.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn restamp(conn: &Connection, id: FileId, mtime: i64, now: i64) -> Result<()> {
    conn.execute(
        "UPDATE files SET mtime = ?2, scanned_at = ?3 WHERE id = ?1",
        params![id.0, mtime, now],
    )?;
    Ok(())
}

/// Marks a row whose content changed for re-verification: the new size and CRC32
/// are recorded, the hashes that no longer apply are cleared, `rom_id` is kept and
/// the state becomes `unverified` until an import or md5 check promotes it again.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn reverify(
    conn: &Connection,
    id: FileId,
    size: i64,
    mtime: i64,
    crc32: &str,
    now: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE files SET size = ?2, mtime = ?3, crc32 = ?4, md5 = NULL, sha1 = NULL,
                          header_rule = NULL, state = 'unverified', scanned_at = ?5
         WHERE id = ?1",
        params![id.0, size, mtime, crc32, now],
    )?;
    Ok(())
}

/// Whether rom `rom_id` has a `verified` file.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn has_verified(conn: &Connection, rom_id: i64) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM files WHERE rom_id = ?1 AND state = 'verified')",
        [rom_id],
        |r| r.get(0),
    )?)
}

/// Marks the `unverified` row at `rel_path` verified as rom `rom_id`. Returns whether a row changed.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_core::PlatformId;
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let pid = PlatformId("arcade".into());
/// assert!(!mistarr_server::db::files::mark_verified(&conn, &pid, "mame/a.zip#a.bin", 1).unwrap());
/// ```
pub fn mark_verified(
    conn: &Connection,
    platform_id: &PlatformId,
    rel_path: &str,
    rom_id: i64,
) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE files SET state = 'verified', rom_id = ?3
         WHERE platform_id = ?1 AND rel_path = ?2 AND state = 'unverified'",
        params![platform_id.0, rel_path, rom_id],
    )? > 0)
}

/// Every hashed field a file row records, when the file was hashed this pass.
#[derive(Debug, Clone, Default)]
pub struct Hashed<'a> {
    /// CRC32, always known once a member's central directory entry is read.
    pub crc32: Option<&'a str>,
    /// MD5, known only once the payload was fully hashed.
    pub md5: Option<&'a str>,
    /// SHA1, known only once the payload was fully hashed.
    pub sha1: Option<&'a str>,
    /// The header rule the payload was hashed under, or last failed to hash under;
    /// `None` for a zip member known by its central-directory CRC32 alone.
    pub header_rule: Option<&'a str>,
}

/// Inserts or replaces a file row keyed on `(platform_id, rel_path)`, with no reason.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
#[allow(clippy::too_many_arguments)] // One column per parameter; the row has no natural grouping.
pub fn upsert(
    conn: &Connection,
    platform_id: &PlatformId,
    rel_path: &str,
    size: i64,
    mtime: i64,
    hashed: &Hashed<'_>,
    rom_id: Option<i64>,
    state: FileState,
    now: i64,
) -> Result<FileId> {
    let row = Columns {
        rel_path,
        size,
        mtime,
        hashed,
        rom_id,
        state,
        reason: None,
    };
    upsert_columns(conn, platform_id, &row, now)
}

/// Inserts or replaces the row `row` describes, keyed on `(platform_id, rel_path)`.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::files::{find_by_path, upsert_row, FileState, NewFile};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// mistarr_server::db::platforms::seed(&mut conn, &mistarr_mister::platforms::PLATFORMS).unwrap();
/// let psx = mistarr_core::PlatformId("psx".into());
/// let row = NewFile { rel_path: "PSX/g.chd".into(), size: 9, mtime: 1, crc32: None, md5: None,
///     sha1: None, header_rule: Some("chd".into()), rom_id: None,
///     state: FileState::Unidentified, reason: Some("off".into()) };
/// upsert_row(&conn, &psx, &row, 5).unwrap();
/// let got = find_by_path(&conn, &psx, "PSX/g.chd").unwrap().unwrap();
/// assert_eq!(got.reason.as_deref(), Some("off"));
/// ```
pub fn upsert_row(
    conn: &Connection,
    platform_id: &PlatformId,
    row: &NewFile,
    now: i64,
) -> Result<FileId> {
    let hashed = Hashed {
        crc32: row.crc32.as_deref(),
        md5: row.md5.as_deref(),
        sha1: row.sha1.as_deref(),
        header_rule: row.header_rule.as_deref(),
    };
    let columns = Columns {
        rel_path: &row.rel_path,
        size: row.size,
        mtime: row.mtime,
        hashed: &hashed,
        rom_id: row.rom_id,
        state: row.state,
        reason: row.reason.as_deref(),
    };
    upsert_columns(conn, platform_id, &columns, now)
}

/// The columns [`upsert_columns`] writes.
struct Columns<'a> {
    rel_path: &'a str,
    size: i64,
    mtime: i64,
    hashed: &'a Hashed<'a>,
    rom_id: Option<i64>,
    state: FileState,
    reason: Option<&'a str>,
}

fn upsert_columns(
    conn: &Connection,
    platform_id: &PlatformId,
    row: &Columns<'_>,
    now: i64,
) -> Result<FileId> {
    conn.prepare_cached(
        "INSERT INTO files (platform_id, rel_path, size, mtime, crc32, md5, sha1, header_rule,
                             rom_id, state, scanned_at, reason)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(platform_id, rel_path) DO UPDATE SET
           size = excluded.size, mtime = excluded.mtime, crc32 = excluded.crc32,
           md5 = excluded.md5, sha1 = excluded.sha1, header_rule = excluded.header_rule,
           rom_id = excluded.rom_id, state = excluded.state, scanned_at = excluded.scanned_at,
           reason = excluded.reason",
    )?
    .execute(params![
        platform_id.0,
        row.rel_path,
        row.size,
        row.mtime,
        row.hashed.crc32,
        row.hashed.md5,
        row.hashed.sha1,
        row.hashed.header_rule,
        row.rom_id,
        row.state.as_str(),
        now,
        row.reason,
    ])?;
    Ok(conn
        .query_row(
            "SELECT id FROM files WHERE platform_id = ?1 AND rel_path = ?2",
            params![platform_id.0, row.rel_path],
            |r| r.get(0).map(FileId),
        )
        .optional()?
        .unwrap_or(FileId(conn.last_insert_rowid())))
}

/// The relative paths currently recorded for a platform.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn existing_paths(conn: &Connection, platform_id: &PlatformId) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT rel_path FROM files WHERE platform_id = ?1")?;
    let rows = stmt.query_map([&platform_id.0], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Rows [`delete_missing`] removes per transaction.
const DELETE_BATCH: usize = 500;

/// Deletes the rows of `platform_id` at `paths` through [`delete_ids`]. Set-based: the
/// caller owns the transaction. Returns rows removed.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn delete_paths(
    conn: &Connection,
    platform_id: &PlatformId,
    paths: &[String],
) -> Result<usize> {
    if paths.is_empty() {
        return Ok(0);
    }
    let list = serde_json::to_string(paths).map_err(|e| crate::Error::Job(e.to_string()))?;
    let ids: Vec<i64> = conn
        .prepare_cached(
            "SELECT id FROM files WHERE platform_id = ?1
               AND rel_path IN (SELECT value FROM json_each(?2))",
        )?
        .query_map(params![platform_id.0, list], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    delete_ids(conn, &ids)
}

/// Deletes the rows `ids`, first clearing the `import_log` references to them, since that
/// foreign key has no delete action. Two statements; the caller owns the transaction.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert_eq!(mistarr_server::db::files::delete_ids(&conn, &[1, 2]).unwrap(), 0);
/// ```
pub fn delete_ids(conn: &Connection, ids: &[i64]) -> Result<usize> {
    if ids.is_empty() {
        return Ok(0);
    }
    let list = serde_json::to_string(ids).map_err(|e| crate::Error::Job(e.to_string()))?;
    conn.prepare_cached(
        "UPDATE import_log SET file_id = NULL WHERE file_id IN (SELECT value FROM json_each(?1))",
    )?
    .execute([&list])?;
    Ok(conn
        .prepare_cached("DELETE FROM files WHERE id IN (SELECT value FROM json_each(?1))")?
        .execute([&list])?)
}

/// Deletes every row of `platform_id` whose `rel_path` is not in `keep` and does not lie
/// under a directory of `unreadable` (one the caller could not list, so its rows are kept),
/// through [`delete_paths`] in batches of one transaction each. Returns the rows removed.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn delete_missing(
    conn: &mut Connection,
    platform_id: &PlatformId,
    keep: &[String],
    unreadable: &[String],
) -> Result<usize> {
    let keep: std::collections::HashSet<&str> = keep.iter().map(String::as_str).collect();
    let under = |p: &str| {
        unreadable.iter().any(|d| {
            p.strip_prefix(d.as_str())
                .is_some_and(|r| r.starts_with('/'))
        })
    };
    let gone: Vec<String> = existing_paths(conn, platform_id)?
        .into_iter()
        .filter(|p| !keep.contains(p.as_str()) && !under(p))
        .collect();
    let mut removed = 0;
    for batch in gone.chunks(DELETE_BATCH) {
        let tx = conn.transaction()?;
        removed += delete_paths(&tx, platform_id, batch)?;
        crate::db::commit(tx)?;
    }
    Ok(removed)
}

/// Finds the rom a hashed payload matches under
/// `docs/VERIFICATION.md` "Matching order", scoped to one platform and
/// preferring a live rom, then a title whose `dat_version` is not superseded.
/// Only DAT titles match; MRA roms are zips found by the arcade catalogue.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn match_rom(
    conn: &Connection,
    platform_id: &PlatformId,
    sha1: &str,
    md5: &str,
    crc32: &str,
    size: i64,
) -> Result<Option<RomMatch>> {
    match_among(conn, platform_id, [sha1, md5, crc32], size, "")
}

/// [`match_rom`] over the roms `live` admits, an SQL condition ending in `AND ` or empty.
fn match_among(
    conn: &Connection,
    platform_id: &PlatformId,
    [sha1, md5, crc32]: [&str; 3],
    size: i64,
    live: &str,
) -> Result<Option<RomMatch>> {
    const ORDER: &str = " ORDER BY (r.retired = 0 AND t.retired = 0) DESC,
         d.superseded_by IS NULL DESC, d.id DESC LIMIT 1";
    let select = format!(
        "SELECT r.id, r.title_id, r.name, r.status
         FROM roms r JOIN titles t ON t.id = r.title_id
         JOIN dat_versions d ON d.id = t.dat_version_id
         WHERE t.platform_id = ?1 AND t.source = 'dat' AND {live}"
    );
    let select = select.as_str();
    let row = |r: &Row<'_>| -> rusqlite::Result<RomMatch> {
        Ok(RomMatch {
            rom_id: r.get(0)?,
            title_id: r.get(1)?,
            name: r.get(2)?,
            status: r.get(3)?,
        })
    };
    if let Some(m) = conn
        .prepare_cached(&format!("{select}r.sha1 = ?2{ORDER}"))?
        .query_row(params![platform_id.0, sha1], row)
        .optional()?
    {
        return Ok(Some(m));
    }
    if let Some(m) = conn
        .prepare_cached(&format!("{select}r.sha1 IS NULL AND r.md5 = ?2{ORDER}"))?
        .query_row(params![platform_id.0, md5], row)
        .optional()?
    {
        return Ok(Some(m));
    }
    Ok(conn
        .prepare_cached(&format!(
            "{select}r.sha1 IS NULL AND r.md5 IS NULL AND r.crc32 = ?2 AND r.size = ?3{ORDER}"
        ))?
        .query_row(params![platform_id.0, crc32, size], row)
        .optional()?)
}

/// Every live rom of a live DAT title on `platform_id` that `hashes` matches at the first
/// tier with any match: SHA1, then MD5, then CRC32 and size; ordered by rom id.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let psx = mistarr_core::PlatformId("psx".into());
/// let h = mistarr_core::HashSet { size: 1, crc32: "0".into(), md5: "0".into(), sha1: "0".into() };
/// assert!(mistarr_server::db::files::roms_matching(&conn, &psx, &h).unwrap().is_empty());
/// ```
pub fn roms_matching(
    conn: &Connection,
    platform_id: &PlatformId,
    hashes: &mistarr_core::HashSet,
) -> Result<Vec<RomMatch>> {
    let select = "SELECT r.id, r.title_id, r.name, r.status
         FROM roms r JOIN titles t ON t.id = r.title_id
         WHERE t.platform_id = ?1 AND t.source = 'dat' AND r.retired = 0 AND t.retired = 0 AND ";
    let size = i64::try_from(hashes.size).unwrap_or(i64::MAX);
    let run = |cond: &str, p: &[&dyn rusqlite::ToSql]| -> Result<Vec<RomMatch>> {
        Ok(conn
            .prepare_cached(&format!("{select}{cond} ORDER BY r.id"))?
            .query_map(p, rom_match)?
            .collect::<rusqlite::Result<_>>()?)
    };
    let found = run("r.sha1 = ?2", &[&platform_id.0, &hashes.sha1])?;
    if !found.is_empty() {
        return Ok(found);
    }
    let found = run(
        "r.sha1 IS NULL AND r.md5 = ?2",
        &[&platform_id.0, &hashes.md5],
    )?;
    if !found.is_empty() {
        return Ok(found);
    }
    run(
        "r.sha1 IS NULL AND r.md5 IS NULL AND r.crc32 = ?2 AND r.size = ?3",
        &[&platform_id.0, &hashes.crc32, &size],
    )
}

fn rom_match(r: &Row<'_>) -> rusqlite::Result<RomMatch> {
    Ok(RomMatch {
        rom_id: r.get(0)?,
        title_id: r.get(1)?,
        name: r.get(2)?,
        status: r.get(3)?,
    })
}

/// The live roms of title `title_id`, ordered by rom id, cue sheets included.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert!(mistarr_server::db::files::disc_roms(&conn, 1).unwrap().is_empty());
/// ```
pub fn disc_roms(conn: &Connection, title_id: i64) -> Result<Vec<RomMatch>> {
    Ok(conn
        .prepare_cached(
            "SELECT r.id, r.title_id, r.name, r.status FROM roms r JOIN titles t ON t.id = r.title_id
             WHERE r.title_id = ?1 AND r.retired = 0 AND t.retired = 0 ORDER BY r.id",
        )?
        .query_map([title_id], rom_match)?
        .collect::<rusqlite::Result<_>>()?)
}

/// Whether a live DAT rom on `platform_id` is a whole `.chd` file of `size` bytes, so a
/// `.chd` of that size is hashed whole before its header is read.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let psx = mistarr_core::PlatformId("psx".into());
/// assert!(!mistarr_server::db::files::chd_rom_sized(&conn, &psx, 10).unwrap());
/// ```
pub fn chd_rom_sized(conn: &Connection, platform_id: &PlatformId, size: i64) -> Result<bool> {
    Ok(conn
        .prepare_cached(
            "SELECT EXISTS(
               SELECT 1 FROM roms r INDEXED BY roms_chd_size JOIN titles t ON t.id = r.title_id
               WHERE r.size = ?2 AND t.platform_id = ?1 AND t.source = 'dat'
                 AND r.retired = 0 AND t.retired = 0 AND lower(r.name) LIKE '%.chd')",
        )?
        .query_row(params![platform_id.0, size], |r| r.get(0))?)
}

/// One file the Platforms card lists as not identified.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnidentifiedFile {
    /// Relative to `games/`.
    pub rel_path: String,
    /// Size in bytes on disk.
    pub size: i64,
    /// The reason code; see `docs/VERIFICATION.md` "CHD images".
    pub reason: String,
}

/// A page of the `unidentified` files of `platform_id` by `rel_path`, and their total.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let psx = mistarr_core::PlatformId("psx".into());
/// let (items, total) = mistarr_server::db::files::unidentified(&conn, &psx, 0, 50).unwrap();
/// assert!(items.is_empty() && total == 0);
/// ```
pub fn unidentified(
    conn: &Connection,
    platform_id: &PlatformId,
    offset: u32,
    limit: u32,
) -> Result<(Vec<UnidentifiedFile>, u64)> {
    let items = conn
        .prepare_cached(
            "SELECT rel_path, size, COALESCE(reason, 'corrupt') FROM files
             WHERE state = 'unidentified' AND platform_id = ?1
             ORDER BY rel_path LIMIT ?2 OFFSET ?3",
        )?
        .query_map(params![platform_id.0, limit, offset], |r| {
            Ok(UnidentifiedFile {
                rel_path: r.get(0)?,
                size: r.get(1)?,
                reason: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let total: i64 = conn
        .prepare_cached(
            "SELECT COUNT(*) FROM files WHERE state = 'unidentified' AND platform_id = ?1",
        )?
        .query_row([&platform_id.0], |r| r.get(0))?;
    Ok((items, u64::try_from(total).unwrap_or(0)))
}

/// Up to `limit` files on `platform_id` matched to a rom that is retired or whose title is.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let nes = mistarr_core::PlatformId("nes".into());
/// assert!(mistarr_server::db::files::retired_matches(&conn, &nes, 10).unwrap().is_empty());
/// ```
pub fn retired_matches(
    conn: &Connection,
    platform_id: &PlatformId,
    limit: u32,
) -> Result<Vec<FileRow>> {
    let cols = COLUMNS
        .split(", ")
        .map(|c| format!("f.{c}"))
        .collect::<Vec<_>>()
        .join(", ");
    Ok(conn
        .prepare_cached(&format!(
            "SELECT {cols} FROM files f JOIN roms r ON r.id = f.rom_id
             JOIN titles t ON t.id = r.title_id
             WHERE f.platform_id = ?1 AND t.source = 'dat' AND (r.retired = 1 OR t.retired = 1)
             ORDER BY f.id LIMIT ?2"
        ))?
        .query_map(params![platform_id.0, limit], from_row)?
        .collect::<rusqlite::Result<_>>()?)
}

/// Up to `limit` files on `platform_id` with an id above `after` that no rom matches:
/// `rom_id` NULL, `unverified`, and fully hashed, with a stored sha1 or md5; a CRC32
/// alone never decides a match. Ordered by id, so a caller pages with the last id it
/// saw and a file that stays unmatched is read once.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::files::{unmatched_after, FileId};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let nes = mistarr_core::PlatformId("nes".into());
/// assert!(unmatched_after(&conn, &nes, FileId(0), 10).unwrap().is_empty());
/// ```
pub fn unmatched_after(
    conn: &Connection,
    platform_id: &PlatformId,
    after: FileId,
    limit: u32,
) -> Result<Vec<FileRow>> {
    Ok(conn
        .prepare_cached(&format!(
            "SELECT {COLUMNS} FROM files
             WHERE platform_id = ?1 AND state = 'unverified' AND rom_id IS NULL AND id > ?2
               AND (sha1 IS NOT NULL OR md5 IS NOT NULL)
             ORDER BY id LIMIT ?3"
        ))?
        .query_map(params![platform_id.0, after.0, limit], from_row)?
        .collect::<rusqlite::Result<_>>()?)
}

/// The files of `platform_id` directly inside the directory `dir` (relative to `games/`),
/// matched case-sensitively, in `rel_path` order. One range seek on the unique
/// `(platform_id, rel_path)` key, so its cost follows the directory, not the platform.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let psx = mistarr_core::PlatformId("psx".into());
/// assert!(mistarr_server::db::files::in_directory(&conn, &psx, "PSX/Example").unwrap().is_empty());
/// ```
pub fn in_directory(
    conn: &Connection,
    platform_id: &PlatformId,
    dir: &str,
) -> Result<Vec<FileRow>> {
    // `dir/` up to `dir0` (`0` follows `/`) is every path inside `dir`, byte for byte.
    let (prefix, to) = (format!("{dir}/"), format!("{dir}0"));
    let rows: Vec<FileRow> = conn
        .prepare_cached(&format!(
            "SELECT {COLUMNS} FROM files
             WHERE platform_id = ?1 AND rel_path >= ?2 AND rel_path < ?3 ORDER BY rel_path"
        ))?
        .query_map(params![platform_id.0, prefix, to], from_row)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows
        .into_iter()
        .filter(|r| !r.rel_path[prefix.len()..].contains('/'))
        .collect())
}

/// [`match_rom`] over live roms of live titles only.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let nes = mistarr_core::PlatformId("nes".into());
/// assert!(mistarr_server::db::files::match_live_rom(&conn, &nes, "", "", "0", 1).unwrap().is_none());
/// ```
pub fn match_live_rom(
    conn: &Connection,
    platform_id: &PlatformId,
    sha1: &str,
    md5: &str,
    crc32: &str,
    size: i64,
) -> Result<Option<RomMatch>> {
    let live = "r.retired = 0 AND t.retired = 0 AND ";
    match_among(conn, platform_id, [sha1, md5, crc32], size, live)
}

/// Sets the matched rom and state of one file.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::files::{set_match, FileId, FileState};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// set_match(&conn, FileId(1), None, FileState::Unverified).unwrap();
/// ```
pub fn set_match(
    conn: &Connection,
    id: FileId,
    rom_id: Option<i64>,
    state: FileState,
) -> Result<()> {
    conn.prepare_cached("UPDATE files SET rom_id = ?2, state = ?3 WHERE id = ?1")?
        .execute(params![id.0, rom_id, state.as_str()])?;
    Ok(())
}

/// Whether any rom of this platform has this CRC32 and size, the pre-check
/// that decides whether a zip member is worth decompressing.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn crc_candidate_exists(
    conn: &Connection,
    platform_id: &PlatformId,
    crc32: &str,
    size: i64,
) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM roms r JOIN titles t ON t.id = r.title_id
           WHERE t.platform_id = ?1 AND t.source = 'dat' AND r.crc32 = ?2 AND r.size = ?3)",
        params![platform_id.0, crc32, size],
        |r| r.get(0),
    )?)
}

/// Number of roms belonging to a title, for the disc all-or-nothing rule.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn count_roms_for_title(conn: &Connection, title_id: i64) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM roms WHERE title_id = ?1",
        [title_id],
        |r| r.get(0),
    )?)
}

/// File counts by state for one platform, for the platforms card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct StateCounts {
    /// Rows `verified`.
    pub verified: u64,
    /// Rows `misnamed`.
    pub misnamed: u64,
    /// Rows `unverified`.
    pub unverified: u64,
    /// Rows `bad`.
    pub bad: u64,
    /// Rows `pending`.
    pub pending: u64,
    /// Rows `unidentified`.
    pub unidentified: u64,
}

/// File state counts for a single platform.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn state_counts(conn: &Connection, platform_id: &PlatformId) -> Result<StateCounts> {
    let mut stmt =
        conn.prepare("SELECT state, COUNT(*) FROM files WHERE platform_id = ?1 GROUP BY state")?;
    let mut counts = StateCounts::default();
    let rows = stmt.query_map([&platform_id.0], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (state, n) = row?;
        let n = u64::try_from(n).unwrap_or(0);
        match FileState::parse(&state) {
            Some(FileState::Verified) => counts.verified = n,
            Some(FileState::Misnamed) => counts.misnamed = n,
            Some(FileState::Unverified) => counts.unverified = n,
            Some(FileState::Bad) => counts.bad = n,
            Some(FileState::Unidentified) => counts.unidentified = n,
            Some(FileState::Pending) | None => counts.pending = n,
        }
    }
    Ok(counts)
}

/// The directories already committed for a platform's in-progress scan.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure or unparseable JSON.
pub fn scan_progress(conn: &Connection, platform_id: &PlatformId) -> Result<Vec<String>> {
    let stored: Option<String> = conn
        .query_row(
            "SELECT done_dirs FROM scan_progress WHERE platform_id = ?1",
            [&platform_id.0],
            |r| r.get(0),
        )
        .optional()?;
    Ok(match stored {
        Some(json) => serde_json::from_str(&json).map_err(|e| crate::Error::Stored {
            key: "scan_progress".to_owned(),
            source: e,
        })?,
        None => Vec::new(),
    })
}

/// Replaces the committed-directory list for a platform's scan.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn save_scan_progress(
    conn: &Connection,
    platform_id: &PlatformId,
    done_dirs: &[String],
    now: i64,
) -> Result<()> {
    let json = serde_json::to_string(done_dirs).unwrap_or_else(|_| "[]".to_owned());
    conn.execute(
        "INSERT INTO scan_progress (platform_id, done_dirs, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(platform_id) DO UPDATE SET done_dirs = excluded.done_dirs, updated_at = excluded.updated_at",
        params![platform_id.0, json, now],
    )?;
    Ok(())
}

/// Clears a platform's scan progress once its scan finished.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn clear_scan_progress(conn: &Connection, platform_id: &PlatformId) -> Result<()> {
    conn.execute(
        "DELETE FROM scan_progress WHERE platform_id = ?1",
        [&platform_id.0],
    )?;
    Ok(())
}

/// Platforms with saved scan progress, for re-enqueuing an interrupted scan
/// at startup.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn platforms_with_progress(conn: &Connection) -> Result<Vec<PlatformId>> {
    let mut stmt = conn.prepare("SELECT platform_id FROM scan_progress")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    Ok(rows
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .map(PlatformId)
        .collect())
}

/// Inserts a title and its `dat_version` directly, bypassing the DAT
/// importer. A fixture for this package's tests until WP-10's importer lands.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn seed_title_fixture(
    conn: &Connection,
    platform_id: &PlatformId,
    game_name: &str,
) -> Result<i64> {
    // Each fixture is its own dat_version: version must be unique per dat_name.
    let version = conn.query_row("SELECT COUNT(*) + 1 FROM dat_versions", [], |r| {
        r.get::<_, i64>(0)
    })?;
    conn.execute(
        "INSERT INTO dat_versions (platform_id, dat_name, version, source_file, loaded_at, game_count)
         VALUES (?1, 'fixture', ?2, 'fixture.dat', 0, 1)",
        params![platform_id.0, version.to_string()],
    )?;
    let dat_version_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO titles (platform_id, dat_version_id, name, base_name)
         VALUES (?1, ?2, ?3, ?3)",
        params![platform_id.0, dat_version_id, game_name],
    )?;
    let title_id = conn.last_insert_rowid();
    conn.execute("UPDATE titles SET parent_id = ?1 WHERE id = ?1", [title_id])?;
    Ok(title_id)
}

/// Inserts a rom under an existing title, for a fixture built with
/// [`seed_title_fixture`].
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn seed_rom_for_title_fixture(
    conn: &Connection,
    title_id: i64,
    rom_name: &str,
    hashes: &mistarr_core::HashSet,
    status: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO roms (title_id, name, size, crc32, md5, sha1, status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            title_id,
            rom_name,
            i64::try_from(hashes.size).unwrap_or(i64::MAX),
            hashes.crc32,
            hashes.md5,
            hashes.sha1,
            status,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Inserts a title, `dat_version` and one rom directly, bypassing the DAT
/// importer. A fixture for this package's tests until WP-10's importer lands.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn seed_rom_fixture(
    conn: &Connection,
    platform_id: &PlatformId,
    game_name: &str,
    rom_name: &str,
    hashes: &mistarr_core::HashSet,
    status: &str,
) -> Result<i64> {
    let title_id = seed_title_fixture(conn, platform_id, game_name)?;
    seed_rom_for_title_fixture(conn, title_id, rom_name, hashes, status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mistarr_core::HashSet;

    fn conn() -> Connection {
        let mut c = Connection::open_in_memory().expect("open");
        crate::db::migrate::apply(&mut c).expect("migrate");
        crate::db::platforms::seed(&mut c, &mistarr_mister::platforms::PLATFORMS).expect("seed");
        c
    }

    fn hashes(size: u64) -> HashSet {
        HashSet {
            size,
            crc32: "352441c2".into(),
            md5: "900150983cd24fb0d6963f7d28e17f72".into(),
            sha1: "a9993e364706816aba3e25717850c26c9cd0d89d".into(),
        }
    }

    #[test]
    fn upsert_and_find_round_trip() {
        let c = conn();
        let pid = PlatformId("nes".into());
        let h = Hashed {
            crc32: Some("352441c2"),
            md5: None,
            sha1: None,
            header_rule: Some("ines"),
        };
        let id = upsert(
            &c,
            &pid,
            "a.nes",
            3,
            10,
            &h,
            None,
            FileState::Unverified,
            20,
        )
        .expect("insert");
        let row = find_by_path(&c, &pid, "a.nes").expect("find").expect("row");
        assert_eq!(row.id, id);
        assert_eq!(row.state, FileState::Unverified);
        assert_eq!(row.crc32.as_deref(), Some("352441c2"));

        let same_id =
            upsert(&c, &pid, "a.nes", 3, 11, &h, None, FileState::Verified, 21).expect("update");
        assert_eq!(same_id, id);
        let row = find_by_path(&c, &pid, "a.nes").expect("find").expect("row");
        assert_eq!(row.mtime, 11);
        assert_eq!(row.state, FileState::Verified);
    }

    #[test]
    fn delete_missing_removes_only_absent_paths() {
        let mut c = conn();
        let pid = PlatformId("nes".into());
        let h = Hashed::default();
        upsert(
            &c,
            &pid,
            "keep.nes",
            1,
            1,
            &h,
            None,
            FileState::Unverified,
            1,
        )
        .expect("insert");
        upsert(
            &c,
            &pid,
            "gone.nes",
            1,
            1,
            &h,
            None,
            FileState::Unverified,
            1,
        )
        .expect("insert");
        let removed = delete_missing(&mut c, &pid, &["keep.nes".to_owned()], &[]).expect("delete");
        assert_eq!(removed, 1);
        assert!(find_by_path(&c, &pid, "keep.nes").expect("find").is_some());
        assert!(find_by_path(&c, &pid, "gone.nes").expect("find").is_none());
    }

    /// A file an import placed and logged can still be pruned once it is gone from
    /// disk: its `import_log.file_id` is cleared, since the FK has no delete action.
    #[test]
    fn delete_missing_clears_the_dangling_import_log_reference() {
        let mut c = conn();
        let pid = PlatformId("nes".into());
        let h = Hashed::default();
        let id =
            upsert(&c, &pid, "gone.nes", 1, 1, &h, None, FileState::Verified, 1).expect("insert");
        c.execute(
            "INSERT INTO import_log (at, file_id, action, detail) VALUES (1, ?1, 'placed', '{}')",
            [id.0],
        )
        .expect("log");
        let removed = delete_missing(&mut c, &pid, &[], &[]).expect("delete");
        assert_eq!(removed, 1);
        let logged: Option<i64> = c
            .query_row("SELECT file_id FROM import_log", [], |r| r.get(0))
            .expect("row");
        assert_eq!(logged, None);
    }

    #[test]
    fn zip_rows_and_directory_pages_use_key_ranges() {
        let c = conn();
        let pid = PlatformId("arcade".into());
        let h = Hashed::default();
        for rel in [
            "mame/a.zip",
            "mame/a.zip#x.bin",
            "mame/a.zip#y.bin",
            "mame/a.zip.zip#z.bin",
            "mame/b.zip#x.bin",
            "mame0.txt",
            "hbmame/c.zip",
        ] {
            upsert(&c, &pid, rel, 1, 1, &h, None, FileState::Unverified, 1).expect("insert");
        }
        let members: Vec<String> = zip_member_rows(&c, &pid, "mame/a.zip")
            .expect("members")
            .into_iter()
            .map(|r| r.rel_path)
            .collect();
        assert_eq!(members, ["mame/a.zip#x.bin", "mame/a.zip#y.bin"]);
        let first = paths_under(&c, &pid, "mame", "", 3).expect("page");
        assert_eq!(
            first,
            ["mame/a.zip", "mame/a.zip#x.bin", "mame/a.zip#y.bin"]
        );
        let rest = paths_under(&c, &pid, "mame", &first[2], 3).expect("page");
        assert_eq!(rest, ["mame/a.zip.zip#z.bin", "mame/b.zip#x.bin"]);
    }

    #[test]
    fn in_directory_lists_a_directory_s_own_files_by_exact_prefix() {
        let c = conn();
        let psx = PlatformId("psx".into());
        let h = Hashed::default();
        let paths = [
            "PSX/Disc",
            "PSX/Disc.bin",
            "PSX/Disc/a.cue",
            "PSX/Disc/a (Track 1).bin",
            "PSX/Disc/sub/b.bin",
            "PSX/Disc 2/c.bin",
            "PSX/Disc0/d.bin",
            "PSX/disc/e.bin",
            "PSX/Díşc/f.bin",
            "PSX/Díşc/g/h.bin",
        ];
        for rel in paths {
            upsert(&c, &psx, rel, 1, 1, &h, None, FileState::Unverified, 1).expect("insert");
        }
        let other = PlatformId("saturn".into());
        upsert(
            &c,
            &other,
            "PSX/Disc/x.bin",
            1,
            1,
            &h,
            None,
            FileState::Unverified,
            1,
        )
        .expect("insert");
        for dir in [
            "PSX/Disc",
            "PSX/disc",
            "PSX/Díşc",
            "PSX/Disc 2",
            "PSX",
            "PSX/None",
        ] {
            let prefix = format!("{dir}/");
            let mut want: Vec<&str> = paths
                .iter()
                .copied()
                .filter(|p| {
                    p.strip_prefix(&prefix)
                        .is_some_and(|rest| !rest.contains('/'))
                })
                .collect();
            want.sort_unstable();
            let got: Vec<String> = in_directory(&c, &psx, dir)
                .expect("list")
                .into_iter()
                .map(|r| r.rel_path)
                .collect();
            assert_eq!(got, want, "{dir}");
        }
    }

    #[test]
    fn zip_rows_nocase_match_any_spelling() {
        let c = conn();
        let pid = PlatformId("arcade".into());
        let h = Hashed::default();
        for rel in [
            "mame/Foo.zip",
            "mame/Foo.zip#a.bin",
            "mame/FOO.ZIP#b.bin",
            "mame/foo2.zip",
        ] {
            upsert(&c, &pid, rel, 1, 1, &h, None, FileState::Unverified, 1).expect("insert");
        }
        let (bare, members) = zip_rows_nocase(&c, &pid, "mame/foo.zip").expect("rows");
        let names = |rows: Vec<FileRow>| rows.into_iter().map(|r| r.rel_path).collect::<Vec<_>>();
        assert_eq!(names(bare), ["mame/Foo.zip"]);
        assert_eq!(names(members), ["mame/FOO.ZIP#b.bin", "mame/Foo.zip#a.bin"]);
    }

    #[test]
    fn restamp_keeps_hashes_and_reverify_clears_them() {
        let c = conn();
        let pid = PlatformId("arcade".into());
        let h = Hashed {
            crc32: Some("0000abcd"),
            md5: Some("m"),
            sha1: Some("s"),
            header_rule: Some("none"),
        };
        let rom = seed_rom_fixture(&c, &pid, "exampleset", "a", &hashes(1), "good").expect("rom");
        let a = upsert(
            &c,
            &pid,
            "mame/a.zip#a",
            1,
            1,
            &h,
            Some(rom),
            FileState::Verified,
            1,
        )
        .expect("insert");
        let b = upsert(
            &c,
            &pid,
            "mame/a.zip#b",
            1,
            1,
            &h,
            Some(rom),
            FileState::Verified,
            1,
        )
        .expect("insert");
        restamp(&c, a, 9, 2).expect("restamp");
        reverify(&c, b, 5, 9, "ffff0000", 2).expect("reverify");
        let a = get(&c, a).expect("get").expect("row");
        assert_eq!(
            (a.mtime, a.state, a.md5.as_deref()),
            (9, FileState::Verified, Some("m"))
        );
        let b = get(&c, b).expect("get").expect("row");
        assert_eq!(
            (
                b.size,
                b.mtime,
                b.crc32.as_deref(),
                b.md5,
                b.sha1,
                b.rom_id,
                b.state
            ),
            (
                5,
                9,
                Some("ffff0000"),
                None,
                None,
                Some(rom),
                FileState::Unverified
            )
        );
    }

    #[test]
    fn delete_paths_removes_a_set_and_clears_its_log_references() {
        let c = conn();
        let pid = PlatformId("nes".into());
        let h = Hashed::default();
        let gone = upsert(&c, &pid, "a.nes", 1, 1, &h, None, FileState::Verified, 1).expect("a");
        upsert(&c, &pid, "b.nes", 1, 1, &h, None, FileState::Verified, 1).expect("b");
        let kept = upsert(&c, &pid, "c.nes", 1, 1, &h, None, FileState::Verified, 1).expect("c");
        for id in [gone, kept] {
            c.execute(
                "INSERT INTO import_log (at, file_id, action, detail) VALUES (1, ?1, 'placed', '{}')",
                [id.0],
            )
            .expect("log");
        }
        let paths = [
            "a.nes".to_owned(),
            "b.nes".to_owned(),
            "nope.nes".to_owned(),
        ];
        assert_eq!(delete_paths(&c, &pid, &paths).expect("delete"), 2);
        assert_eq!(delete_paths(&c, &pid, &[]).expect("delete"), 0);
        let logged: Vec<Option<i64>> = c
            .prepare("SELECT file_id FROM import_log ORDER BY id")
            .expect("prepare")
            .query_map([], |r| r.get(0))
            .expect("query")
            .collect::<rusqlite::Result<_>>()
            .expect("rows");
        assert_eq!(logged, [None, Some(kept.0)]);
    }

    #[test]
    fn delete_missing_keeps_rows_under_an_unreadable_directory() {
        let mut c = conn();
        let pid = PlatformId("nes".into());
        let h = Hashed::default();
        for rel in ["NES/a.nes", "NES/sub/b.nes", "NESX/c.nes", "d.nes"] {
            upsert(&c, &pid, rel, 1, 1, &h, None, FileState::Verified, 1).expect("insert");
        }
        let removed = delete_missing(&mut c, &pid, &[], &["NES".to_owned()]).expect("delete");
        assert_eq!(removed, 2, "only rows outside NES/ go");
        assert!(find_by_path(&c, &pid, "NES/a.nes").expect("find").is_some());
        assert!(find_by_path(&c, &pid, "NES/sub/b.nes")
            .expect("find")
            .is_some());
        assert!(find_by_path(&c, &pid, "NESX/c.nes")
            .expect("find")
            .is_none());
    }

    #[test]
    fn delete_missing_spans_several_batches() {
        let mut c = conn();
        let pid = PlatformId("nes".into());
        let h = Hashed::default();
        let n = DELETE_BATCH * 2 + 7;
        for i in 0..n {
            upsert(
                &c,
                &pid,
                &format!("g{i}.nes"),
                1,
                1,
                &h,
                None,
                FileState::Unverified,
                1,
            )
            .expect("insert");
        }
        assert_eq!(delete_missing(&mut c, &pid, &[], &[]).expect("delete"), n);
        assert!(existing_paths(&c, &pid).expect("paths").is_empty());
    }

    /// `keep` is deduplicated through a set rather than scanned per row, so a
    /// repeated entry does not change the count removed.
    #[test]
    fn delete_missing_keep_lookup_is_set_based() {
        let mut c = conn();
        let pid = PlatformId("nes".into());
        let h = Hashed::default();
        for i in 0..50 {
            upsert(
                &c,
                &pid,
                &format!("game{i}.nes"),
                1,
                1,
                &h,
                None,
                FileState::Unverified,
                1,
            )
            .expect("insert");
        }
        let keep: Vec<String> = std::iter::repeat_n("game0.nes".to_owned(), 10)
            .chain(std::iter::repeat_n("game1.nes".to_owned(), 5))
            .collect();
        let removed = delete_missing(&mut c, &pid, &keep, &[]).expect("delete");
        assert_eq!(removed, 48);
        assert!(find_by_path(&c, &pid, "game0.nes").expect("find").is_some());
        assert!(find_by_path(&c, &pid, "game1.nes").expect("find").is_some());
    }

    #[test]
    fn match_rom_prefers_sha1_then_md5_then_crc() {
        let c = conn();
        let pid = PlatformId("nes".into());
        let h = hashes(3);
        seed_rom_fixture(
            &c,
            &pid,
            "Example Quest (USA)",
            "Example Quest (USA).nes",
            &h,
            "good",
        )
        .expect("seed");
        let m = match_rom(&c, &pid, &h.sha1, &h.md5, &h.crc32, 3)
            .expect("match")
            .expect("found");
        assert_eq!(m.name, "Example Quest (USA).nes");
        assert_eq!(m.status, "good");

        // A wrong sha1 with the right md5 falls through to the md5 match
        // only because this rom has no sha1 recorded.
        let mut sha1_less = hashes(4);
        sha1_less.sha1 = "0000000000000000000000000000000000000000".into();
        conn_insert_sha1_less_rom(&c, &pid, &sha1_less);
        let m = match_rom(
            &c,
            &pid,
            "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
            &sha1_less.md5,
            &sha1_less.crc32,
            4,
        )
        .expect("match")
        .expect("found");
        assert!(m.rom_id > 0);
    }

    fn conn_insert_sha1_less_rom(c: &Connection, pid: &PlatformId, h: &HashSet) {
        c.execute(
            "INSERT INTO dat_versions (platform_id, dat_name, version, source_file, loaded_at, game_count)
             VALUES (?1, 'fixture2', '1', 'f2.dat', 0, 1)",
            [&pid.0],
        )
        .expect("dat");
        let dv = c.last_insert_rowid();
        c.execute(
            "INSERT INTO titles (platform_id, dat_version_id, name, base_name)
             VALUES (?1, ?2, 'Other Quest', 'Other Quest')",
            params![pid.0, dv],
        )
        .expect("title");
        let title = c.last_insert_rowid();
        c.execute(
            "INSERT INTO roms (title_id, name, size, crc32, md5, sha1, status)
             VALUES (?1, 'Other Quest.nes', ?2, ?3, ?4, NULL, 'good')",
            params![
                title,
                i64::try_from(h.size).expect("test size"),
                h.crc32,
                h.md5
            ],
        )
        .expect("rom");
    }

    #[test]
    fn match_rom_prefers_non_superseded_dat_version() {
        let c = conn();
        let pid = PlatformId("nes".into());
        let h = hashes(3);
        let old_dv = seed_dat_version(&c, &pid, "old");
        let title_old = seed_title(&c, &pid, old_dv, "Old Pick");
        seed_rom(&c, title_old, "Old Pick.nes", &h, "good");
        let new_dv = seed_dat_version(&c, &pid, "new");
        c.execute(
            "UPDATE dat_versions SET superseded_by = NULL WHERE id = ?1",
            [old_dv],
        )
        .expect("noop");
        c.execute(
            "UPDATE dat_versions SET superseded_by = ?1 WHERE id = ?2",
            [new_dv, old_dv],
        )
        .expect("supersede");
        let title_new = seed_title(&c, &pid, new_dv, "New Pick");
        seed_rom(&c, title_new, "New Pick.nes", &h, "good");

        let m = match_rom(&c, &pid, &h.sha1, &h.md5, &h.crc32, 3)
            .expect("match")
            .expect("found");
        assert_eq!(m.name, "New Pick.nes");
    }

    fn seed_dat_version(c: &Connection, pid: &PlatformId, tag: &str) -> i64 {
        c.execute(
            "INSERT INTO dat_versions (platform_id, dat_name, version, source_file, loaded_at, game_count)
             VALUES (?1, ?2, '1', 'x.dat', 0, 1)",
            params![pid.0, tag],
        )
        .expect("dat");
        c.last_insert_rowid()
    }

    fn seed_title(c: &Connection, pid: &PlatformId, dv: i64, name: &str) -> i64 {
        c.execute(
            "INSERT INTO titles (platform_id, dat_version_id, name, base_name)
             VALUES (?1, ?2, ?3, ?3)",
            params![pid.0, dv, name],
        )
        .expect("title");
        c.last_insert_rowid()
    }

    fn seed_rom(c: &Connection, title_id: i64, name: &str, h: &HashSet, status: &str) {
        c.execute(
            "INSERT INTO roms (title_id, name, size, crc32, md5, sha1, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                title_id,
                name,
                i64::try_from(h.size).expect("test size"),
                h.crc32,
                h.md5,
                h.sha1,
                status
            ],
        )
        .expect("rom");
    }

    #[test]
    fn count_roms_for_title_counts_only_that_title() {
        let c = conn();
        let pid = PlatformId("psx".into());
        let h = hashes(3);
        let title = seed_rom_fixture(&c, &pid, "Disc Quest", "Disc Quest.cue", &h, "good")
            .map(|rom_id| {
                c.query_row("SELECT title_id FROM roms WHERE id = ?1", [rom_id], |r| {
                    r.get(0)
                })
                .expect("title_id")
            })
            .expect("seed");
        seed_rom(&c, title, "Disc Quest (Track 1).bin", &hashes(10), "good");
        assert_eq!(count_roms_for_title(&c, title).expect("count"), 2);
    }

    #[test]
    fn scan_progress_round_trips_and_clears() {
        let c = conn();
        let pid = PlatformId("nes".into());
        assert!(scan_progress(&c, &pid).expect("empty").is_empty());
        save_scan_progress(&c, &pid, &["NES".to_owned()], 5).expect("save");
        assert_eq!(scan_progress(&c, &pid).expect("get"), ["NES".to_owned()]);
        let got = platforms_with_progress(&c).expect("list");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, pid.0);
        clear_scan_progress(&c, &pid).expect("clear");
        assert!(scan_progress(&c, &pid).expect("empty").is_empty());
        assert!(platforms_with_progress(&c).expect("list").is_empty());
    }

    #[test]
    fn state_counts_group_by_state() {
        let c = conn();
        let pid = PlatformId("nes".into());
        let h = Hashed::default();
        upsert(&c, &pid, "a.nes", 1, 1, &h, None, FileState::Verified, 1).expect("insert");
        upsert(&c, &pid, "b.nes", 1, 1, &h, None, FileState::Verified, 1).expect("insert");
        upsert(&c, &pid, "c.nes", 1, 1, &h, None, FileState::Unverified, 1).expect("insert");
        let counts = state_counts(&c, &pid).expect("counts");
        assert_eq!(counts.verified, 2);
        assert_eq!(counts.unverified, 1);
        assert_eq!(counts.bad, 0);
    }

    #[test]
    fn get_and_move_to_follow_a_rename() {
        let c = conn();
        let pid = PlatformId("nes".into());
        let h = Hashed::default();
        let id = upsert(
            &c,
            &pid,
            "NES/a.nes",
            1,
            1,
            &h,
            None,
            FileState::Misnamed,
            1,
        )
        .expect("insert");
        move_to(&c, id, "NES/b.nes", FileState::Verified, 7, 8).expect("move");
        let row = get(&c, id).expect("get").expect("row");
        assert_eq!(
            (row.rel_path.as_str(), row.state, row.mtime),
            ("NES/b.nes", FileState::Verified, 7)
        );
        assert!(get(&c, FileId(999)).expect("get").is_none());
    }

    #[test]
    fn zip_members_verified_roms_and_deletes() {
        let c = conn();
        let pid = PlatformId("neogeo".into());
        let h = Hashed::default();
        let rom =
            seed_rom_fixture(&c, &pid, "Example Set", "a.rom", &hashes(3), "good").expect("rom");
        assert!(!has_verified(&c, rom).expect("none"));
        let a = upsert(
            &c,
            &pid,
            "NeoGeo/set.zip#a.rom",
            1,
            1,
            &h,
            Some(rom),
            FileState::Verified,
            1,
        )
        .expect("a");
        upsert(
            &c,
            &pid,
            "NeoGeo/set.zip#b.rom",
            1,
            1,
            &h,
            None,
            FileState::Unverified,
            1,
        )
        .expect("b");
        upsert(
            &c,
            &pid,
            "NeoGeo/set.zip2#c.rom",
            1,
            1,
            &h,
            None,
            FileState::Unverified,
            1,
        )
        .expect("c");
        let rows = zip_member_rows(&c, &pid, "NeoGeo/set.zip").expect("rows");
        assert_eq!(rows.len(), 2);
        assert!(has_verified(&c, rom).expect("some"));
        delete(&c, a).expect("delete");
        assert!(get(&c, a).expect("get").is_none());
        assert!(!has_verified(&c, rom).expect("gone"));
    }

    #[test]
    fn match_rom_prefers_a_live_rom() {
        let c = conn();
        let pid = PlatformId("nes".into());
        let h = hashes(3);
        let dv = seed_dat_version(&c, &pid, "only");
        let gone = seed_title(&c, &pid, dv, "Gone Quest");
        seed_rom(&c, gone, "Gone Quest.nes", &h, "good");
        c.execute("UPDATE roms SET retired = 1 WHERE title_id = ?1", [gone])
            .expect("retire");
        let live = seed_title(&c, &pid, dv, "Live Quest");
        seed_rom(&c, live, "Live Quest.nes", &h, "good");
        let m = match_rom(&c, &pid, &h.sha1, &h.md5, &h.crc32, 3)
            .expect("match")
            .expect("found");
        assert_eq!(m.name, "Live Quest.nes");
    }

    #[test]
    fn states_round_trip() {
        for s in ["pending", "verified", "misnamed", "unverified", "bad"] {
            assert_eq!(FileState::parse(s).map(FileState::as_str), Some(s));
        }
        assert_eq!(FileId(4).to_string(), "4");
    }

    #[test]
    fn unidentified_rows_keep_their_state_and_reason() {
        let c = conn();
        let pid = PlatformId("psx".into());
        assert_eq!(
            FileState::parse("unidentified"),
            Some(FileState::Unidentified)
        );
        for (rel, reason) in [
            ("PSX/B/b.chd", "off"),
            ("PSX/A/a.chd", "cooked"),
            ("PSX/C/c.chd", "pending"),
        ] {
            let row = NewFile {
                rel_path: rel.into(),
                size: 5,
                mtime: 1,
                crc32: None,
                md5: None,
                sha1: None,
                header_rule: Some("chd".into()),
                rom_id: None,
                state: FileState::Unidentified,
                reason: Some(reason.into()),
            };
            upsert_row(&c, &pid, &row, 1).expect("upsert");
        }
        let row = find_by_path(&c, &pid, "PSX/A/a.chd")
            .expect("find")
            .expect("row");
        assert_eq!(
            (row.state, row.reason.as_deref()),
            (FileState::Unidentified, Some("cooked"))
        );
        let h = Hashed::default();
        upsert(
            &c,
            &pid,
            "PSX/A/a.chd",
            5,
            1,
            &h,
            None,
            FileState::Unverified,
            2,
        )
        .expect("upsert");
        let row = find_by_path(&c, &pid, "PSX/A/a.chd")
            .expect("find")
            .expect("row");
        assert_eq!(row.reason, None, "a plain upsert clears the reason");

        let (page, total) = unidentified(&c, &pid, 0, 1).expect("page");
        assert_eq!(total, 2);
        assert_eq!(page[0].rel_path, "PSX/B/b.chd");
        assert_eq!(page[0].reason, "off");
        let (page, _) = unidentified(&c, &pid, 1, 5).expect("page");
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].rel_path, "PSX/C/c.chd");
        assert_eq!(state_counts(&c, &pid).expect("counts").unidentified, 2);
    }

    #[test]
    fn roms_matching_returns_every_live_rom_of_the_first_tier() {
        let c = conn();
        let pid = PlatformId("psx".into());
        let h = hashes(2352);
        let a = seed_title_fixture(&c, &pid, "Disc A").expect("title");
        let b = seed_title_fixture(&c, &pid, "Disc B").expect("title");
        let ra = seed_rom_for_title_fixture(&c, a, "a.bin", &h, "good").expect("rom");
        let rb = seed_rom_for_title_fixture(&c, b, "b.bin", &h, "good").expect("rom");
        let got: Vec<i64> = roms_matching(&c, &pid, &h)
            .expect("match")
            .iter()
            .map(|m| m.rom_id)
            .collect();
        assert_eq!(got, [ra, rb]);
        c.execute("UPDATE roms SET retired = 1 WHERE id = ?1", [rb])
            .expect("retire");
        let got: Vec<i64> = roms_matching(&c, &pid, &h)
            .expect("match")
            .iter()
            .map(|m| m.rom_id)
            .collect();
        assert_eq!(got, [ra], "retired roms never match");
        let crc_only = HashSet {
            sha1: "f".repeat(40),
            md5: "e".repeat(32),
            ..h.clone()
        };
        assert!(
            roms_matching(&c, &pid, &crc_only)
                .expect("match")
                .is_empty(),
            "sha1 roms need sha1"
        );
        assert!(roms_matching(&c, &PlatformId("saturn".into()), &h)
            .expect("match")
            .is_empty());

        let roms: Vec<String> = disc_roms(&c, a)
            .expect("roms")
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert_eq!(roms, ["a.bin"]);
        assert!(
            disc_roms(&c, b).expect("roms").is_empty(),
            "a retired rom is gone"
        );
    }

    #[test]
    fn chd_rom_sized_finds_whole_file_chd_roms_by_size() {
        let c = conn();
        let pid = PlatformId("psx".into());
        let t = seed_title_fixture(&c, &pid, "Disc").expect("title");
        seed_rom_for_title_fixture(&c, t, "Disc.CHD", &hashes(4096), "good").expect("rom");
        seed_rom_for_title_fixture(&c, t, "Disc.bin", &hashes(8192), "good").expect("rom");
        assert!(chd_rom_sized(&c, &pid, 4096).expect("sized"));
        assert!(!chd_rom_sized(&c, &pid, 8192).expect("a bin is not a chd"));
        assert!(!chd_rom_sized(&c, &PlatformId("saturn".into()), 4096).expect("other platform"));
    }

    #[test]
    fn delete_ids_clears_the_import_log_first() {
        let c = conn();
        let pid = PlatformId("psx".into());
        let h = Hashed::default();
        let id = upsert(
            &c,
            &pid,
            "PSX/G/g.chd",
            1,
            1,
            &h,
            None,
            FileState::Unverified,
            1,
        )
        .expect("upsert");
        c.execute(
            "INSERT INTO import_log (at, file_id, action, detail) VALUES (0, ?1, 'placed', '{}')",
            [id.0],
        )
        .expect("log");
        c.execute_batch("PRAGMA foreign_keys = ON").expect("fk");
        assert_eq!(delete_ids(&c, &[id.0, 999]).expect("delete"), 1);
        let logged: Option<i64> = c
            .query_row("SELECT file_id FROM import_log", [], |r| r.get(0))
            .expect("log");
        assert_eq!(logged, None);
    }
}
