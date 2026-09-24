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
    /// Size in bytes after any header rule was applied.
    pub size: i64,
    /// Filesystem mtime, Unix seconds.
    pub mtime: i64,
    /// CRC32 as lowercase hex, when hashed.
    pub crc32: Option<String>,
    /// MD5 as lowercase hex, when fully hashed.
    pub md5: Option<String>,
    /// SHA1 as lowercase hex, when fully hashed.
    pub sha1: Option<String>,
    /// Header rule name applied while hashing.
    pub header_rule: Option<String>,
    /// The matched `roms.id`, when any.
    pub rom_id: Option<i64>,
    /// Lifecycle state.
    pub state: FileState,
    /// Unix seconds this row was last written by a scan.
    pub scanned_at: i64,
}

/// A file found on disk and hashed, ready to be matched and written by the
/// scan job. `rom_id` and `state` are already decided.
#[derive(Debug, Clone)]
pub struct NewFile {
    /// Relative to `games/`, including the top directory name; a zip member
    /// is `a.zip#b.nes`.
    pub rel_path: String,
    /// Size in bytes after any header rule was applied.
    pub size: i64,
    /// Filesystem mtime, Unix seconds.
    pub mtime: i64,
    /// CRC32 as lowercase hex.
    pub crc32: Option<String>,
    /// MD5 as lowercase hex, when fully hashed.
    pub md5: Option<String>,
    /// SHA1 as lowercase hex, when fully hashed.
    pub sha1: Option<String>,
    /// Header rule name applied, when fully hashed.
    pub header_rule: Option<String>,
    /// The matched `roms.id`, when any.
    pub rom_id: Option<i64>,
    /// The decided state.
    pub state: FileState,
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

const COLUMNS: &str =
    "id, platform_id, rel_path, size, mtime, crc32, md5, sha1, header_rule, rom_id, state, scanned_at";

fn from_row(r: &Row<'_>) -> rusqlite::Result<FileRow> {
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

/// The member rows the scanner keeps for zip `zip_rel`, stored as `zip_rel#member`.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn zip_member_rows(
    conn: &Connection,
    platform_id: &PlatformId,
    zip_rel: &str,
) -> Result<Vec<FileRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM files WHERE platform_id = ?1 AND substr(rel_path, 1, ?2) = ?3
         ORDER BY rel_path"
    ))?;
    let prefix = format!("{zip_rel}#");
    let len = i64::try_from(prefix.chars().count()).unwrap_or(i64::MAX);
    let rows = stmt.query_map(params![platform_id.0, len, prefix], from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
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
    /// The header rule name applied, when the payload was fully hashed.
    pub header_rule: Option<&'a str>,
}

/// Inserts or replaces a file row keyed on `(platform_id, rel_path)`.
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
    conn.execute(
        "INSERT INTO files (platform_id, rel_path, size, mtime, crc32, md5, sha1, header_rule,
                             rom_id, state, scanned_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(platform_id, rel_path) DO UPDATE SET
           size = excluded.size, mtime = excluded.mtime, crc32 = excluded.crc32,
           md5 = excluded.md5, sha1 = excluded.sha1, header_rule = excluded.header_rule,
           rom_id = excluded.rom_id, state = excluded.state, scanned_at = excluded.scanned_at",
        params![
            platform_id.0,
            rel_path,
            size,
            mtime,
            hashed.crc32,
            hashed.md5,
            hashed.sha1,
            hashed.header_rule,
            rom_id,
            state.as_str(),
            now,
        ],
    )?;
    Ok(conn
        .query_row(
            "SELECT id FROM files WHERE platform_id = ?1 AND rel_path = ?2",
            params![platform_id.0, rel_path],
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

/// Deletes every row of `platform_id` whose `rel_path` is not in `keep`.
/// Returns the number of rows removed.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn delete_missing(
    conn: &Connection,
    platform_id: &PlatformId,
    keep: &[String],
) -> Result<usize> {
    let existing = existing_paths(conn, platform_id)?;
    let keep: std::collections::HashSet<&str> = keep.iter().map(String::as_str).collect();
    let mut removed = 0;
    let mut stmt = conn.prepare("DELETE FROM files WHERE platform_id = ?1 AND rel_path = ?2")?;
    for path in existing.iter().filter(|p| !keep.contains(p.as_str())) {
        removed += stmt.execute(params![platform_id.0, path])?;
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
    const SELECT: &str = "SELECT r.id, r.title_id, r.name, r.status
         FROM roms r JOIN titles t ON t.id = r.title_id
         JOIN dat_versions d ON d.id = t.dat_version_id
         WHERE t.platform_id = ?1 AND t.source = 'dat' AND ";
    const ORDER: &str = " ORDER BY (r.retired = 0 AND t.retired = 0) DESC,
         d.superseded_by IS NULL DESC, d.id DESC LIMIT 1";
    let row = |r: &Row<'_>| -> rusqlite::Result<RomMatch> {
        Ok(RomMatch {
            rom_id: r.get(0)?,
            title_id: r.get(1)?,
            name: r.get(2)?,
            status: r.get(3)?,
        })
    };
    if let Some(m) = conn
        .query_row(
            &format!("{SELECT}r.sha1 = ?2{ORDER}"),
            params![platform_id.0, sha1],
            row,
        )
        .optional()?
    {
        return Ok(Some(m));
    }
    if let Some(m) = conn
        .query_row(
            &format!("{SELECT}r.sha1 IS NULL AND r.md5 = ?2{ORDER}"),
            params![platform_id.0, md5],
            row,
        )
        .optional()?
    {
        return Ok(Some(m));
    }
    Ok(conn
        .query_row(
            &format!(
                "{SELECT}r.sha1 IS NULL AND r.md5 IS NULL AND r.crc32 = ?2 AND r.size = ?3{ORDER}"
            ),
            params![platform_id.0, crc32, size],
            row,
        )
        .optional()?)
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

/// Verified/misnamed/unverified/bad counts for one platform, for the
/// platforms card.
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
        let c = conn();
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
        let removed = delete_missing(&c, &pid, &["keep.nes".to_owned()]).expect("delete");
        assert_eq!(removed, 1);
        assert!(find_by_path(&c, &pid, "keep.nes").expect("find").is_some());
        assert!(find_by_path(&c, &pid, "gone.nes").expect("find").is_none());
    }

    /// `keep` is deduplicated through a set rather than scanned per row, so a
    /// repeated entry does not change the count removed.
    #[test]
    fn delete_missing_keep_lookup_is_set_based() {
        let c = conn();
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
        let removed = delete_missing(&c, &pid, &keep).expect("delete");
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
}
