//! The `downloads` reads the importer needs beyond [`super::downloads`]; see
//! `docs/ARCHITECTURE.md` "Import".

use rusqlite::{params, Connection, OptionalExtension};

use super::candidates;
use super::downloads::{self, CancelOutcome, Cancelled, DownloadId, DownloadState, NewDownload};
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

/// Whether `other` is a live title in the clone group of `wanted`, other than `wanted` itself.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn other_version_of(conn: &Connection, wanted: TitleId, other: TitleId) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM titles w JOIN titles o
               ON COALESCE(o.parent_id, o.id) = COALESCE(w.parent_id, w.id)
             WHERE w.id = ?1 AND o.id = ?2 AND o.id != w.id AND o.retired = 0",
            params![wanted.0, other.0],
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
    rom: i64,
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
            params![source.0, index, rom],
            |r| r.get::<_, Option<i64>>(0),
        )
        .optional()?
        .flatten()
        .map(TitleId))
}

/// A verified file of `rom`, if any.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn verified_file(conn: &Connection, rom: i64) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT id FROM files WHERE rom_id = ?1 AND state = 'verified' ORDER BY id LIMIT 1",
            [rom],
            |r| r.get(0),
        )
        .optional()?)
}

/// Why a download's file was not its wanted rom, for [`settle_elsewhere`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Elsewhere {
    /// The download's error.
    pub reason: String,
    /// The torrent file.
    pub source: SourceId,
    /// Its index, when the download named one.
    pub file_index: Option<u32>,
    /// The wanted rom, which the file is not.
    pub rom_id: i64,
    /// The rom the file hashed to, when it was another version.
    pub proven: Option<i64>,
    /// Whether the torrent file itself hashed to `proven`, not a member of it.
    pub whole: bool,
    /// Whether the file was placed or kept as that rom.
    pub placed: bool,
}

/// What [`settle_elsewhere`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settled {
    /// The download, when it moved to `bad`.
    pub moved: Vec<DownloadId>,
    /// The download opened again for the wanted rom, and its state.
    pub again: Option<(DownloadId, DownloadState)>,
    /// Open downloads of the proven rom that its placed file made redundant.
    pub cancelled: Vec<Cancelled>,
}

/// Ends download `id` `bad` with the reason, forgets that its file may be the
/// wanted rom, records the rom the file proved to be and cancels that rom's
/// other open downloads once placed, then opens the wanted rom again on its
/// next best file, or `wanted` without one, while its title is still wanted.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn settle_elsewhere(
    conn: &Connection,
    id: DownloadId,
    e: &Elsewhere,
    now: i64,
) -> Result<Settled> {
    let mut out = Settled {
        moved: downloads::move_all(conn, &[id], DownloadState::Bad, Some(&e.reason), now)?,
        ..Settled::default()
    };
    if let Some(index) = e.file_index {
        if let (Some(proven), true) = (e.proven, e.whole) {
            candidates::prove(conn, e.source, index, proven)?;
        }
        candidates::drop_pair(conn, e.source, index, e.rom_id)?;
    }
    if let (Some(proven), true) = (e.proven, e.placed) {
        let redundant: Vec<i64> = conn
            .prepare(
                "SELECT id FROM downloads WHERE rom_id = ?1
                   AND state IN ('wanted', 'queued', 'transferring', 'checking')
                   AND NOT (source_id IS ?2 AND file_index IS ?3)",
            )?
            .query_map(params![proven, e.source.0, e.file_index], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for other in redundant {
            if let CancelOutcome::Cancelled(c) = downloads::cancel(conn, DownloadId(other), now)? {
                out.cancelled.push(c);
            }
        }
    }
    out.again = want_again(conn, id, &e.reason, now)?;
    Ok(out)
}

/// Opens the rom of `bad`, a download that ended `bad`, again when its
/// title is still wanted and the rom has neither a verified file nor an open
/// download: `queued` on its next best file, else `wanted`, with `note` as
/// its error so the history shows.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn want_again(
    conn: &Connection,
    bad: DownloadId,
    note: &str,
    now: i64,
) -> Result<Option<(DownloadId, DownloadState)>> {
    let Some(row) = downloads::get(conn, bad)? else {
        return Ok(None);
    };
    let open: bool = conn.query_row(
        "SELECT (SELECT wanted = 0 OR retired = 1 FROM titles WHERE id = ?1)
             OR EXISTS (SELECT 1 FROM files WHERE rom_id = ?2 AND state = 'verified')
             OR EXISTS (SELECT 1 FROM downloads WHERE rom_id = ?2
                        AND state IN ('wanted', 'queued', 'transferring', 'checking', 'importing'))",
        params![row.title_id.0, row.rom_id],
        |r| r.get(0),
    )?;
    if row.state != DownloadState::Bad || open {
        return Ok(None);
    }
    let file = downloads::best_file(conn, row.rom_id)?;
    let again = downloads::create(
        conn,
        &NewDownload {
            title_id: row.title_id,
            rom_id: row.rom_id,
            file,
            now,
        },
    )?;
    conn.execute(
        "UPDATE downloads SET error = ?2 WHERE id = ?1",
        params![again.0, note],
    )?;
    let state = if file.is_some() {
        DownloadState::Queued
    } else {
        DownloadState::Wanted
    };
    Ok(Some((again, state)))
}

/// Whether file `index` of `source` was offered for `rom` only as a fuzzy
/// or size-only candidate.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn guessed(conn: &Connection, source: SourceId, index: u32, rom: i64) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM torrent_candidates WHERE source_id = ?1
           AND file_index = ?2 AND rom_id = ?3 AND confidence IN ('fuzzy', 'size'))",
        params![source.0, index, rom],
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
        [source.0],
        |r| r.get(0),
    )?)
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
    use crate::db::imports::ImportAction;
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

        let alt = sources::fixtures::seed_rom(&c, "nes", "Example Quest (USA) (Alt).nes", 10, "[]")
            .expect("rom");
        let other =
            sources::fixtures::seed_rom(&c, "nes", "Other Tale (USA).nes", 10, "[]").expect("rom");
        let title_of = |rom: i64| {
            c.query_row("SELECT title_id FROM roms WHERE id = ?1", [rom], |r| {
                r.get(0).map(TitleId)
            })
            .expect("title")
        };
        let (main, alt_title) = (TitleId(title), title_of(alt));
        c.execute(
            "UPDATE titles SET parent_id = ?1 WHERE id IN (?1, ?2)",
            [main.0, alt_title.0],
        )
        .expect("group");
        assert!(other_version_of(&c, main, alt_title).expect("group"));
        assert!(other_version_of(&c, alt_title, main).expect("group"));
        assert!(!other_version_of(&c, main, main).expect("self"));
        assert!(!other_version_of(&c, main, title_of(other)).expect("other group"));
        c.execute("UPDATE titles SET retired = 1 WHERE id = ?1", [alt_title.0])
            .expect("retire");
        assert!(!other_version_of(&c, main, alt_title).expect("retired"));

        assert!(!placed_any(&c, src).expect("placed"));
        let bad = insert_fixture(&c, rom, src, 0, "bad", None).expect("bad");
        assert!(!placed_any(&c, src).expect("quarantined only"));
        assert_eq!(placed_on_file(&c, src, 0, rom).expect("none"), None);
        let detail = serde_json::json!({ "title_id": alt_title.0, "rom_id": alt });
        crate::db::imports::log(&c, 1, Some(bad.0), None, ImportAction::Placed, &detail)
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
            [src.0],
        )
        .expect("zip");
        assert_eq!(
            placed_on_file(&c, src, 0, rom).expect("zip"),
            None,
            "merged zip"
        );
    }

    /// A database with a wanted title of two versions and a torrent of two files.
    fn grouped() -> (Connection, SourceId, i64, i64) {
        let mut c = Connection::open_in_memory().expect("open");
        crate::db::migrate::apply(&mut c).expect("migrate");
        crate::db::platforms::seed(&mut c, &mistarr_mister::platforms::PLATFORMS).expect("seed");
        let src = sources::insert(
            &c,
            &NewSource {
                infohash: &"0b".repeat(20),
                display_name: "Synthetic Set",
                origin_file: "set.torrent",
                state: SourceState::Bound,
                reason: None,
                added_at: 1,
            },
        )
        .expect("source");
        let files: Vec<_> = ["a.nes", "b.nes"]
            .iter()
            .zip(0..)
            .map(|(p, index)| mistarr_sources::torrent::TorrentFile {
                index,
                path: (*p).to_owned(),
                size: 10,
            })
            .collect();
        sources::replace_files(&c, src, &files).expect("files");
        let rom = sources::fixtures::seed_rom(&c, "nes", "Example Quest (USA).nes", 10, "[]")
            .expect("rom");
        let alt = sources::fixtures::seed_rom(&c, "nes", "Example Quest (USA) (Alt).nes", 10, "[]")
            .expect("alt");
        c.execute(
            "UPDATE titles SET wanted = 1, parent_id = (SELECT title_id FROM roms WHERE id = ?1)
             WHERE id IN (SELECT title_id FROM roms WHERE id IN (?1, ?2))",
            [rom, alt],
        )
        .expect("group");
        (c, src, rom, alt)
    }

    fn add(c: &Connection, src: SourceId, index: u32, rom: i64, confidence: &'static str) {
        let change = candidates::Change {
            add: vec![(index, rom, confidence)],
            ..Default::default()
        };
        candidates::apply(c, src, &change).expect("candidate");
    }

    fn state_of(c: &Connection, id: DownloadId) -> DownloadState {
        downloads::get(c, id).expect("get").expect("row").state
    }

    #[test]
    fn settling_elsewhere_proves_the_file_cancels_the_rest_and_wants_again() {
        let (c, src, rom, alt) = grouped();
        add(&c, src, 0, rom, "fuzzy");
        add(&c, src, 0, alt, "size");
        add(&c, src, 1, rom, "size");
        let first = insert_fixture(&c, rom, src, 0, "importing", None).expect("first");
        let alt_elsewhere = insert_fixture(&c, alt, src, 1, "queued", None).expect("alt");
        assert!(guessed(&c, src, 0, rom).expect("guessed"));
        let e = Elsewhere {
            reason: "the file in this source is a different version: Alt".to_owned(),
            source: src,
            file_index: Some(0),
            rom_id: rom,
            proven: Some(alt),
            whole: true,
            placed: true,
        };
        let settled = settle_elsewhere(&c, first, &e, 5).expect("settle");
        assert_eq!(settled.moved, [first]);
        assert_eq!(state_of(&c, first), DownloadState::Bad);
        assert_eq!(settled.cancelled.len(), 1);
        assert_eq!(settled.cancelled[0].id, alt_elsewhere);
        assert!(candidates::of_file(&c, src, 0).expect("of").is_empty());
        let (proven, confidence): (i64, String) = c
            .query_row(
                "SELECT rom_id, confidence FROM torrent_files WHERE source_id = ?1 AND file_index = 0",
                [src.0],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("file");
        assert_eq!((proven, confidence.as_str()), (alt, candidates::PROVEN));
        let (again, state) = settled.again.expect("wanted again");
        assert_eq!(state, DownloadState::Queued, "the next file is tried");
        let row = downloads::get(&c, again).expect("get").expect("row");
        assert_eq!((row.rom_id, row.file_index), (rom, Some(1)));
        assert_eq!(row.error.as_deref(), Some(e.reason.as_str()));
    }

    #[test]
    fn a_rom_is_wanted_again_only_while_nothing_holds_it() {
        let (c, src, rom, alt) = grouped();
        let open = insert_fixture(&c, rom, src, 0, "queued", None).expect("open");
        let bad = insert_fixture(&c, rom, src, 1, "bad", None).expect("bad");
        assert_eq!(want_again(&c, open, "n", 1).expect("open"), None, "not bad");
        assert_eq!(
            want_again(&c, bad, "n", 1).expect("busy"),
            None,
            "one is open"
        );
        c.execute("UPDATE downloads SET state = 'bad' WHERE id = ?1", [open.0])
            .expect("bad");
        let (again, state) = want_again(&c, bad, "n", 1)
            .expect("again")
            .expect("reopened");
        assert_eq!(state, DownloadState::Wanted, "both files ruled out");
        assert_eq!(state_of(&c, again), DownloadState::Wanted);
        downloads::cancel(&c, again, 1).expect("cancel");
        assert_eq!(verified_file(&c, rom).expect("none"), None);
        c.execute(
            "INSERT INTO files (platform_id, rel_path, size, mtime, rom_id, state, scanned_at)
             VALUES ('nes', 'Example Quest (USA).nes', 10, 0, ?1, 'verified', 0)",
            [rom],
        )
        .expect("file");
        assert!(verified_file(&c, rom).expect("verified").is_some());
        assert_eq!(verified_file(&c, alt).expect("alt"), None);
        assert_eq!(want_again(&c, bad, "n", 1).expect("held"), None, "held");
        assert!(!guessed(&c, src, 0, rom).expect("no candidate"));
    }
}
