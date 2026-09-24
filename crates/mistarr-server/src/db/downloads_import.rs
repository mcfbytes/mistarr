//! The `downloads` reads the importer needs beyond [`super::downloads`]; see
//! `docs/ARCHITECTURE.md` "Import".

use rusqlite::{params, Connection, OptionalExtension};

use super::downloads::DownloadId;
use super::sources::SourceId;
use super::titles::TitleId;
use crate::error::Result;

/// Every download of a title, oldest first.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn for_title(conn: &Connection, title_id: TitleId) -> Result<Vec<DownloadId>> {
    let mut stmt = conn.prepare("SELECT id FROM downloads WHERE title_id = ?1 ORDER BY id")?;
    let rows = stmt.query_map([title_id.0], |r| r.get(0).map(DownloadId))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
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

/// Inserts a download row in `state` directly, standing in for the transfer
/// poller in tests.
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

    #[test]
    fn title_rows_and_torrent_paths() {
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
        let rom = sources::fixtures::seed_rom(&c, "nes", "Example Quest (USA).nes", 10, "[]")
            .expect("rom");
        let title: i64 = c
            .query_row("SELECT title_id FROM roms WHERE id = ?1", [rom], |r| {
                r.get(0)
            })
            .expect("title");
        let id = insert_fixture(&c, rom, src, 0, "importing", Some("/s/a.nes")).expect("insert");
        assert_eq!(for_title(&c, TitleId(title)).expect("rows"), [id]);
        let row = crate::db::downloads::get(&c, id)
            .expect("get")
            .expect("row");
        assert_eq!(row.staged_path.as_deref(), Some("/s/a.nes"));
        let file = mistarr_sources::torrent::TorrentFile {
            index: 0,
            path: "Set/a.nes".into(),
            size: 10,
        };
        sources::replace_files(&c, src, &[file]).expect("files");
        assert_eq!(
            torrent_path(&c, src, 0).expect("path").as_deref(),
            Some("Set/a.nes")
        );
        assert!(torrent_path(&c, src, 1).expect("path").is_none());
    }
}
