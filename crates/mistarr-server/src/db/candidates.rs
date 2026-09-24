//! The `torrent_candidates` table and the availability read that joins it
//! with `torrent_files`; see `docs/DATA-MODEL.md` and `docs/VERIFICATION.md`.

use mistarr_core::PlatformId;
use mistarr_sources::binding::{Confidence, RomRef};
use mistarr_sources::fuzzy::SizeIndex;
use rusqlite::{params, Connection};
use serde::Serialize;

use super::sources::{confidence_text, SourceId};
use super::titles::TitleId;
use crate::error::Result;

/// Orders a `confidence` column from strongest to weakest: name, base, fuzzy, size.
pub(crate) const RANK: &str =
    "CASE {c} WHEN 'name' THEN 0 WHEN 'base' THEN 1 WHEN 'fuzzy' THEN 2 WHEN 'size' THEN 3 ELSE 4 END";

/// [`RANK`] over the column `column`.
pub(crate) fn rank(column: &str) -> String {
    RANK.replace("{c}", column)
}

/// True when a `bad` download already tried this file for this rom.
const NOT_BAD: &str = "NOT EXISTS (SELECT 1 FROM downloads b
    WHERE b.rom_id = {rom} AND b.state = 'bad' AND b.source_id = {src} AND b.file_index = {idx})";

/// [`NOT_BAD`] over the given columns.
pub(crate) fn not_bad(rom: &str, src: &str, idx: &str) -> String {
    NOT_BAD
        .replace("{rom}", rom)
        .replace("{src}", src)
        .replace("{idx}", idx)
}

/// Replaces the source's candidates with `found`, skipping a pair its
/// `torrent_files` row already maps and one a `bad` download ruled out.
/// Returns how many rows were stored.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn replace(
    conn: &Connection,
    source: SourceId,
    found: &[(u32, RomRef, Confidence)],
) -> Result<usize> {
    clear(conn, source)?;
    let mut insert = conn.prepare_cached(&format!(
        "INSERT OR IGNORE INTO torrent_candidates (source_id, file_index, rom_id, confidence)
         SELECT ?1, ?2, ?3, ?4
         WHERE NOT EXISTS (SELECT 1 FROM torrent_files tf
                           WHERE tf.source_id = ?1 AND tf.file_index = ?2 AND tf.rom_id = ?3)
           AND {}",
        not_bad("?3", "?1", "?2")
    ))?;
    let mut stored = 0;
    for (index, rom, confidence) in found {
        let Some(text) = confidence_text(*confidence) else {
            continue;
        };
        stored += insert.execute(params![source.0, index, rom.0, text])?;
    }
    Ok(stored)
}

/// Removes every candidate of the source.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn clear(conn: &Connection, source: SourceId) -> Result<()> {
    conn.execute(
        "DELETE FROM torrent_candidates WHERE source_id = ?1",
        [source.0],
    )?;
    Ok(())
}

/// Forgets that file `index` of `source` may be `rom`, as a candidate and as
/// its `torrent_files` match, once its hashes showed it is another rom.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn drop_pair(conn: &Connection, source: SourceId, index: u32, rom: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM torrent_candidates WHERE source_id = ?1 AND file_index = ?2 AND rom_id = ?3",
        params![source.0, index, rom],
    )?;
    conn.execute(
        "UPDATE torrent_files SET rom_id = NULL, confidence = NULL
         WHERE source_id = ?1 AND file_index = ?2 AND rom_id = ?3",
        params![source.0, index, rom],
    )?;
    Ok(())
}

/// The candidates of one file, strongest first, as `(rom_id, confidence)`.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn of_file(conn: &Connection, source: SourceId, index: u32) -> Result<Vec<(i64, String)>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT rom_id, confidence FROM torrent_candidates
         WHERE source_id = ?1 AND file_index = ?2 ORDER BY {}, rom_id",
        rank("confidence")
    ))?;
    let rows = stmt
        .query_map(params![source.0, index], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// One torrent file of a bound source that may hold a rom of a title.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Availability {
    /// The source.
    pub source_id: SourceId,
    /// The torrent's name.
    pub source_name: String,
    /// The file's index in the torrent.
    pub file_index: u32,
    /// The file's path inside the torrent.
    pub path: String,
    /// The rom the file may be.
    pub rom_id: i64,
    /// `name`, `base`, `fuzzy` or `size`.
    pub confidence: String,
}

/// Every file of a bound source mapped to, or a candidate for, a live rom of
/// a title in the clone group rooted at `parent`, with the title it serves;
/// strongest confidence first. Pairs a `bad` download ruled out are left out.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn for_group(conn: &Connection, parent: TitleId) -> Result<Vec<(TitleId, Availability)>> {
    let group = "FROM titles t JOIN roms r ON r.title_id = t.id AND r.retired = 0";
    let sql = format!(
        "SELECT title_id, source_id, display_name, file_index, path, rom_id, confidence FROM (
           SELECT t.id AS title_id, tf.source_id, s.display_name, tf.file_index, tf.path,
                  r.id AS rom_id, tf.confidence
           {group}
           JOIN torrent_files tf ON tf.rom_id = r.id
           JOIN sources s ON s.id = tf.source_id AND s.state = 'bound'
           WHERE (t.parent_id = ?1 OR t.id = ?1) AND {bad_tf}
           UNION ALL
           SELECT t.id, c.source_id, s.display_name, c.file_index, tf.path, r.id, c.confidence
           {group}
           JOIN torrent_candidates c ON c.rom_id = r.id
           JOIN torrent_files tf ON tf.source_id = c.source_id AND tf.file_index = c.file_index
           JOIN sources s ON s.id = c.source_id AND s.state = 'bound'
           WHERE (t.parent_id = ?1 OR t.id = ?1) AND {bad_c}
         )
         ORDER BY title_id, {rank}, source_id, file_index, rom_id",
        bad_tf = not_bad("r.id", "tf.source_id", "tf.file_index"),
        bad_c = not_bad("r.id", "c.source_id", "c.file_index"),
        rank = rank("confidence"),
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([parent.0], |r| {
            Ok((
                TitleId(r.get(0)?),
                Availability {
                    source_id: SourceId(r.get(1)?),
                    source_name: r.get(2)?,
                    file_index: r.get(3)?,
                    path: r.get(4)?,
                    rom_id: r.get(5)?,
                    confidence: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
                },
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// The rom lookup of the fuzzy tiers for one platform: live roms of titles
/// that are neither retired nor BIOS, whose size is the file's, or the
/// file's less the copier header the platform's header rule skips. Query
/// failures read as no rom.
pub struct SqlSizeIndex<'c> {
    conn: &'c Connection,
    platform: &'c PlatformId,
    header: u64,
}

impl<'c> SqlSizeIndex<'c> {
    /// An index over `conn` for `platform`; call
    /// [`super::sources::refresh_match_keys`] on it first.
    #[must_use]
    pub fn new(conn: &'c Connection, platform: &'c PlatformId) -> Self {
        let rule = mistarr_mister::platforms::by_id(&platform.0).map_or("none", |p| p.header_rule);
        Self {
            conn,
            platform,
            header: header_len(rule),
        }
    }
}

/// Bytes of the header a platform's header rule skips when hashing, 0 for none.
///
/// ```
/// use mistarr_server::db::candidates::header_len;
/// assert_eq!((header_len("ines"), header_len("n64")), (16, 0));
/// ```
#[must_use]
pub fn header_len(rule: &str) -> u64 {
    match rule {
        "ines" => 16,
        "lnx" => 64,
        "a78" => 128,
        "smc" => 512,
        _ => 0,
    }
}

impl SizeIndex for SqlSizeIndex<'_> {
    fn roms_of_size(&self, size: u64) -> Vec<(RomRef, String)> {
        let run = || -> rusqlite::Result<Vec<(RomRef, String)>> {
            let mut stmt = self.conn.prepare_cached(
                "SELECT r.id, r.match_base FROM roms r JOIN titles t ON t.id = r.title_id
                 WHERE r.size IN (?2, ?3) AND t.platform_id = ?1 AND r.retired = 0 AND t.retired = 0
                   AND r.match_base IS NOT NULL AND t.flags NOT LIKE '%\"bios\"%'
                 ORDER BY r.id",
            )?;
            let bare = size.checked_sub(self.header).filter(|_| self.header > 0);
            let size = i64::try_from(size).unwrap_or(i64::MAX);
            let bare = bare.map_or(size, |b| i64::try_from(b).unwrap_or(i64::MAX));
            let rows = stmt.query_map(params![self.platform.0, size, bare], |r| {
                Ok((RomRef(r.get(0)?), r.get(1)?))
            })?;
            rows.collect()
        };
        run().unwrap_or_else(|e| {
            tracing::warn!(error = %e, "rom size lookup failed");
            Vec::new()
        })
    }
}

#[cfg(test)]
mod tests {
    use mistarr_sources::torrent::TorrentFile;

    use super::*;
    use crate::db::sources::{self, fixtures::seed_rom, NewSource, SourceState};

    fn conn() -> Connection {
        let mut c = Connection::open_in_memory().expect("open");
        crate::db::migrate::apply(&mut c).expect("migrate");
        crate::db::platforms::seed(&mut c, &mistarr_mister::platforms::PLATFORMS).expect("seed");
        c
    }

    fn source(c: &Connection, byte: &str, state: SourceState) -> SourceId {
        let id = sources::insert(
            c,
            &NewSource {
                infohash: &byte.repeat(20),
                display_name: "Synthetic Set",
                origin_file: "s.torrent",
                state,
                reason: None,
                added_at: 0,
            },
        )
        .expect("insert");
        let files = [
            TorrentFile {
                index: 0,
                path: "Set/nova.nes".into(),
                size: 16,
            },
            TorrentFile {
                index: 1,
                path: "Set/cover.png".into(),
                size: 4,
            },
        ];
        sources::replace_files(c, id, &files).expect("files");
        sources::set_binding(c, id, Some(&PlatformId("nes".into())), Some(0.0)).expect("bind");
        id
    }

    fn title_of(c: &Connection, rom: i64) -> TitleId {
        c.query_row("SELECT title_id FROM roms WHERE id = ?1", [rom], |r| {
            r.get(0).map(TitleId)
        })
        .expect("title")
    }

    #[test]
    fn candidates_are_stored_ranked_listed_and_dropped() {
        let c = conn();
        let a = seed_rom(&c, "nes", "Nova Quest (World).nes", 16, "[]").expect("rom");
        let b = seed_rom(&c, "nes", "Nova Quest (World) (Alt).nes", 16, "[]").expect("rom");
        let src = source(&c, "0a", SourceState::Bound);
        let found = [
            (0, RomRef(b), Confidence::Size),
            (0, RomRef(a), Confidence::Fuzzy),
            (0, RomRef(a), Confidence::Fuzzy),
            (1, RomRef(a), Confidence::Unmatched),
        ];
        assert_eq!(replace(&c, src, &found).expect("replace"), 2);
        assert_eq!(
            of_file(&c, src, 0).expect("of"),
            [(a, "fuzzy".to_owned()), (b, "size".to_owned())]
        );
        let (ta, tb) = (title_of(&c, a), title_of(&c, b));
        c.execute("UPDATE titles SET parent_id = ?1", [ta.0])
            .expect("group");
        let listed = for_group(&c, ta).expect("group");
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].0, ta);
        assert_eq!(listed[0].1.path, "Set/nova.nes");
        assert_eq!(listed[0].1.source_name, "Synthetic Set");
        assert_eq!((listed[1].0, listed[1].1.confidence.as_str()), (tb, "size"));

        crate::db::downloads_import::insert_fixture(&c, a, src, 0, "bad", None).expect("bad");
        assert_eq!(for_group(&c, ta).expect("group").len(), 1, "ruled out");
        assert_eq!(replace(&c, src, &found).expect("again"), 1);
        drop_pair(&c, src, 0, b).expect("drop");
        assert!(of_file(&c, src, 0).expect("of").is_empty());

        sources::set_state(&c, src, SourceState::Disabled, None).expect("disable");
        replace(&c, src, &[(0, RomRef(b), Confidence::Size)]).expect("replace");
        assert!(for_group(&c, ta).expect("group").is_empty(), "bound only");
        clear(&c, src).expect("clear");
        assert!(of_file(&c, src, 0).expect("of").is_empty());
    }

    #[test]
    fn a_mapped_pair_is_not_a_candidate_and_files_cascade() {
        let c = conn();
        let a = seed_rom(&c, "nes", "Nova Quest (World).nes", 16, "[]").expect("rom");
        let src = source(&c, "0b", SourceState::Bound);
        sources::set_matches(&c, src, &[(0, Some(RomRef(a)), Confidence::Name)]).expect("map");
        assert_eq!(
            replace(&c, src, &[(0, RomRef(a), Confidence::Fuzzy)]).expect("replace"),
            0
        );
        drop_pair(&c, src, 0, a).expect("drop");
        assert_eq!(
            sources::files(&c, src, 10, 0).expect("files").0[0].rom_id,
            None
        );
        replace(&c, src, &[(0, RomRef(a), Confidence::Fuzzy)]).expect("replace");
        sources::replace_files(&c, src, &[]).expect("empty");
        assert!(of_file(&c, src, 0).expect("of").is_empty(), "cascaded");
    }

    #[test]
    fn size_index_reads_live_roms_of_one_platform() {
        let c = conn();
        let a = seed_rom(&c, "nes", "Nova Quest (World).nes", 16, "[]").expect("rom");
        seed_rom(&c, "nes", "Boot (World).nes", 16, r#"["bios"]"#).expect("bios");
        seed_rom(&c, "snes", "Nova Quest (World).sfc", 16, "[]").expect("snes");
        seed_rom(&c, "nes", "Other (World).nes", 8, "[]").expect("other");
        sources::refresh_match_keys(&c).expect("keys");
        let nes = PlatformId("nes".into());
        let index = SqlSizeIndex::new(&c, &nes);
        assert_eq!(
            index.roms_of_size(16),
            [(RomRef(a), "nova quest".to_owned())]
        );
        assert!(index.roms_of_size(u64::MAX).is_empty());
        assert_eq!(index.roms_of_size(32).len(), 1, "an iNES header on top");
        assert_eq!(header_len("smc"), 512);
        assert!(rank("x").contains("WHEN 'fuzzy' THEN 2"));
        assert!(not_bad("1", "2", "3").contains("b.rom_id = 1"));
    }
}
