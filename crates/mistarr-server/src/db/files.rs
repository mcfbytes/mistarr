//! The `files` table; the files rows are matched to roms by `roms` and scanned against
//! `scan_progress`. See `docs/DATA-MODEL.md` "files" and "files.state".

use mistarr_core::PlatformId;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::Serialize;

use super::ids::{FileId, RomId, TitleId};
use super::sql::{self, text_enum, Page, Paged};
use crate::error::Result;

text_enum! {
    /// `files.state`.
    pub enum FileState {
        /// Seen, not yet hashed.
        Pending = "pending",
        /// Hash matches a rom and the filename is what the adapter expects.
        Verified = "verified",
        /// Hash matches a rom, name differs.
        Misnamed = "misnamed",
        /// No rom matches in any loaded DAT.
        Unverified = "unverified",
        /// Matches a rom flagged `baddump`.
        Bad = "bad",
        /// A disc image whose tracks are not identified yet or cannot be; `reason` says why.
        Unidentified = "unidentified",
    }
}

impl FileState {
    /// States whose file holds its rom's content, which a launch may load.
    pub const LOADABLE: [Self; 3] = [Self::Verified, Self::Misnamed, Self::Bad];
    /// [`FileState::LOADABLE`] as an SQL list.
    pub const LOADABLE_SQL: &'static str = "('verified', 'misnamed', 'bad')";
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
    /// The whole file's hashes under a rule that strips a header.
    pub whole: WholeHashes,
    /// The matched `roms.id`, when any.
    pub rom_id: Option<RomId>,
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
    /// The whole file's hashes under a rule that strips a header.
    pub whole: WholeHashes,
    /// The matched `roms.id`, when any.
    pub rom_id: Option<RomId>,
    /// The decided state.
    pub state: FileState,
    /// Why an `unidentified` row is not identified; `None` in every other state.
    pub reason: Option<String>,
}

impl NewFile {
    /// A row of `rel_path` with no hashes, rom or reason.
    ///
    /// ```
    /// use mistarr_server::db::files::{FileState, NewFile};
    /// let row = NewFile::unhashed("NES/a.nes", 4, 7, FileState::Pending);
    /// assert_eq!((row.size, row.mtime, row.crc32), (4, 7, None));
    /// ```
    #[must_use]
    pub fn unhashed(rel_path: &str, size: i64, mtime: i64, state: FileState) -> Self {
        Self {
            rel_path: rel_path.to_owned(),
            size,
            mtime,
            crc32: None,
            md5: None,
            sha1: None,
            header_rule: None,
            whole: WholeHashes::default(),
            rom_id: None,
            state,
            reason: None,
        }
    }
}

/// The whole file's hashes, header included, stored beside a row's hashes when it was
/// hashed under a rule that strips a header (`ines`, `a78`, `lnx`). They equal the row's
/// hashes when no header was found, and are all `None` under any other rule.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct WholeHashes {
    /// CRC32 of the whole file, also known for a member the pre-check did not decompress.
    pub crc32: Option<String>,
    /// MD5 of the whole file, when fully hashed.
    pub md5: Option<String>,
    /// SHA1 of the whole file, when fully hashed.
    pub sha1: Option<String>,
}

impl WholeHashes {
    /// What a row hashed under the rule named `rule` stores from `forms`.
    ///
    /// ```
    /// use mistarr_core::hash::{hash_forms, HeaderRule};
    /// use mistarr_server::db::files::WholeHashes;
    /// let forms = hash_forms(&b"abc"[..], HeaderRule::Ines, None).unwrap();
    /// assert_eq!(WholeHashes::of("ines", &forms).sha1, Some(forms.content.sha1.clone()));
    /// assert_eq!(WholeHashes::of("none", &forms), WholeHashes::default());
    /// ```
    #[must_use]
    pub fn of(rule: &str, forms: &mistarr_core::hash::HeaderForms) -> Self {
        Self::whole_file(rule, forms.whole_or_content())
    }

    /// What a row hashed under the rule named `rule` stores for a file whose whole hashes are `whole`.
    ///
    /// ```
    /// use mistarr_server::db::files::WholeHashes;
    /// let h = mistarr_core::HashSet { size: 1, crc32: "0".into(), md5: "1".into(), sha1: "2".into() };
    /// assert_eq!(WholeHashes::whole_file("lnx", &h).md5.as_deref(), Some("1"));
    /// assert_eq!(WholeHashes::whole_file("n64", &h), WholeHashes::default());
    /// ```
    #[must_use]
    pub fn whole_file(rule: &str, whole: &mistarr_core::HashSet) -> Self {
        if !rule
            .parse::<mistarr_core::hash::HeaderRule>()
            .unwrap_or_default()
            .strips_header()
        {
            return Self::default();
        }
        Self {
            crc32: Some(whole.crc32.clone()),
            md5: Some(whole.md5.clone()),
            sha1: Some(whole.sha1.clone()),
        }
    }
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

pub(crate) const COLUMNS: &str =
    "id, platform_id, rel_path, size, mtime, crc32, md5, sha1, header_rule, rom_id, state, scanned_at, reason, crc32_whole, md5_whole, sha1_whole";

pub(crate) fn from_row(r: &Row<'_>) -> rusqlite::Result<FileRow> {
    Ok(FileRow {
        id: r.get(0)?,
        platform_id: PlatformId(r.get(1)?),
        rel_path: r.get(2)?,
        size: r.get(3)?,
        mtime: r.get(4)?,
        crc32: r.get(5)?,
        md5: r.get(6)?,
        sha1: r.get(7)?,
        header_rule: r.get(8)?,
        whole: WholeHashes {
            crc32: r.get(13)?,
            md5: r.get(14)?,
            sha1: r.get(15)?,
        },
        rom_id: r.get(9)?,
        state: r.get(10)?,
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
        .prepare_cached(&format!(
            "SELECT {COLUMNS} FROM files WHERE platform_id = ?1 AND rel_path = ?2"
        ))?
        .query_row(params![platform_id.0, rel_path], from_row)
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
            [id],
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
        params![id, rel_path, state, mtime, now],
    )?;
    Ok(())
}

/// Deletes one file row.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn delete(conn: &Connection, id: FileId) -> Result<()> {
    conn.execute("DELETE FROM files WHERE id = ?1", [id])?;
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
    let rows = stmt.query_map(params![platform_id.0, after, to, sql::to_i64(limit)], |r| {
        r.get(0)
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Keeps a row whose file changed on disk but whose hashes still apply: only its
/// `mtime` and `scanned_at` move.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn restamp(conn: &Connection, id: FileId, mtime: i64, now: i64) -> Result<()> {
    conn.prepare_cached("UPDATE files SET mtime = ?2, scanned_at = ?3 WHERE id = ?1")?
        .execute(params![id, mtime, now])?;
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
    conn.prepare_cached(
        "UPDATE files SET size = ?2, mtime = ?3, crc32 = ?4, md5 = NULL, sha1 = NULL,
                          header_rule = NULL, crc32_whole = NULL, md5_whole = NULL,
                          sha1_whole = NULL, state = 'unverified', scanned_at = ?5
         WHERE id = ?1",
    )?
    .execute(params![id, size, mtime, crc32, now])?;
    Ok(())
}

/// Whether rom `rom_id` has a `verified` file.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn has_verified(conn: &Connection, rom_id: RomId) -> Result<bool> {
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
/// let rom = mistarr_server::db::ids::RomId(1);
/// assert!(!mistarr_server::db::files::mark_verified(&conn, &pid, "mame/a.zip#a.bin", rom).unwrap());
/// ```
pub fn mark_verified(
    conn: &Connection,
    platform_id: &PlatformId,
    rel_path: &str,
    rom_id: RomId,
) -> Result<bool> {
    Ok(conn
        .prepare_cached(
            "UPDATE files SET state = 'verified', rom_id = ?3
         WHERE platform_id = ?1 AND rel_path = ?2 AND state = 'unverified'",
        )?
        .execute(params![platform_id.0, rel_path, rom_id])?
        > 0)
}

/// Inserts or replaces the row `row` describes, keyed on `(platform_id, rel_path)`.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::files::{find_by_path, upsert, FileState, NewFile};
/// let conn = mistarr_server::db::fixtures::conn();
/// let psx = mistarr_core::PlatformId("psx".into());
/// let row = NewFile { rel_path: "PSX/g.chd".into(), size: 9, mtime: 1, crc32: None, md5: None,
///     sha1: None, header_rule: Some("chd".into()), whole: Default::default(), rom_id: None,
///     state: FileState::Unidentified, reason: Some("off".into()) };
/// upsert(&conn, &psx, &row, 5).unwrap();
/// let got = find_by_path(&conn, &psx, "PSX/g.chd").unwrap().unwrap();
/// assert_eq!(got.reason.as_deref(), Some("off"));
/// ```
pub fn upsert(
    conn: &Connection,
    platform_id: &PlatformId,
    row: &NewFile,
    now: i64,
) -> Result<FileId> {
    Ok(conn
        .prepare_cached(
            "INSERT INTO files (platform_id, rel_path, size, mtime, crc32, md5, sha1, header_rule,
                             rom_id, state, scanned_at, reason, crc32_whole, md5_whole, sha1_whole)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
         ON CONFLICT(platform_id, rel_path) DO UPDATE SET
           size = excluded.size, mtime = excluded.mtime, crc32 = excluded.crc32,
           md5 = excluded.md5, sha1 = excluded.sha1, header_rule = excluded.header_rule,
           rom_id = excluded.rom_id, state = excluded.state, scanned_at = excluded.scanned_at,
           reason = excluded.reason, crc32_whole = excluded.crc32_whole,
           md5_whole = excluded.md5_whole, sha1_whole = excluded.sha1_whole
         RETURNING id",
        )?
        .query_row(
            params![
                platform_id.0,
                row.rel_path,
                row.size,
                row.mtime,
                row.crc32,
                row.md5,
                row.sha1,
                row.header_rule,
                row.rom_id,
                row.state,
                now,
                row.reason,
                row.whole.crc32,
                row.whole.md5,
                row.whole.sha1,
            ],
            |r| r.get(0),
        )?)
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
    let list = sql::json_list(paths)?;
    let ids: Vec<FileId> = conn
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
/// use mistarr_server::db::ids::FileId;
/// assert_eq!(mistarr_server::db::files::delete_ids(&conn, &[FileId(1), FileId(2)]).unwrap(), 0);
/// ```
pub fn delete_ids(conn: &Connection, ids: &[FileId]) -> Result<usize> {
    if ids.is_empty() {
        return Ok(0);
    }
    let list = sql::json_list(ids)?;
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
        removed += crate::db::transact(conn, |tx| delete_paths(tx, platform_id, batch))?;
    }
    Ok(removed)
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
/// let page = mistarr_server::db::sql::Page { limit: 50, offset: 0 };
/// let got = mistarr_server::db::files::unidentified(&conn, &psx, page).unwrap();
/// assert!(got.items.is_empty() && got.total == 0);
/// ```
pub fn unidentified(
    conn: &Connection,
    platform_id: &PlatformId,
    page: Page,
) -> Result<Paged<UnidentifiedFile>> {
    sql::snapshot(conn, |conn| {
        let total = conn
            .prepare(
                "SELECT COUNT(*) FROM files WHERE state = 'unidentified' AND platform_id = ?1",
            )?
            .query_row([&platform_id.0], |r| sql::get_u64(r, 0))?;
        let items = conn
            .prepare(
                "SELECT rel_path, size, COALESCE(reason, 'corrupt') FROM files
                 WHERE state = 'unidentified' AND platform_id = ?1
                 ORDER BY rel_path LIMIT ?2 OFFSET ?3",
            )?
            .query_map(params![platform_id.0, page.limit, page.offset], |r| {
                Ok(UnidentifiedFile {
                    rel_path: r.get(0)?,
                    size: r.get(1)?,
                    reason: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Paged { items, total })
    })
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
/// use mistarr_server::db::files::unmatched_after;
/// use mistarr_server::db::ids::FileId;
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
        .query_map(params![platform_id.0, after, limit], from_row)?
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

/// Sets the matched rom and state of one file.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::files::{set_match, FileState};
/// use mistarr_server::db::ids::FileId;
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// set_match(&conn, FileId(1), None, FileState::Unverified).unwrap();
/// ```
pub fn set_match(
    conn: &Connection,
    id: FileId,
    rom_id: Option<RomId>,
    state: FileState,
) -> Result<()> {
    conn.prepare_cached("UPDATE files SET rom_id = ?2, state = ?3 WHERE id = ?1")?
        .execute(params![id, rom_id, state])?;
    Ok(())
}

/// Unmatches the files of rom `name` of title `title_id` when a DAT load is about to give
/// it another size or `[crc32, md5, sha1]`, so no file stays verified against hashes it
/// does not have: a fully hashed file becomes `unverified` for the recompute to match again
/// from its stored hashes, any other `pending` for the next scan to hash. A CHD's cue row is
/// left alone: it holds no hashes and follows its tracks. Returns the files changed.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let listed = [Some("00000000"), None, None];
/// let title = mistarr_server::db::ids::TitleId(1);
/// assert_eq!(mistarr_server::db::files::unmatch_changed_rom(&conn, title, "a.nes", 4, listed).unwrap(), 0);
/// ```
pub fn unmatch_changed_rom(
    conn: &Connection,
    title_id: TitleId,
    name: &str,
    size: i64,
    [crc32, md5, sha1]: [Option<&str>; 3],
) -> Result<usize> {
    Ok(conn
        .prepare_cached(
            "UPDATE files SET rom_id = NULL, state = CASE WHEN sha1 IS NOT NULL OR md5 IS NOT NULL
                                                  THEN 'unverified' ELSE 'pending' END
             WHERE rom_id = (SELECT id FROM roms WHERE title_id = ?1 AND name = ?2
                               AND (size IS NOT ?3 OR crc32 IS NOT ?4 OR md5 IS NOT ?5
                                    OR sha1 IS NOT ?6))
               AND NOT (header_rule IS 'chd' AND sha1 IS NULL AND md5 IS NULL)",
        )?
        .execute(params![title_id, name, size, crc32, md5, sha1])?)
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
    let rows = stmt.query_map([&platform_id.0], |r| Ok((r.get(0)?, sql::get_u64(r, 1)?)))?;
    for row in rows {
        let (state, n) = row?;
        match state {
            FileState::Verified => counts.verified = n,
            FileState::Misnamed => counts.misnamed = n,
            FileState::Unverified => counts.unverified = n,
            FileState::Bad => counts.bad = n,
            FileState::Unidentified => counts.unidentified = n,
            FileState::Pending => counts.pending = n,
        }
    }
    Ok(counts)
}

/// A `misnamed` file with the name of the rom it matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MisnamedRow {
    /// The file.
    pub id: FileId,
    /// Its platform.
    pub platform_id: PlatformId,
    /// Relative to `games/`, a zip member as `a.zip#b.nes`.
    pub rel_path: String,
    /// The matched `roms.id`.
    pub rom_id: RomId,
    /// The rom's name in its DAT.
    pub rom_name: String,
    /// The name of the rom's title.
    pub game: String,
}

/// Every `misnamed` file with a rom, in id order, so the name rule can be applied
/// again without hashing.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert!(mistarr_server::db::files::misnamed(&conn).unwrap().is_empty());
/// ```
pub fn misnamed(conn: &Connection) -> Result<Vec<MisnamedRow>> {
    let mut stmt = conn.prepare(
        "SELECT f.id, f.platform_id, f.rel_path, r.id, r.name, t.name
         FROM files f JOIN roms r ON r.id = f.rom_id JOIN titles t ON t.id = r.title_id
         WHERE f.state = 'misnamed' ORDER BY f.id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(MisnamedRow {
            id: r.get(0)?,
            platform_id: PlatformId(r.get(1)?),
            rel_path: r.get(2)?,
            rom_id: r.get(3)?,
            rom_name: r.get(4)?,
            game: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::fixtures::{conn, dat};
    use crate::db::titles::RomStatus;
    use mistarr_core::HashSet;

    fn hashes(size: u64) -> HashSet {
        HashSet {
            size,
            crc32: "352441c2".into(),
            md5: "900150983cd24fb0d6963f7d28e17f72".into(),
            sha1: "a9993e364706816aba3e25717850c26c9cd0d89d".into(),
        }
    }

    #[test]
    fn loadable_matches_its_sql() {
        let text: Vec<&str> = FileState::LOADABLE.iter().map(|s| s.as_str()).collect();
        assert_eq!(FileState::LOADABLE_SQL, crate::db::sql::text_list(&text));
    }

    #[test]
    fn upsert_and_find_round_trip() {
        let c = conn();
        let pid = PlatformId("nes".into());
        let hashed = NewFile {
            crc32: Some("352441c2".into()),
            header_rule: Some("ines".into()),
            ..NewFile::unhashed("a.nes", 3, 10, FileState::Unverified)
        };
        let id = upsert(&c, &pid, &hashed, 20).expect("insert");
        let row = find_by_path(&c, &pid, "a.nes").expect("find").expect("row");
        assert_eq!(row.id, id);
        assert_eq!(row.state, FileState::Unverified);
        assert_eq!(row.crc32.as_deref(), Some("352441c2"));

        let again = NewFile {
            mtime: 11,
            state: FileState::Verified,
            ..hashed
        };
        let same_id = upsert(&c, &pid, &again, 21).expect("update");
        assert_eq!(same_id, id);
        let row = find_by_path(&c, &pid, "a.nes").expect("find").expect("row");
        assert_eq!(row.mtime, 11);
        assert_eq!(row.state, FileState::Verified);
    }

    #[test]
    fn delete_missing_removes_only_absent_paths() {
        let mut c = conn();
        let pid = PlatformId("nes".into());
        upsert(
            &c,
            &pid,
            &NewFile::unhashed("keep.nes", 1, 1, FileState::Unverified),
            1,
        )
        .expect("insert");
        upsert(
            &c,
            &pid,
            &NewFile::unhashed("gone.nes", 1, 1, FileState::Unverified),
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
        let id = upsert(
            &c,
            &pid,
            &NewFile::unhashed("gone.nes", 1, 1, FileState::Verified),
            1,
        )
        .expect("insert");
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
        for rel in [
            "mame/a.zip",
            "mame/a.zip#x.bin",
            "mame/a.zip#y.bin",
            "mame/a.zip.zip#z.bin",
            "mame/b.zip#x.bin",
            "mame0.txt",
            "hbmame/c.zip",
        ] {
            upsert(
                &c,
                &pid,
                &NewFile::unhashed(rel, 1, 1, FileState::Unverified),
                1,
            )
            .expect("insert");
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
            upsert(
                &c,
                &psx,
                &NewFile::unhashed(rel, 1, 1, FileState::Unverified),
                1,
            )
            .expect("insert");
        }
        let other = PlatformId("saturn".into());
        upsert(
            &c,
            &other,
            &NewFile::unhashed("PSX/Disc/x.bin", 1, 1, FileState::Unverified),
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
        for rel in [
            "mame/Foo.zip",
            "mame/Foo.zip#a.bin",
            "mame/FOO.ZIP#b.bin",
            "mame/foo2.zip",
        ] {
            upsert(
                &c,
                &pid,
                &NewFile::unhashed(rel, 1, 1, FileState::Unverified),
                1,
            )
            .expect("insert");
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
        let rom = dat(&pid)
            .title("exampleset")
            .rom("a", &hashes(1), RomStatus::Good)
            .write(&c)
            .expect("rom")
            .first_rom();
        let row = |rel: &str| NewFile {
            rom_id: Some(rom),
            crc32: Some("0000abcd".into()),
            md5: Some("m".into()),
            sha1: Some("s".into()),
            header_rule: Some("none".into()),
            ..NewFile::unhashed(rel, 1, 1, FileState::Verified)
        };
        let a = upsert(&c, &pid, &row("mame/a.zip#a"), 1).expect("insert");
        let b = upsert(&c, &pid, &row("mame/a.zip#b"), 1).expect("insert");
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
        let gone = upsert(
            &c,
            &pid,
            &NewFile::unhashed("a.nes", 1, 1, FileState::Verified),
            1,
        )
        .expect("a");
        upsert(
            &c,
            &pid,
            &NewFile::unhashed("b.nes", 1, 1, FileState::Verified),
            1,
        )
        .expect("b");
        let kept = upsert(
            &c,
            &pid,
            &NewFile::unhashed("c.nes", 1, 1, FileState::Verified),
            1,
        )
        .expect("c");
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
        for rel in ["NES/a.nes", "NES/sub/b.nes", "NESX/c.nes", "d.nes"] {
            upsert(
                &c,
                &pid,
                &NewFile::unhashed(rel, 1, 1, FileState::Verified),
                1,
            )
            .expect("insert");
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
        let n = DELETE_BATCH * 2 + 7;
        for i in 0..n {
            upsert(
                &c,
                &pid,
                &NewFile::unhashed(&format!("g{i}.nes"), 1, 1, FileState::Unverified),
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
        for i in 0..50 {
            upsert(
                &c,
                &pid,
                &NewFile::unhashed(&format!("game{i}.nes"), 1, 1, FileState::Unverified),
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
    fn state_counts_group_by_state() {
        let c = conn();
        let pid = PlatformId("nes".into());
        upsert(
            &c,
            &pid,
            &NewFile::unhashed("a.nes", 1, 1, FileState::Verified),
            1,
        )
        .expect("insert");
        upsert(
            &c,
            &pid,
            &NewFile::unhashed("b.nes", 1, 1, FileState::Verified),
            1,
        )
        .expect("insert");
        upsert(
            &c,
            &pid,
            &NewFile::unhashed("c.nes", 1, 1, FileState::Unverified),
            1,
        )
        .expect("insert");
        let counts = state_counts(&c, &pid).expect("counts");
        assert_eq!(counts.verified, 2);
        assert_eq!(counts.unverified, 1);
        assert_eq!(counts.bad, 0);
    }

    #[test]
    fn get_and_move_to_follow_a_rename() {
        let c = conn();
        let pid = PlatformId("nes".into());
        let id = upsert(
            &c,
            &pid,
            &NewFile::unhashed("NES/a.nes", 1, 1, FileState::Misnamed),
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
        let rom = crate::db::fixtures::dat(&pid)
            .title("Example Set")
            .rom("a.rom", &hashes(3), RomStatus::Good)
            .write(&c)
            .expect("rom")
            .first_rom();
        assert!(!has_verified(&c, rom).expect("none"));
        let a = upsert(
            &c,
            &pid,
            &NewFile {
                rom_id: Some(rom),
                ..NewFile::unhashed("NeoGeo/set.zip#a.rom", 1, 1, FileState::Verified)
            },
            1,
        )
        .expect("a");
        upsert(
            &c,
            &pid,
            &NewFile::unhashed("NeoGeo/set.zip#b.rom", 1, 1, FileState::Unverified),
            1,
        )
        .expect("b");
        upsert(
            &c,
            &pid,
            &NewFile::unhashed("NeoGeo/set.zip2#c.rom", 1, 1, FileState::Unverified),
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
    fn a_rom_given_other_hashes_unmatches_its_files() {
        let c = conn();
        let pid = PlatformId("nes".into());
        let h = hashes(3);
        let seeded = dat(&pid)
            .title("Moved Quest")
            .rom("Moved Quest.nes", &h, RomStatus::Good)
            .write(&c)
            .expect("rom");
        let (title, rom) = (seeded.titles[0], seeded.first_rom());
        let verified = FileState::Verified;
        let a = upsert(
            &c,
            &pid,
            &NewFile {
                rom_id: Some(rom),
                crc32: Some(h.crc32.clone()),
                md5: Some(h.md5.clone()),
                sha1: Some(h.sha1.clone()),
                header_rule: Some("ines".to_string()),
                ..NewFile::unhashed("NES/a.nes", 3, 1, verified)
            },
            1,
        )
        .expect("a");
        let b = upsert(
            &c,
            &pid,
            &NewFile {
                rom_id: Some(rom),
                crc32: Some(h.crc32.clone()),
                ..NewFile::unhashed("NES/b.nes", 3, 1, verified)
            },
            1,
        )
        .expect("b");
        let cue_row = upsert(
            &c,
            &pid,
            &NewFile {
                rom_id: Some(rom),
                header_rule: Some("chd".to_string()),
                ..NewFile::unhashed("NES/d.chd#cue", 3, 1, verified)
            },
            1,
        )
        .expect("d");
        let same = [
            Some(h.crc32.as_str()),
            Some(h.md5.as_str()),
            Some(h.sha1.as_str()),
        ];
        let unmatch =
            |size, listed| unmatch_changed_rom(&c, title, "Moved Quest.nes", size, listed);
        assert_eq!(
            unmatch(3, same).expect("same"),
            0,
            "unchanged hashes keep the match"
        );
        assert_eq!(unmatch(4, same).expect("size"), 2);
        let state = |id| get(&c, id).expect("get").map(|f| (f.rom_id, f.state));
        assert_eq!(
            state(cue_row),
            Some((Some(rom), verified)),
            "a CHD cue row follows its tracks"
        );
        assert_eq!(state(a), Some((None, FileState::Unverified)));
        assert_eq!(
            state(b),
            Some((None, FileState::Pending)),
            "hashed again by a scan"
        );
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
                whole: WholeHashes::default(),
                rom_id: None,
                state: FileState::Unidentified,
                reason: Some(reason.into()),
            };
            upsert(&c, &pid, &row, 1).expect("upsert");
        }
        let row = find_by_path(&c, &pid, "PSX/A/a.chd")
            .expect("find")
            .expect("row");
        assert_eq!(
            (row.state, row.reason.as_deref()),
            (FileState::Unidentified, Some("cooked"))
        );
        upsert(
            &c,
            &pid,
            &NewFile::unhashed("PSX/A/a.chd", 5, 1, FileState::Unverified),
            2,
        )
        .expect("upsert");
        let row = find_by_path(&c, &pid, "PSX/A/a.chd")
            .expect("find")
            .expect("row");
        assert_eq!(row.reason, None, "a plain upsert clears the reason");

        let first = unidentified(
            &c,
            &pid,
            Page {
                limit: 1,
                offset: 0,
            },
        )
        .expect("page");
        assert_eq!(first.total, 2);
        assert_eq!(first.items[0].rel_path, "PSX/B/b.chd");
        assert_eq!(first.items[0].reason, "off");
        let rest = unidentified(
            &c,
            &pid,
            Page {
                limit: 5,
                offset: 1,
            },
        )
        .expect("page");
        assert_eq!(rest.items.len(), 1);
        assert_eq!(rest.items[0].rel_path, "PSX/C/c.chd");

        assert_eq!(state_counts(&c, &pid).expect("counts").unidentified, 2);
    }

    #[test]
    fn delete_ids_clears_the_import_log_first() {
        let c = conn();
        let pid = PlatformId("psx".into());
        let id = upsert(
            &c,
            &pid,
            &NewFile::unhashed("PSX/G/g.chd", 1, 1, FileState::Unverified),
            1,
        )
        .expect("upsert");
        c.execute(
            "INSERT INTO import_log (at, file_id, action, detail) VALUES (0, ?1, 'placed', '{}')",
            [id.0],
        )
        .expect("log");
        c.execute_batch("PRAGMA foreign_keys = ON").expect("fk");
        assert_eq!(delete_ids(&c, &[id, FileId(999)]).expect("delete"), 1);
        let logged: Option<i64> = c
            .query_row("SELECT file_id FROM import_log", [], |r| r.get(0))
            .expect("log");
        assert_eq!(logged, None);
    }
}
