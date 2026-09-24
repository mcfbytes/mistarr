//! The `torrent_candidates` table and the availability read that joins it
//! with `torrent_files`; see `docs/DATA-MODEL.md` and `docs/VERIFICATION.md`.

use std::collections::{BTreeMap, HashMap, HashSet};

use mistarr_core::hash::HeaderRule;
use mistarr_core::PlatformId;
use mistarr_sources::binding::{Confidence, RomRef};
use mistarr_sources::fuzzy::{SizeIndex, SizedRom};
use rusqlite::{params, Connection};
use serde::Serialize;

use super::sources::{confidence_text, SourceId};
use super::titles::TitleId;
use crate::error::Result;

/// Orders a `confidence` column from strongest to weakest: hash, name, base, fuzzy, size.
pub(crate) const RANK: &str = "CASE {c} WHEN 'hash' THEN 0 WHEN 'name' THEN 1 WHEN 'base' THEN 2
    WHEN 'fuzzy' THEN 3 WHEN 'size' THEN 4 ELSE 5 END";

/// 0 for a confidence of the name tiers or a hash, 1 for the fuzzy and size-only tiers.
pub(crate) const TIER: &str = "CASE WHEN {c} IN ('hash', 'name', 'base') THEN 0 ELSE 1 END";

/// [`TIER`] over the column `column`.
pub(crate) fn tier(column: &str) -> String {
    TIER.replace("{c}", column)
}

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

/// The confidence of a pair a hash proved, stored in `torrent_files`.
pub const PROVEN: &str = "hash";

/// The static text of a stored `confidence`, so a mapping of many files
/// holds no string per file; `None` for a value this build does not know.
#[must_use]
pub fn known(text: &str) -> Option<&'static str> {
    ["hash", "name", "base", "fuzzy", "size"]
        .into_iter()
        .find(|k| *k == text)
}

/// A source's mapping as stored, and the pairs `bad` downloads ruled out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stored {
    /// Each matched file's `torrent_files` rom and confidence; an unmatched file is absent.
    pub matches: BTreeMap<u32, (Option<i64>, Option<&'static str>)>,
    /// Each `(file, rom)` candidate and its confidence.
    pub candidates: BTreeMap<(u32, i64), &'static str>,
    /// `(file, rom)` pairs a `bad` download used.
    pub bad: HashSet<(u32, i64)>,
}

/// The stored mapping of `source`.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn stored(conn: &Connection, source: SourceId) -> Result<Stored> {
    let mut out = Stored::default();
    let mut stmt = conn.prepare_cached(
        "SELECT file_index, rom_id, confidence FROM torrent_files
         WHERE source_id = ?1 AND (rom_id IS NOT NULL OR confidence IS NOT NULL)",
    )?;
    let mut rows = stmt.query([source.0])?;
    while let Some(r) = rows.next()? {
        let text: Option<String> = r.get(2)?;
        let text = text.as_deref().and_then(known);
        out.matches.insert(r.get(0)?, (r.get(1)?, text));
    }
    let mut stmt = conn.prepare_cached(
        "SELECT file_index, rom_id, confidence FROM torrent_candidates WHERE source_id = ?1",
    )?;
    let mut rows = stmt.query([source.0])?;
    while let Some(r) = rows.next()? {
        let text: String = r.get(2)?;
        out.candidates
            .insert((r.get(0)?, r.get(1)?), known(&text).unwrap_or_default());
    }
    let mut stmt = conn.prepare_cached(
        "SELECT file_index, rom_id FROM downloads
         WHERE source_id = ?1 AND state = 'bad' AND file_index IS NOT NULL",
    )?;
    let mut rows = stmt.query([source.0])?;
    while let Some(r) = rows.next()? {
        out.bad.insert((r.get(0)?, r.get(1)?));
    }
    Ok(out)
}

/// The writes that turn a stored mapping into a new one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Change {
    /// `torrent_files` rows whose rom or confidence changes.
    pub matches: Vec<(u32, Option<i64>, Option<&'static str>)>,
    /// Candidates to remove.
    pub remove: Vec<(u32, i64)>,
    /// Candidates to add.
    pub add: Vec<(u32, i64, &'static str)>,
}

impl Change {
    /// Whether nothing changes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.matches.is_empty() && self.remove.is_empty() && self.add.is_empty()
    }

    /// The same writes in pieces of at most `rows` rows, removals before additions.
    #[must_use]
    pub fn split(self, rows: usize) -> Vec<Change> {
        let rows = rows.max(1);
        let mut out = Vec::new();
        for m in self.matches.chunks(rows) {
            out.push(Change {
                matches: m.to_vec(),
                ..Change::default()
            });
        }
        for r in self.remove.chunks(rows) {
            out.push(Change {
                remove: r.to_vec(),
                ..Change::default()
            });
        }
        for a in self.add.chunks(rows) {
            out.push(Change {
                add: a.to_vec(),
                ..Change::default()
            });
        }
        out
    }
}

/// The [`Change`] from `stored` to the mapping of `matches` (one per file)
/// and `found` (further candidates). A file a hash proved keeps its row and
/// gets no candidate; a candidate a `bad` download ruled out, or equal to its
/// file's match, is left out; a pair found twice keeps its strongest confidence.
#[must_use]
pub fn diff(
    stored: &Stored,
    matches: &[(u32, Option<RomRef>, Confidence)],
    found: &[(u32, RomRef, Confidence)],
) -> Change {
    let proven = |i: u32| {
        stored
            .matches
            .get(&i)
            .is_some_and(|(_, c)| c.as_deref() == Some(PROVEN))
    };
    let mut change = Change::default();
    let mut mapped: HashMap<u32, i64> = HashMap::new();
    for (i, rom, confidence) in matches {
        if proven(*i) {
            continue;
        }
        let rom = rom.map(|r| r.0);
        if let Some(r) = rom {
            mapped.insert(*i, r);
        }
        let text = confidence_text(*confidence);
        let now = stored.matches.get(i).copied().unwrap_or((None, None));
        if now != (rom, text) {
            change.matches.push((*i, rom, text));
        }
    }
    let mut wanted: BTreeMap<(u32, i64), Confidence> = BTreeMap::new();
    for (i, rom, confidence) in found {
        let keep = confidence_text(*confidence).is_some()
            && !proven(*i)
            && !stored.bad.contains(&(*i, rom.0))
            && mapped.get(i) != Some(&rom.0);
        if keep {
            let slot = wanted.entry((*i, rom.0)).or_insert(*confidence);
            *slot = (*slot).min(*confidence);
        }
    }
    for (pair, text) in &stored.candidates {
        let same = wanted
            .get(pair)
            .and_then(|c| confidence_text(*c))
            .is_some_and(|t| t == *text);
        if !same {
            change.remove.push(*pair);
        }
    }
    for ((i, rom), confidence) in wanted {
        let text = confidence_text(confidence).unwrap_or_default();
        if stored.candidates.get(&(i, rom)).copied() != Some(text) {
            change.add.push((i, rom, text));
        }
    }
    change
}

/// Writes `change` for `source`.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn apply(conn: &Connection, source: SourceId, change: &Change) -> Result<()> {
    let mut update = conn.prepare_cached(
        "UPDATE torrent_files SET rom_id = ?3, confidence = ?4
         WHERE source_id = ?1 AND file_index = ?2",
    )?;
    for (i, rom, text) in &change.matches {
        update.execute(params![source.0, i, rom, text])?;
    }
    let mut remove = conn.prepare_cached(
        "DELETE FROM torrent_candidates WHERE source_id = ?1 AND file_index = ?2 AND rom_id = ?3",
    )?;
    for (i, rom) in &change.remove {
        remove.execute(params![source.0, i, rom])?;
    }
    let mut add = conn.prepare_cached(
        "INSERT OR REPLACE INTO torrent_candidates (source_id, file_index, rom_id, confidence)
         SELECT ?1, ?2, ?3, ?4 WHERE EXISTS (SELECT 1 FROM torrent_files
                                             WHERE source_id = ?1 AND file_index = ?2)",
    )?;
    for (i, rom, text) in &change.add {
        add.execute(params![source.0, i, rom, text])?;
    }
    Ok(())
}

/// Records that file `index` of `source` hashed to `rom`: its row names the
/// rom as [`PROVEN`] and its candidates are dropped.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn prove(conn: &Connection, source: SourceId, index: u32, rom: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM torrent_candidates WHERE source_id = ?1 AND file_index = ?2",
        params![source.0, index],
    )?;
    conn.execute(
        "UPDATE torrent_files SET rom_id = ?3, confidence = ?4
         WHERE source_id = ?1 AND file_index = ?2",
        params![source.0, index, rom, PROVEN],
    )?;
    Ok(())
}

/// A text that changes whenever the live roms of `platform` do, so a source
/// mapped against the same text needs no new mapping.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn rom_stamp(conn: &Connection, platform: &PlatformId) -> Result<String> {
    Ok(conn.query_row(
        "SELECT COUNT(*) || ':' || COALESCE(MAX(r.id), 0) || ':' || COALESCE(SUM(r.id), 0)
         FROM roms r JOIN titles t ON t.id = r.title_id
         WHERE t.platform_id = ?1 AND r.retired = 0 AND t.retired = 0",
        [&platform.0],
        |r| r.get(0),
    )?)
}

/// The candidates of files `from..to` of `source` with their rom names,
/// strongest first per file.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn of_files(
    conn: &Connection,
    source: SourceId,
    from: u32,
    to: u32,
) -> Result<Vec<(u32, FileCandidate)>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT c.file_index, c.rom_id, r.name, r.title_id, c.confidence
         FROM torrent_candidates c JOIN roms r ON r.id = c.rom_id
         WHERE c.source_id = ?1 AND c.file_index BETWEEN ?2 AND ?3
         ORDER BY c.file_index, {}, c.rom_id",
        rank("c.confidence")
    ))?;
    let rows = stmt
        .query_map(params![source.0, from, to], |r| {
            Ok((
                r.get(0)?,
                FileCandidate {
                    rom_id: r.get(1)?,
                    rom_name: r.get(2)?,
                    title_id: r.get(3)?,
                    confidence: r.get(4)?,
                },
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// A candidate rom of one torrent file, as `/sources/{id}/files` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileCandidate {
    /// The rom.
    pub rom_id: i64,
    /// Its DAT name.
    pub rom_name: String,
    /// Its title.
    pub title_id: i64,
    /// `name`, `base`, `fuzzy` or `size`.
    pub confidence: String,
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
/// file's less the header the platform's hashing skips, with their clone
/// group. Query failures read as no rom.
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
        Self {
            conn,
            platform,
            header: header_len(platform),
        }
    }
}

/// Bytes of the header `platform`'s hashing skips, 0 for none or an unknown platform.
///
/// ```
/// use mistarr_core::PlatformId;
/// use mistarr_server::db::candidates::header_len;
/// assert_eq!(header_len(&PlatformId("nes".into())), 16);
/// assert_eq!(header_len(&PlatformId("gba".into())), 0);
/// ```
#[must_use]
pub fn header_len(platform: &PlatformId) -> u64 {
    mistarr_mister::platforms::by_id(&platform.0)
        .map_or(0, |p| HeaderRule::from_name(p.header_rule).header_len())
}

impl SizeIndex for SqlSizeIndex<'_> {
    fn roms_of_size(&self, size: u64) -> Vec<SizedRom> {
        let run = || -> rusqlite::Result<Vec<SizedRom>> {
            let mut stmt = self.conn.prepare_cached(
                "SELECT r.id, r.match_base, COALESCE(t.parent_id, t.id)
                 FROM roms r JOIN titles t ON t.id = r.title_id
                 WHERE r.size IN (?2, ?3) AND t.platform_id = ?1 AND r.retired = 0 AND t.retired = 0
                   AND r.match_base IS NOT NULL AND t.flags NOT LIKE '%\"bios\"%'
                 ORDER BY r.id",
            )?;
            let bare = size.checked_sub(self.header).filter(|_| self.header > 0);
            let size = i64::try_from(size).unwrap_or(i64::MAX);
            let bare = bare.map_or(size, |b| i64::try_from(b).unwrap_or(i64::MAX));
            let rows = stmt.query_map(params![self.platform.0, size, bare], |r| {
                Ok(SizedRom {
                    rom: RomRef(r.get(0)?),
                    base: r.get(1)?,
                    group: r.get(2)?,
                })
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

    /// Replaces the candidates of `src` with `found`, as a mapping does.
    fn put(c: &Connection, src: SourceId, found: &[(u32, RomRef, Confidence)]) -> usize {
        let change = diff(&stored(c, src).expect("stored"), &[], found);
        apply(c, src, &change).expect("apply");
        change.add.len()
    }

    #[test]
    fn known_confidences_are_static() {
        assert_eq!(known("fuzzy"), Some("fuzzy"));
        assert_eq!(known(PROVEN), Some(PROVEN));
        assert_eq!(known("other"), None);
    }

    #[test]
    fn candidates_are_stored_ranked_listed_and_dropped() {
        let c = conn();
        let a = seed_rom(&c, "nes", "Nova Quest (World).nes", 16, "[]").expect("rom");
        let b = seed_rom(&c, "nes", "Nova Quest (World) (Alt).nes", 16, "[]").expect("rom");
        let src = source(&c, "0a", SourceState::Bound);
        let found = [
            (0, RomRef(b), Confidence::Size),
            (0, RomRef(a), Confidence::Size),
            (0, RomRef(a), Confidence::Fuzzy),
            (1, RomRef(a), Confidence::Unmatched),
        ];
        assert_eq!(put(&c, src, &found), 2);
        assert_eq!(put(&c, src, &found), 0, "unchanged");
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
        let named = of_files(&c, src, 0, 10).expect("files");
        assert_eq!(named.len(), 2);
        assert_eq!(named[0].1.rom_name, "Nova Quest (World).nes");

        crate::db::downloads_import::insert_fixture(&c, a, src, 0, "bad", None).expect("bad");
        assert_eq!(for_group(&c, ta).expect("group").len(), 1, "ruled out");
        let change = diff(&stored(&c, src).expect("stored"), &[], &found);
        assert_eq!(change.remove, [(0, a)], "a ruled-out pair is removed");
        apply(&c, src, &change).expect("apply");
        drop_pair(&c, src, 0, b).expect("drop");
        assert!(of_file(&c, src, 0).expect("of").is_empty());

        sources::set_state(&c, src, SourceState::Disabled, None).expect("disable");
        put(&c, src, &[(0, RomRef(b), Confidence::Size)]);
        assert!(for_group(&c, ta).expect("group").is_empty(), "bound only");
        clear(&c, src).expect("clear");
        assert!(of_file(&c, src, 0).expect("of").is_empty());
    }

    #[test]
    fn a_mapped_or_proven_pair_is_not_a_candidate_and_files_cascade() {
        let c = conn();
        let a = seed_rom(&c, "nes", "Nova Quest (World).nes", 16, "[]").expect("rom");
        let b = seed_rom(&c, "nes", "Nova Quest (World) (Alt).nes", 16, "[]").expect("rom");
        let src = source(&c, "0b", SourceState::Bound);
        let matches = [(0, Some(RomRef(a)), Confidence::Name)];
        let found = [
            (0, RomRef(a), Confidence::Fuzzy),
            (0, RomRef(b), Confidence::Fuzzy),
        ];
        let change = diff(&stored(&c, src).expect("stored"), &matches, &found);
        assert_eq!(change.matches, [(0, Some(a), Some("name"))]);
        assert_eq!(change.add, [(0, b, "fuzzy")]);
        let pieces = change.clone().split(1);
        assert_eq!(pieces.len(), 2);
        assert!(pieces.iter().all(|p| !p.is_empty()));
        apply(&c, src, &change).expect("apply");
        assert!(diff(&stored(&c, src).expect("stored"), &matches, &found).is_empty());

        prove(&c, src, 0, b).expect("prove");
        let proven = stored(&c, src).expect("stored");
        assert_eq!(proven.matches[&0], (Some(b), Some(PROVEN)));
        assert!(proven.candidates.is_empty());
        assert!(
            diff(&proven, &matches, &found).is_empty(),
            "a proven file keeps its rom"
        );

        drop_pair(&c, src, 0, b).expect("drop");
        assert_eq!(
            sources::files(&c, src, 10, 0).expect("files").0[0].rom_id,
            None
        );
        put(&c, src, &[(0, RomRef(a), Confidence::Fuzzy)]);
        sources::replace_files(&c, src, &[]).expect("empty");
        assert!(of_file(&c, src, 0).expect("of").is_empty(), "cascaded");
    }

    #[test]
    fn size_index_reads_live_roms_of_one_platform_with_their_group() {
        let c = conn();
        let a = seed_rom(&c, "nes", "Nova Quest (World).nes", 16, "[]").expect("rom");
        seed_rom(&c, "nes", "Boot (World).nes", 16, r#"["bios"]"#).expect("bios");
        seed_rom(&c, "snes", "Nova Quest (World).sfc", 16, "[]").expect("snes");
        seed_rom(&c, "nes", "Other (World).nes", 8, "[]").expect("other");
        sources::refresh_match_keys(&c).expect("keys");
        let nes = PlatformId("nes".into());
        let index = SqlSizeIndex::new(&c, &nes);
        let group = title_of(&c, a).0;
        assert_eq!(
            index.roms_of_size(16),
            [SizedRom {
                rom: RomRef(a),
                base: "nova quest".to_owned(),
                group
            }]
        );
        assert!(index.roms_of_size(u64::MAX).is_empty());
        assert_eq!(index.roms_of_size(32).len(), 1, "an iNES header on top");
        assert_eq!(header_len(&PlatformId("snes".into())), 512);
        assert!(rank("x").contains("WHEN 'fuzzy' THEN 3"));
        assert!(tier("x").contains("'base'"));
        assert!(not_bad("1", "2", "3").contains("b.rom_id = 1"));
        let before = rom_stamp(&c, &nes).expect("stamp");
        seed_rom(&c, "nes", "New (World).nes", 8, "[]").expect("new");
        assert_ne!(rom_stamp(&c, &nes).expect("stamp"), before);
    }
}
