//! The `import_log` table and the DAT entry reads placement needs; see
//! `docs/DATA-MODEL.md` and `docs/ARCHITECTURE.md` "Import".

use mistarr_core::PlatformId;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::Serialize;
use serde_json::Value;

use super::titles::TitleId;
use crate::error::Result;

/// `import_log.action`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ImportAction {
    /// Moved into `games/` where nothing was.
    Placed,
    /// Moved into `games/` over a file that was not verified.
    Replaced,
    /// Hash mismatch; moved to `staging/quarantine/`.
    Quarantined,
    /// A verified copy was already in place; the new file was discarded.
    SkippedExisting,
    /// A misnamed file was given its canonical name in place.
    Renamed,
}

impl ImportAction {
    /// The column value.
    ///
    /// ```
    /// use mistarr_server::db::imports::ImportAction;
    /// assert_eq!(ImportAction::SkippedExisting.as_str(), "skipped_existing");
    /// ```
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Placed => "placed",
            Self::Replaced => "replaced",
            Self::Quarantined => "quarantined",
            Self::SkippedExisting => "skipped_existing",
            Self::Renamed => "renamed",
        }
    }
}

/// One `import_log` row as `GET /imports` lists it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LogRow {
    /// Row id.
    pub id: i64,
    /// Unix seconds.
    pub at: i64,
    /// The download imported, when the entry came from one.
    pub download_id: Option<i64>,
    /// The file placed, kept or renamed.
    pub file_id: Option<i64>,
    /// What happened.
    pub action: String,
    /// Action-specific JSON.
    pub detail: Value,
}

/// Appends a log entry and returns its id.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn log(
    conn: &Connection,
    at: i64,
    download_id: Option<i64>,
    file_id: Option<i64>,
    action: ImportAction,
    detail: &Value,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO import_log (at, download_id, file_id, action, detail) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![at, download_id, file_id, action.as_str(), detail.to_string()],
    )?;
    Ok(conn.last_insert_rowid())
}

/// A page of the log, newest first, and the total row count.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn list(conn: &Connection, limit: u32, offset: u32) -> Result<(Vec<LogRow>, u64)> {
    let total: i64 = conn.query_row("SELECT COUNT(*) FROM import_log", [], |r| r.get(0))?;
    let mut stmt = conn.prepare(
        "SELECT id, at, download_id, file_id, action, detail FROM import_log
         ORDER BY id DESC LIMIT ?1 OFFSET ?2",
    )?;
    let rows = stmt.query_map([limit, offset], |r| {
        let detail: String = r.get(5)?;
        Ok(LogRow {
            id: r.get(0)?,
            at: r.get(1)?,
            download_id: r.get(2)?,
            file_id: r.get(3)?,
            action: r.get(4)?,
            detail: serde_json::from_str(&detail).unwrap_or(Value::Null),
        })
    })?;
    let items = rows.collect::<rusqlite::Result<_>>()?;
    Ok((items, u64::try_from(total).unwrap_or(0)))
}

/// A rom with everything placement and the quarantine report need.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EntryRom {
    /// `roms.id`.
    pub id: i64,
    /// File name in the DAT.
    pub name: String,
    /// Size in bytes.
    pub size: u64,
    /// Lowercase hex CRC32.
    pub crc32: Option<String>,
    /// Lowercase hex MD5.
    pub md5: Option<String>,
    /// Lowercase hex SHA1.
    pub sha1: Option<String>,
    /// DAT status.
    pub status: String,
    /// The DAT's `header` attribute, verbatim.
    pub header: Option<String>,
}

/// A DAT game with its live roms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitleEntry {
    /// `titles.id`.
    pub id: TitleId,
    /// Owning platform.
    pub platform_id: PlatformId,
    /// Full DAT name.
    pub name: String,
    /// Flag labels, `bios` among them for BIOS entries.
    pub flags: Vec<String>,
    /// Live roms in id order.
    pub roms: Vec<EntryRom>,
    /// Read from an MRA file rather than a DAT.
    pub from_mra: bool,
}

impl TitleEntry {
    /// Whether the DAT flags this entry as a BIOS.
    #[must_use]
    pub fn is_bios(&self) -> bool {
        self.flags.iter().any(|f| f == "bios")
    }
}

const ROM_COLUMNS: &str = "id, name, size, crc32, md5, sha1, status, header";

fn rom_row(r: &Row<'_>) -> rusqlite::Result<EntryRom> {
    Ok(EntryRom {
        id: r.get(0)?,
        name: r.get(1)?,
        size: u64::try_from(r.get::<_, i64>(2)?).unwrap_or(0),
        crc32: r.get(3)?,
        md5: r.get(4)?,
        sha1: r.get(5)?,
        status: r.get(6)?,
        header: r.get(7)?,
    })
}

/// One rom, live or retired.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn rom(conn: &Connection, id: i64) -> Result<Option<EntryRom>> {
    Ok(conn
        .query_row(
            &format!("SELECT {ROM_COLUMNS} FROM roms WHERE id = ?1"),
            [id],
            rom_row,
        )
        .optional()?)
}

/// The title owning rom `rom_id`.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn title_of_rom(conn: &Connection, rom_id: i64) -> Result<Option<TitleId>> {
    Ok(conn
        .query_row("SELECT title_id FROM roms WHERE id = ?1", [rom_id], |r| {
            r.get(0).map(TitleId)
        })
        .optional()?)
}

/// A title and its live roms, or `None` when there is no such title.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn title_entry(conn: &Connection, id: TitleId) -> Result<Option<TitleEntry>> {
    let Some((platform, name, source)): Option<(String, String, String)> = conn
        .query_row(
            "SELECT platform_id, name, source FROM titles WHERE id = ?1",
            [id.0],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
    else {
        return Ok(None);
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT {ROM_COLUMNS} FROM roms WHERE title_id = ?1 AND retired = 0 ORDER BY id"
    ))?;
    let roms = stmt
        .query_map([id.0], rom_row)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(Some(TitleEntry {
        id,
        platform_id: PlatformId(platform),
        name,
        flags: super::titles::flags_of(conn, id)?,
        roms,
        from_mra: source == "mra",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn conn() -> Connection {
        let mut c = Connection::open_in_memory().expect("open");
        crate::db::migrate::apply(&mut c).expect("migrate");
        crate::db::platforms::seed(&mut c, &mistarr_mister::platforms::PLATFORMS).expect("seed");
        c
    }

    #[test]
    fn log_lists_newest_first_with_paging() {
        let c = conn();
        for (i, action) in [
            ImportAction::Placed,
            ImportAction::Replaced,
            ImportAction::Renamed,
        ]
        .into_iter()
        .enumerate()
        {
            log(&c, 10, None, None, action, &json!({ "n": i })).expect("log");
        }
        let (items, total) = list(&c, 2, 0).expect("list");
        assert_eq!(total, 3);
        assert_eq!(items[0].action, "renamed");
        assert_eq!(items[0].detail, json!({ "n": 2 }));
        let (rest, _) = list(&c, 2, 2).expect("list");
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].action, "placed");
        assert_eq!(ImportAction::Quarantined.as_str(), "quarantined");
    }

    #[test]
    fn title_entry_carries_flags_live_roms_and_headers() {
        let c = conn();
        let rom_id = crate::db::sources::fixtures::seed_rom(
            &c,
            "nes",
            "Example Quest (USA).nes",
            8,
            &["bios"],
        )
        .expect("seed");
        c.execute(
            "UPDATE roms SET header = '4E 45 53 1A' WHERE id = ?1",
            [rom_id],
        )
        .expect("header");
        let title = title_of_rom(&c, rom_id).expect("title").expect("some");
        let entry = title_entry(&c, title).expect("entry").expect("some");
        assert!(entry.is_bios());
        assert_eq!(entry.name, "Example Quest (USA)");
        assert_eq!(entry.roms[0].header.as_deref(), Some("4E 45 53 1A"));
        assert_eq!(rom(&c, rom_id).expect("rom").map(|r| r.size), Some(8));
        c.execute("UPDATE roms SET retired = 1 WHERE id = ?1", [rom_id])
            .expect("retire");
        assert!(title_entry(&c, title)
            .expect("entry")
            .expect("some")
            .roms
            .is_empty());
        assert!(title_entry(&c, TitleId(99)).expect("entry").is_none());
        assert!(title_of_rom(&c, 99).expect("none").is_none());
    }
}
