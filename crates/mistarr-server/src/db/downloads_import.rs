//! The `downloads` reads the importer needs beyond [`super::downloads`]; see
//! `docs/ARCHITECTURE.md` "Import".

use rusqlite::{params, Connection, OptionalExtension};

use super::candidates::MatchConfidence;
use super::ids::{DownloadId, RomId, SourceId, TitleId};
use crate::error::Result;

/// Every download of a title, oldest first.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn for_title(conn: &Connection, title_id: TitleId) -> Result<Vec<DownloadId>> {
    let mut stmt = conn.prepare("SELECT id FROM downloads WHERE title_id = ?1 ORDER BY id")?;
    let rows = stmt.query_map([title_id], |r| r.get(0))?;
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
            params![source_id, index],
            |r| r.get(0),
        )
        .optional()?)
}

/// Whether `other` is a live title in the effective clone group of `wanted`, other than itself.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn other_version_of(conn: &Connection, wanted: TitleId, other: TitleId) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM titles w JOIN titles o
               ON COALESCE(o.group_root, o.parent_id, o.id) = COALESCE(w.group_root, w.parent_id, w.id)
             WHERE w.id = ?1 AND o.id = ?2 AND o.id != w.id AND o.retired = 0",
            params![wanted, other],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// The title a download of file `index` in `source` last placed or kept as
/// a rom other than `rom`, from the import log; covers a `bad` download
/// whose file went to another version as well as a `done` one. `None` for a
/// zip, whose other members are other roms.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn placed_on_file(
    conn: &Connection,
    source: SourceId,
    index: u32,
    rom: RomId,
) -> Result<Option<TitleId>> {
    Ok(conn
        .query_row(
            "SELECT json_extract(l.detail, '$.title_id') FROM import_log l
             JOIN downloads d ON d.id = l.download_id
             JOIN torrent_files f ON f.source_id = d.source_id AND f.file_index = d.file_index
             WHERE d.source_id = ?1 AND d.file_index = ?2 AND lower(f.path) NOT LIKE '%.zip'
               AND l.action IN ('placed', 'replaced', 'skipped_existing')
               AND json_extract(l.detail, '$.rom_id') != ?3
             ORDER BY l.id DESC LIMIT 1",
            params![source, index, rom],
            |r| r.get::<_, Option<TitleId>>(0),
        )
        .optional()?
        .flatten())
}

/// Whether file `index` of `source` was offered for `rom` only as a fuzzy
/// or size-only candidate.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn guessed(conn: &Connection, source: SourceId, index: u32, rom: RomId) -> Result<bool> {
    Ok(conn.query_row(
        &format!(
            "SELECT EXISTS (SELECT 1 FROM torrent_candidates WHERE source_id = ?1
               AND file_index = ?2 AND rom_id = ?3 AND confidence IN {})",
            MatchConfidence::GUESSED_SQL
        ),
        params![source, index, rom],
        |r| r.get(0),
    )?)
}

/// Whether a download of `source` placed its file: one `done`, or one `bad`
/// whose file was placed as another version of its entry.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn placed_any(conn: &Connection, source: SourceId) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM downloads d WHERE d.source_id = ?1
           AND (d.state = 'done' OR (d.state = 'bad' AND EXISTS (
                 SELECT 1 FROM import_log l WHERE l.download_id = d.id
                   AND l.action IN ('placed', 'replaced', 'skipped_existing')))))",
        [source],
        |r| r.get(0),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::imports::ImportAction;
    use crate::db::sources::{self, NewSource, SourceState};

    #[test]
    fn title_rows_and_torrent_paths() {
        use crate::db::downloads::DownloadState;
        use crate::db::fixtures::{conn, download, pid, seed_rom};
        let c = conn();
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
        let rom = seed_rom(&c, &pid("nes"), "Example Quest (USA).nes", 10, &[]).expect("rom");
        let title: TitleId = c
            .query_row("SELECT title_id FROM roms WHERE id = ?1", [rom], |r| {
                r.get(0)
            })
            .expect("title");
        let id =
            download(&c, rom, src, 0, DownloadState::Importing, Some("/s/a.nes")).expect("insert");
        assert_eq!(for_title(&c, title).expect("rows"), [id]);
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

        let alt = seed_rom(&c, &pid("nes"), "Example Quest (USA) (Alt).nes", 10, &[]).expect("rom");
        let other = seed_rom(&c, &pid("nes"), "Other Tale (USA).nes", 10, &[]).expect("rom");
        let title_of = |rom: RomId| {
            c.query_row("SELECT title_id FROM roms WHERE id = ?1", [rom], |r| {
                r.get::<_, TitleId>(0)
            })
            .expect("title")
        };
        let (main, alt_title) = (title, title_of(alt));
        c.execute(
            "UPDATE titles SET parent_id = ?1 WHERE id IN (?1, ?2)",
            [main, alt_title],
        )
        .expect("group");
        assert!(other_version_of(&c, main, alt_title).expect("group"));
        assert!(other_version_of(&c, alt_title, main).expect("group"));
        assert!(!other_version_of(&c, main, main).expect("self"));
        assert!(!other_version_of(&c, main, title_of(other)).expect("other group"));
        let linked = title_of(other);
        c.execute(
            "UPDATE titles SET group_root = ?1 WHERE id = ?2",
            [main, linked],
        )
        .expect("link");
        assert!(other_version_of(&c, main, linked).expect("linked by another DAT"));
        c.execute("UPDATE titles SET retired = 1 WHERE id = ?1", [alt_title])
            .expect("retire");
        assert!(!other_version_of(&c, main, alt_title).expect("retired"));

        assert!(!placed_any(&c, src).expect("placed"));
        let bad = download(
            &c,
            rom,
            src,
            0,
            crate::db::downloads::DownloadState::Bad,
            None,
        )
        .expect("bad");
        assert!(!placed_any(&c, src).expect("quarantined only"));
        assert_eq!(placed_on_file(&c, src, 0, rom).expect("none"), None);
        let detail = serde_json::json!({ "title_id": alt_title.0, "rom_id": alt });
        crate::db::imports::log(&c, 1, Some(bad), None, ImportAction::Placed, &detail)
            .expect("log");
        assert!(placed_any(&c, src).expect("placed as another version"));
        assert_eq!(
            placed_on_file(&c, src, 0, rom).expect("placed"),
            Some(alt_title),
            "a bad download that placed the file counts"
        );
        assert_eq!(placed_on_file(&c, src, 0, alt).expect("own rom"), None);
        assert_eq!(placed_on_file(&c, src, 1, rom).expect("other file"), None);
        c.execute(
            "UPDATE torrent_files SET path = 'Set/pack.ZIP' WHERE source_id = ?1",
            [src],
        )
        .expect("zip");

        assert_eq!(
            placed_on_file(&c, src, 0, rom).expect("zip"),
            None,
            "merged zip"
        );
    }
}
