//! The `downloads` reads and transitions the importer needs; see
//! `docs/DATA-MODEL.md` "downloads.state" and `docs/ARCHITECTURE.md` "Import".

use std::fmt;

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::Serialize;

use super::sources::SourceId;
use super::titles::TitleId;
use crate::error::Result;

/// A `downloads.id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct DownloadId(pub i64);

impl fmt::Display for DownloadId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// The `downloads.state` values the importer reads or writes.
pub mod state {
    /// Handed to the importer: the file is complete and checked by the client.
    pub const IMPORTING: &str = "importing";
    /// Placed, or kept an existing verified copy.
    pub const DONE: &str = "done";
    /// Hash mismatch; the file is quarantined.
    pub const BAD: &str = "bad";
    /// Refused or not placeable; the file stays in staging.
    pub const FAILED: &str = "failed";
    /// Cancelled by the user.
    pub const CANCELLED: &str = "cancelled";
    /// States a download passes through before it reaches the importer.
    pub const IN_FLIGHT: [&str; 4] = ["wanted", "queued", "transferring", "checking"];
}

/// One download as the importer sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportRow {
    /// Row id.
    pub id: DownloadId,
    /// The wanted title.
    pub title_id: TitleId,
    /// The rom the transfer was chosen for; hashing may reassign it.
    pub rom_id: i64,
    /// The torrent; `None` while `wanted`.
    pub source_id: Option<SourceId>,
    /// File index in the torrent; `None` while `wanted`.
    pub file_index: Option<u32>,
    /// Current state.
    pub state: String,
    /// Local path of the finished file, set on `importing`.
    pub staged_path: Option<String>,
}

const COLUMNS: &str = "id, title_id, rom_id, source_id, file_index, state, staged_path";

fn from_row(r: &Row<'_>) -> rusqlite::Result<ImportRow> {
    Ok(ImportRow {
        id: DownloadId(r.get(0)?),
        title_id: TitleId(r.get(1)?),
        rom_id: r.get(2)?,
        source_id: r.get::<_, Option<i64>>(3)?.map(SourceId),
        file_index: r
            .get::<_, Option<i64>>(4)?
            .and_then(|i| u32::try_from(i).ok()),
        state: r.get(5)?,
        staged_path: r.get(6)?,
    })
}

/// Reads one download.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn get(conn: &Connection, id: DownloadId) -> Result<Option<ImportRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM downloads WHERE id = ?1"),
            [id.0],
            from_row,
        )
        .optional()?)
}

/// Every download waiting for the importer, oldest first.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn importing(conn: &Connection) -> Result<Vec<DownloadId>> {
    let mut stmt = conn.prepare("SELECT id FROM downloads WHERE state = ?1 ORDER BY id")?;
    let rows = stmt.query_map([state::IMPORTING], |r| r.get(0).map(DownloadId))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Every download of a title, oldest first.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn for_title(conn: &Connection, title_id: TitleId) -> Result<Vec<ImportRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM downloads WHERE title_id = ?1 ORDER BY id"
    ))?;
    let rows = stmt.query_map([title_id.0], from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Moves a download out of `importing` to `to`, with `error` as its reason.
/// Returns false when the row was no longer `importing`.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn finish(
    conn: &Connection,
    id: DownloadId,
    to: &str,
    error: Option<&str>,
    now: i64,
) -> Result<bool> {
    let n = conn.execute(
        "UPDATE downloads SET state = ?2, error = ?3, progress = 1, updated_at = ?4
         WHERE id = ?1 AND state = ?5",
        params![id.0, to, error, now, state::IMPORTING],
    )?;
    Ok(n > 0)
}

/// Whether a source has a `done` download and none still selected in the
/// client: queued, transferring, checking or importing.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn source_settled(conn: &Connection, source_id: SourceId) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM downloads WHERE source_id = ?1 AND state = ?2)
            AND NOT EXISTS(SELECT 1 FROM downloads WHERE source_id = ?1
                           AND state IN ('queued', 'transferring', 'checking', ?3))",
        params![source_id.0, state::DONE, state::IMPORTING],
        |r| r.get(0),
    )?)
}

/// The path of file `index` inside the torrent of `source`.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn torrent_path(conn: &Connection, source_id: SourceId, index: u32) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT path FROM torrent_files WHERE source_id = ?1 AND file_index = ?2",
            params![source_id.0, index],
            |r| r.get(0),
        )
        .optional()?)
}

/// Inserts a download row directly, standing in for the transfer poller in tests.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
#[cfg(any(test, feature = "test-support"))]
pub fn insert_fixture(
    conn: &Connection,
    rom_id: i64,
    source_id: SourceId,
    file_index: u32,
    state: &str,
    staged_path: Option<&str>,
) -> Result<DownloadId> {
    conn.execute(
        "INSERT INTO downloads (title_id, rom_id, source_id, file_index, state, progress,
                                staged_path, created_at, updated_at)
         SELECT title_id, ?1, ?2, ?3, ?4, 1, ?5, 0, 0 FROM roms WHERE id = ?1",
        params![rom_id, source_id.0, file_index, state, staged_path],
    )?;
    Ok(DownloadId(conn.last_insert_rowid()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sources::{self, NewSource, SourceState};

    fn conn() -> (Connection, SourceId, i64, i64) {
        let mut c = Connection::open_in_memory().expect("open");
        crate::db::migrate::apply(&mut c).expect("migrate");
        crate::db::platforms::seed(&mut c, &mistarr_mister::platforms::PLATFORMS).expect("seed");
        let src = sources::insert(
            &c,
            &NewSource {
                infohash: &"0a".repeat(20),
                display_name: "Synthetic Set",
                origin_file: "set.torrent",
                state: SourceState::Bound,
                reason: None,
                added_at: 1,
            },
        )
        .expect("source");
        let a = sources::fixtures::seed_rom(&c, "psx", "Example Quest (USA).cue", 10, "[]")
            .expect("rom");
        let title: i64 = c
            .query_row("SELECT title_id FROM roms WHERE id = ?1", [a], |r| r.get(0))
            .expect("title");
        c.execute(
            "INSERT INTO roms (title_id, name, size, status) VALUES (?1, 'Example Quest (USA).bin', 20, 'good')",
            [title],
        )
        .expect("rom");
        (c, src, a, title)
    }

    #[test]
    fn rows_move_out_of_importing_once() {
        let (c, src, rom, title) = conn();
        let id =
            insert_fixture(&c, rom, src, 0, state::IMPORTING, Some("/s/a.cue")).expect("insert");
        let row = get(&c, id).expect("get").expect("row");
        assert_eq!(row.title_id, TitleId(title));
        assert_eq!(row.staged_path.as_deref(), Some("/s/a.cue"));
        assert_eq!(importing(&c).expect("list"), [id]);
        assert!(!source_settled(&c, src).expect("settled"));
        assert!(finish(&c, id, state::DONE, None, 5).expect("finish"));
        assert!(!finish(&c, id, state::FAILED, Some("late"), 6).expect("finish"));
        assert_eq!(get(&c, id).expect("get").expect("row").state, state::DONE);
        assert!(importing(&c).expect("list").is_empty());
        assert!(source_settled(&c, src).expect("settled"));
        assert_eq!(for_title(&c, TitleId(title)).expect("title").len(), 1);
        assert!(get(&c, DownloadId(99)).expect("get").is_none());
        assert_eq!(row.source_id, Some(src));
        assert_eq!(row.file_index, Some(0));
        let file = mistarr_sources::torrent::TorrentFile {
            index: 0,
            path: "Set/a.cue".into(),
            size: 10,
        };
        sources::replace_files(&c, src, &[file]).expect("files");
        assert_eq!(
            torrent_path(&c, src, 0).expect("path").as_deref(),
            Some("Set/a.cue")
        );
        assert!(torrent_path(&c, src, 1).expect("path").is_none());
        assert_eq!(DownloadId(3).to_string(), "3");
    }
}
