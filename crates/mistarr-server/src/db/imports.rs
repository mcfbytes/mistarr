//! The `import_log` table and the DAT entry reads placement needs; see
//! `docs/DATA-MODEL.md` and `docs/ARCHITECTURE.md` "Import".

use mistarr_core::PlatformId;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::Value;

use super::ids::{DownloadId, FileId, ImportId, TitleId};
use super::roms::{rom_row, EntryRom, ROM_COLUMNS};
use super::sql::{self, Page, Paged};
use super::titles::TitleSource;
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
    pub id: ImportId,
    /// Unix seconds.
    pub at: i64,
    /// The download imported, when the entry came from one.
    pub download_id: Option<DownloadId>,
    /// The file placed, kept or renamed.
    pub file_id: Option<FileId>,
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
    download_id: Option<DownloadId>,
    file_id: Option<FileId>,
    action: ImportAction,
    detail: &Value,
) -> Result<ImportId> {
    conn.execute(
        "INSERT INTO import_log (at, download_id, file_id, action, detail) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![at, download_id, file_id, action.as_str(), detail.to_string()],
    )?;
    Ok(ImportId(conn.last_insert_rowid()))
}

/// A page of the log, newest first, and the total row count.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure, [`crate::Error::Stored`] for a detail that is
/// not JSON.
pub fn list(conn: &Connection, page: Page) -> Result<Paged<LogRow>> {
    sql::snapshot(conn, |conn| {
        let total = conn.query_row("SELECT COUNT(*) FROM import_log", [], |r| {
            sql::get_u64(r, 0)
        })?;
        let mut stmt = conn.prepare(
            "SELECT id, at, download_id, file_id, action, detail FROM import_log
             ORDER BY id DESC LIMIT ?1 OFFSET ?2",
        )?;
        let items = stmt
            .query_map([page.limit, page.offset], |r| {
                Ok(LogRow {
                    id: r.get(0)?,
                    at: r.get(1)?,
                    download_id: r.get(2)?,
                    file_id: r.get(3)?,
                    action: r.get(4)?,
                    detail: sql::get_json(r, 5, "import_log.detail")?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Paged { items, total })
    })
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

/// A title and its live roms, or `None` when there is no such title.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn title_entry(conn: &Connection, id: TitleId) -> Result<Option<TitleEntry>> {
    let Some((platform, name, source)): Option<(String, String, TitleSource)> = conn
        .query_row(
            "SELECT platform_id, name, source FROM titles WHERE id = ?1",
            [id],
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
        .query_map([id], rom_row)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(Some(TitleEntry {
        id,
        platform_id: PlatformId(platform),
        name,
        flags: super::titles::flags_of(conn, id)?,
        roms,
        from_mra: source == TitleSource::Mra,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::fixtures::conn;
    use crate::db::ids::RomId;
    use crate::db::roms;
    use serde_json::json;

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
        let first = list(
            &c,
            Page {
                limit: 2,
                offset: 0,
            },
        )
        .expect("list");
        assert_eq!(first.total, 3);
        assert_eq!(first.items[0].action, "renamed");
        assert_eq!(first.items[0].detail, json!({ "n": 2 }));
        let rest = list(
            &c,
            Page {
                limit: 2,
                offset: 2,
            },
        )
        .expect("list");
        assert_eq!(rest.items.len(), 1);
        assert_eq!(rest.items[0].action, "placed");
        assert_eq!(ImportAction::Quarantined.as_str(), "quarantined");
        c.execute("UPDATE import_log SET detail = 'not json'", [])
            .expect("corrupt");
        assert!(matches!(
            list(
                &c,
                Page {
                    limit: 2,
                    offset: 0
                }
            ),
            Err(crate::Error::Stored { .. })
        ));
    }

    #[test]
    fn title_entry_carries_flags_live_roms_and_headers() {
        let c = conn();
        let rom_id =
            crate::db::fixtures::seed_rom(&c, "nes", "Example Quest (USA).nes", 8, &["bios"])
                .expect("seed");
        c.execute(
            "UPDATE roms SET header = '4E 45 53 1A' WHERE id = ?1",
            [rom_id],
        )
        .expect("header");
        let title = roms::title_of_rom(&c, rom_id)
            .expect("title")
            .expect("some");
        let entry = title_entry(&c, title).expect("entry").expect("some");
        assert!(entry.is_bios());
        assert_eq!(entry.name, "Example Quest (USA)");
        assert_eq!(entry.roms[0].header.as_deref(), Some("4E 45 53 1A"));
        assert_eq!(roms::rom(&c, rom_id).expect("rom").map(|r| r.size), Some(8));
        c.execute("UPDATE roms SET retired = 1 WHERE id = ?1", [rom_id])
            .expect("retire");
        assert!(title_entry(&c, title)
            .expect("entry")
            .expect("some")
            .roms
            .is_empty());
        assert!(title_entry(&c, TitleId(99)).expect("entry").is_none());
        assert!(roms::title_of_rom(&c, RomId(99)).expect("none").is_none());
    }
}
