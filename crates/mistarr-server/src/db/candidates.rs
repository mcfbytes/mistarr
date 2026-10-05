//! The `torrent_candidates` table and the availability read that joins it
//! with `torrent_files`; see `docs/DATA-MODEL.md` and `docs/VERIFICATION.md`.

use std::collections::{BTreeMap, HashSet};

use mistarr_core::PlatformId;
use mistarr_sources::binding::Confidence;
use mistarr_sources::fuzzy::{SizeIndex, SizedRom};
use rusqlite::{params, Connection};
use serde::Serialize;

use super::ids::{SourceId, TitleId};

use super::sql::{self, text_enum};
use crate::error::Result;
use mistarr_core::RomId;

text_enum! {
    /// `torrent_files.confidence` and `torrent_candidates.confidence`: how a file was
    /// paired with a rom, strongest first, so the derived order ranks them.
    #[derive(PartialOrd, Ord)]
    pub enum MatchConfidence {
        /// The file's hashes proved it; only `torrent_files` holds it.
        Hash = "hash",
        /// Exact or normalized name.
        Name = "name",
        /// Base name and size.
        Base = "base",
        /// Size, and names sharing their significant words.
        Fuzzy = "fuzzy",
        /// Size alone, under the narrow size-only rule.
        Size = "size",
    }
}

impl MatchConfidence {
    /// A hash and the name tiers: confidences a download may act on without a guess.
    pub const FIRM: [Self; 3] = [Self::Hash, Self::Name, Self::Base];
    /// [`MatchConfidence::FIRM`] as an SQL list.
    pub const FIRM_SQL: &'static str = "('hash', 'name', 'base')";

    /// The fuzzy and size-only tiers: confidences that are guesses.
    pub const GUESSED: [Self; 2] = [Self::Fuzzy, Self::Size];
    /// [`MatchConfidence::GUESSED`] as an SQL list.
    pub const GUESSED_SQL: &'static str = "('fuzzy', 'size')";

    /// The stored confidence of a binding match; `None` for an unmatched file.
    ///
    /// ```
    /// use mistarr_server::db::candidates::MatchConfidence;
    /// use mistarr_sources::binding::Confidence;
    /// assert_eq!(MatchConfidence::of(Confidence::Base), Some(MatchConfidence::Base));
    /// assert_eq!(MatchConfidence::of(Confidence::Unmatched), None);
    /// ```
    #[must_use]
    pub fn of(c: Confidence) -> Option<Self> {
        match c {
            Confidence::Name => Some(Self::Name),
            Confidence::Base => Some(Self::Base),
            Confidence::Fuzzy => Some(Self::Fuzzy),
            Confidence::Size => Some(Self::Size),
            _ => None,
        }
    }
}

/// Orders a `confidence` column from strongest to weakest: hash, name, base, fuzzy, size.
pub(crate) const RANK: &str = "CASE {c} WHEN 'hash' THEN 0 WHEN 'name' THEN 1 WHEN 'base' THEN 2
    WHEN 'fuzzy' THEN 3 WHEN 'size' THEN 4 ELSE 5 END";

/// 0 when the column `column` holds a [`MatchConfidence::FIRM`] confidence, 1 otherwise.
pub(crate) fn tier(column: &str) -> String {
    format!(
        "CASE WHEN {column} IN {} THEN 0 ELSE 1 END",
        MatchConfidence::FIRM_SQL
    )
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

/// What a mapping of a source must respect, beyond its per-file matches,
/// which [`diff_matches`] reads from the database as it goes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stored {
    /// The files a hash proved, in index order.
    pub proven: Vec<u32>,
    /// Whether any file has a matched rom.
    pub matched: bool,
    /// Each `(file, rom)` candidate and its confidence.
    pub candidates: BTreeMap<(u32, RomId), MatchConfidence>,
    /// `(file, rom)` pairs a `bad` download used.
    pub bad: HashSet<(u32, RomId)>,
}

impl Stored {
    /// Whether nothing is stored: no match, candidate or `bad` pair.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.matched && self.proven.is_empty() && self.candidates.is_empty() && self.bad.is_empty()
    }

    /// Whether a hash proved file `index`.
    #[must_use]
    pub fn is_proven(&self, index: u32) -> bool {
        self.proven.binary_search(&index).is_ok()
    }
}

/// What is stored for `source`, without its per-file matches.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn stored(conn: &Connection, source: SourceId) -> Result<Stored> {
    let proven = conn
        .prepare_cached(
            "SELECT file_index FROM torrent_files WHERE source_id = ?1 AND confidence = 'hash'
             ORDER BY file_index",
        )?
        .query_map([source], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let matched = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM torrent_files WHERE source_id = ?1 AND rom_id IS NOT NULL)",
        [source],
        |r| r.get(0),
    )?;
    let mut out = Stored {
        proven,
        matched,
        ..Stored::default()
    };
    let mut stmt = conn.prepare_cached(
        "SELECT file_index, rom_id, confidence FROM torrent_candidates WHERE source_id = ?1",
    )?;
    let mut rows = stmt.query([source])?;
    while let Some(r) = rows.next()? {
        out.candidates.insert((r.get(0)?, r.get(1)?), r.get(2)?);
    }
    let mut stmt = conn.prepare_cached(
        "SELECT file_index, rom_id FROM downloads
         WHERE source_id = ?1 AND state = 'bad' AND file_index IS NOT NULL",
    )?;
    let mut rows = stmt.query([source])?;
    while let Some(r) = rows.next()? {
        out.bad.insert((r.get(0)?, r.get(1)?));
    }
    Ok(out)
}

/// A `torrent_files` row to write: file index, rom and confidence.
pub type MatchRow = (u32, Option<RomId>, Option<MatchConfidence>);

/// The writes that turn a stored mapping into a new one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Change {
    /// `torrent_files` rows whose rom or confidence changes.
    pub matches: Vec<MatchRow>,
    /// Candidates to remove.
    pub remove: Vec<(u32, RomId)>,
    /// Candidates to add.
    pub add: Vec<(u32, RomId, MatchConfidence)>,
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

/// The [`Change`] from what `source` stores to the mapping of `matches`
/// (one per file, in file index order) and `found` (further candidates):
/// [`diff_matches`] and [`diff_candidates`] together.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn diff(
    conn: &Connection,
    source: SourceId,
    stored: &Stored,
    matches: &[(u32, Option<RomId>, Confidence)],
    found: &[(u32, RomId, Confidence)],
) -> Result<Change> {
    Ok(Change {
        matches: diff_matches(conn, source, matches)?,
        ..diff_candidates(stored, matches, found)
    })
}

/// The `torrent_files` rows of `source` whose rom or confidence differs from
/// `matches` (in file index order; a file absent from it is unmatched), read
/// row by row so no copy of the stored matches is held; a proven row is kept.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn diff_matches(
    conn: &Connection,
    source: SourceId,
    matches: &[(u32, Option<RomId>, Confidence)],
) -> Result<Vec<MatchRow>> {
    let mut stmt = conn.prepare_cached(
        "SELECT file_index, rom_id, confidence FROM torrent_files WHERE source_id = ?1
         ORDER BY file_index",
    )?;
    let sorted;
    let matches = if matches.is_sorted_by_key(|m| m.0) {
        matches
    } else {
        sorted = {
            let mut copy = matches.to_vec();
            copy.sort_by_key(|m| m.0);
            copy
        };
        &sorted
    };
    let mut rows = stmt.query([source])?;
    let mut out = Vec::new();
    while let Some(r) = rows.next()? {
        let index: u32 = r.get(0)?;
        let now: (Option<RomId>, Option<MatchConfidence>) = (r.get(1)?, r.get(2)?);
        if now.1 == Some(MatchConfidence::Hash) {
            continue;
        }
        let new = matches
            .binary_search_by_key(&index, |m| m.0)
            .map_or((None, None), |at| {
                let (_, rom, confidence) = matches[at];
                (rom, MatchConfidence::of(confidence))
            });
        if now != new {
            out.push((index, new.0, new.1));
        }
    }
    Ok(out)
}

/// The candidates to remove and add to turn `stored` into `found`, as a
/// [`Change`] with no `matches`. A file
/// a hash proved gets none; a pair a `bad` download ruled out, or equal to
/// its file's entry in `matches` (in file index order, possibly empty), is
/// left out; a pair found twice keeps its strongest confidence.
#[must_use]
pub fn diff_candidates(
    stored: &Stored,
    matches: &[(u32, Option<RomId>, Confidence)],
    found: &[(u32, RomId, Confidence)],
) -> Change {
    let proven = |i: u32| stored.is_proven(i);
    let own = |i: u32| {
        matches
            .binary_search_by_key(&i, |m| m.0)
            .ok()
            .and_then(|at| matches[at].1)
    };
    let mut wanted: BTreeMap<(u32, RomId), MatchConfidence> = BTreeMap::new();
    for (i, rom, confidence) in found {
        let Some(confidence) = MatchConfidence::of(*confidence) else {
            continue;
        };
        let pair = (*i, *rom);
        if !proven(*i) && !stored.bad.contains(&pair) && own(*i) != Some(*rom) {
            let slot = wanted.entry(pair).or_insert(confidence);
            *slot = (*slot).min(confidence);
        }
    }
    let mut change = Change::default();
    for (pair, confidence) in &stored.candidates {
        if wanted.get(pair) != Some(confidence) {
            change.remove.push(*pair);
        }
    }
    for ((i, rom), confidence) in wanted {
        if stored.candidates.get(&(i, rom)) != Some(&confidence) {
            change.add.push((i, rom, confidence));
        }
    }
    change
}

/// Writes `change` for `source`. A row a hash proved is never overwritten,
/// and no candidate is added to a proven file or for a pair a `bad` download
/// ruled out, so a proof written while a re-map was worked out survives it.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn apply(conn: &Connection, source: SourceId, change: &Change) -> Result<()> {
    let mut update = conn.prepare_cached(
        "UPDATE torrent_files SET rom_id = ?3, confidence = ?4
         WHERE source_id = ?1 AND file_index = ?2 AND confidence IS NOT 'hash'",
    )?;
    for (i, rom, confidence) in &change.matches {
        update.execute(params![source, i, rom, confidence])?;
    }
    let mut remove = conn.prepare_cached(
        "DELETE FROM torrent_candidates WHERE source_id = ?1 AND file_index = ?2 AND rom_id = ?3",
    )?;
    for (i, rom) in &change.remove {
        remove.execute(params![source, i, rom])?;
    }
    let mut add = conn.prepare_cached(&format!(
        "INSERT OR REPLACE INTO torrent_candidates (source_id, file_index, rom_id, confidence)
         SELECT ?1, ?2, ?3, ?4 WHERE EXISTS (SELECT 1 FROM torrent_files
                                             WHERE source_id = ?1 AND file_index = ?2
                                               AND confidence IS NOT 'hash')
           AND {}",
        not_bad("?3", "?1", "?2")
    ))?;
    for (i, rom, confidence) in &change.add {
        add.execute(params![source, i, rom, confidence])?;
    }
    Ok(())
}

/// Forgets the hash proofs of `source` that name a rom outside the live
/// catalogue of `platform`, as when the source is bound to another platform.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn drop_foreign_proofs(
    conn: &Connection,
    source: SourceId,
    platform: &PlatformId,
) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE torrent_files SET rom_id = NULL, confidence = NULL
         WHERE source_id = ?1 AND confidence = 'hash' AND NOT EXISTS (
           SELECT 1 FROM roms r JOIN titles t ON t.id = r.title_id
           WHERE r.id = torrent_files.rom_id AND t.platform_id = ?2
             AND r.retired = 0 AND t.retired = 0)",
        params![source, platform.as_str()],
    )?)
}

/// Records that file `index` of `source` hashed to `rom`: its row names the
/// rom as [`MatchConfidence::Hash`] and its candidates are dropped.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn prove(conn: &Connection, source: SourceId, index: u32, rom: RomId) -> Result<()> {
    conn.execute(
        "DELETE FROM torrent_candidates WHERE source_id = ?1 AND file_index = ?2",
        params![source, index],
    )?;
    conn.execute(
        "UPDATE torrent_files SET rom_id = ?3, confidence = ?4
         WHERE source_id = ?1 AND file_index = ?2",
        params![source, index, rom, MatchConfidence::Hash],
    )?;
    Ok(())
}

/// A text that changes whenever the live roms of `platform` do, so a source
/// mapped against the same text needs no new mapping: its live DAT versions
/// with their load times, which catch a reload that updates roms in place,
/// the count of its live roms, and a sum of a hash of each one's id and effective group, which
/// moves when roms trade groups. MRA versions are left out, since a catalogue run touches
/// theirs every time; MRA roms change their ids when renamed.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn rom_stamp(conn: &Connection, platform: &PlatformId) -> Result<String> {
    let versions: String = conn.query_row(
        "SELECT COALESCE(group_concat(id || '@' || loaded_at, ','), '')
         FROM (SELECT id, loaded_at FROM dat_versions
               WHERE platform_id = ?1 AND retired = 0 AND source != 'mra' ORDER BY id)",
        [&platform.as_str()],
        |r| r.get(0),
    )?;
    let mut stmt = conn.prepare_cached(
        "SELECT r.id, COALESCE(t.group_root, t.id)
         FROM roms r JOIN titles t ON t.id = r.title_id
         WHERE t.platform_id = ?1 AND r.retired = 0 AND t.retired = 0",
    )?;
    let mut rows = stmt.query([&platform.as_str()])?;
    let (mut count, mut sum) = (0u64, 0u64);
    while let Some(r) = rows.next()? {
        let (rom, group): (RomId, TitleId) = (r.get(0)?, r.get(1)?);
        count += 1;
        sum = sum.wrapping_add(mix(
            mix(rom.get().cast_unsigned()) ^ group.get().cast_unsigned()
        ));
    }
    Ok(format!("{versions};{count}:{sum:016x}"))
}

/// The `splitmix64` finaliser: a fixed bijection of `u64` that spreads every input bit.
fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
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
        .query_map(params![source, from, to], |r| {
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
    pub rom_id: RomId,
    /// Its DAT name.
    pub rom_name: String,
    /// Its title.
    pub title_id: TitleId,
    /// `name`, `base`, `fuzzy` or `size`.
    pub confidence: MatchConfidence,
}

/// Removes every candidate of the source.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn clear(conn: &Connection, source: SourceId) -> Result<()> {
    conn.execute(
        "DELETE FROM torrent_candidates WHERE source_id = ?1",
        [source],
    )?;
    Ok(())
}

/// Forgets that file `index` of `source` may be `rom`, as a candidate and as
/// its `torrent_files` match, once its hashes showed it is another rom.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn drop_pair(conn: &Connection, source: SourceId, index: u32, rom: RomId) -> Result<()> {
    conn.execute(
        "DELETE FROM torrent_candidates WHERE source_id = ?1 AND file_index = ?2 AND rom_id = ?3",
        params![source, index, rom],
    )?;
    conn.execute(
        "UPDATE torrent_files SET rom_id = NULL, confidence = NULL
         WHERE source_id = ?1 AND file_index = ?2 AND rom_id = ?3",
        params![source, index, rom],
    )?;
    Ok(())
}

/// The candidates of one file, strongest first, as `(rom_id, confidence)`.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn of_file(
    conn: &Connection,
    source: SourceId,
    index: u32,
) -> Result<Vec<(RomId, MatchConfidence)>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT rom_id, confidence FROM torrent_candidates
         WHERE source_id = ?1 AND file_index = ?2 ORDER BY {}, rom_id",
        rank("confidence")
    ))?;
    let rows = stmt
        .query_map(params![source, index], |r| Ok((r.get(0)?, r.get(1)?)))?
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
    pub rom_id: RomId,
    /// How the file was paired with the rom; `None` for a file mapped before
    /// confidences were stored.
    pub confidence: Option<MatchConfidence>,
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
    // Unary `+` keeps `sources_state` out, so the plan starts from the group, not every source.
    let sql = format!(
        "SELECT title_id, source_id, display_name, file_index, path, rom_id, confidence FROM (
           SELECT t.id AS title_id, tf.source_id, s.display_name, tf.file_index, tf.path,
                  r.id AS rom_id, tf.confidence
           {group}
           JOIN torrent_files tf ON tf.rom_id = r.id
           JOIN sources s ON s.id = tf.source_id AND +s.state = 'bound'
           WHERE (t.group_root = ?1 OR (t.id = ?1 AND t.group_root IS NULL)) AND {bad_tf}
           UNION ALL
           SELECT t.id, c.source_id, s.display_name, c.file_index, tf.path, r.id, c.confidence
           {group}
           JOIN torrent_candidates c ON c.rom_id = r.id
           JOIN torrent_files tf ON tf.source_id = c.source_id AND tf.file_index = c.file_index
           JOIN sources s ON s.id = c.source_id AND +s.state = 'bound'
           WHERE (t.group_root = ?1 OR (t.id = ?1 AND t.group_root IS NULL)) AND {bad_c}
         )
         ORDER BY title_id, {rank}, source_id, file_index, rom_id",
        bad_tf = not_bad("r.id", "tf.source_id", "tf.file_index"),
        bad_c = not_bad("r.id", "c.source_id", "c.file_index"),
        rank = rank("confidence"),
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([parent], |r| {
            Ok((
                r.get(0)?,
                Availability {
                    source_id: r.get(1)?,
                    source_name: r.get(2)?,
                    file_index: r.get(3)?,
                    path: r.get(4)?,
                    rom_id: r.get(5)?,
                    confidence: r.get(6)?,
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
/// assert_eq!(header_len(&PlatformId::new("nes")), 16);
/// assert_eq!(header_len(&PlatformId::new("gba")), 0);
/// ```
#[must_use]
pub fn header_len(platform: &PlatformId) -> u64 {
    mistarr_mister::platforms::by_id(platform.as_str()).map_or(0, |p| p.header_rule.header_len())
}

impl SizeIndex for SqlSizeIndex<'_> {
    fn roms_of_size(&self, size: u64) -> Vec<SizedRom> {
        let run = || -> rusqlite::Result<Vec<SizedRom>> {
            let mut stmt = self.conn.prepare_cached(
                "SELECT r.id, r.match_base, COALESCE(t.group_root, t.parent_id, t.id)
                 FROM roms r JOIN titles t ON t.id = r.title_id
                 WHERE r.size IN (?2, ?3) AND t.platform_id = ?1 AND r.retired = 0 AND t.retired = 0
                   AND r.match_base IS NOT NULL
                   AND NOT EXISTS (SELECT 1 FROM title_flags f
                                   WHERE f.title_id = t.id AND f.flag = 'bios')
                 ORDER BY r.id",
            )?;
            let bare = size.checked_sub(self.header).filter(|_| self.header > 0);
            let size = sql::to_i64(size);
            let bare = bare.map_or(size, sql::to_i64);

            let rows = stmt.query_map(params![self.platform.as_str(), size, bare], |r| {
                Ok(SizedRom {
                    rom: RomId::new(r.get(0)?),
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
    use crate::db::fixtures::conn;
    use crate::db::fixtures::{pid, seed_rom};
    use crate::db::sources::{self, NewSource, SourceState};

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
        sources::set_binding(c, id, Some(&PlatformId::new("nes")), Some(0.0)).expect("bind");
        id
    }

    fn title_of(c: &Connection, rom: RomId) -> TitleId {
        c.query_row("SELECT title_id FROM roms WHERE id = ?1", [rom], |r| {
            r.get(0)
        })
        .expect("title")
    }

    /// Replaces the candidates of `src` with `found`, as a mapping does.
    fn put(c: &Connection, src: SourceId, found: &[(u32, RomId, Confidence)]) -> usize {
        let change = diff(c, src, &stored(c, src).expect("stored"), &[], found).expect("diff");
        apply(c, src, &change).expect("apply");
        change.add.len()
    }

    #[test]
    fn a_proof_survives_a_stale_change_and_foreign_proofs_drop() {
        let c = conn();
        let a = seed_rom(&c, &pid("nes"), "Nova Quest (World).nes", 16, &[]).expect("rom");
        let b = seed_rom(&c, &pid("nes"), "Nova Quest (World) (Alt).nes", 16, &[]).expect("rom");
        let src = source(&c, "0c", SourceState::Bound);
        assert!(stored(&c, src).expect("stored").is_empty());
        let stale = diff(
            &c,
            src,
            &stored(&c, src).expect("stored"),
            &[(0, Some(a), Confidence::Name)],
            &[(0, b, Confidence::Fuzzy), (1, a, Confidence::Size)],
        )
        .expect("diff");
        assert_eq!(
            diff_matches(&c, src, &[(0, None, Confidence::Unmatched)]).expect("diff"),
            []
        );
        assert_eq!(
            diff_candidates(&Stored::default(), &[], &[]),
            Change::default()
        );
        let unsorted = [
            (1, None, Confidence::Unmatched),
            (0, Some(a), Confidence::Name),
        ];
        assert_eq!(
            diff_matches(&c, src, &unsorted).expect("diff"),
            [(0, Some(a), Some(MatchConfidence::Name))]
        );
        prove(&c, src, 0, b).expect("prove");
        crate::db::fixtures::download(
            &c,
            a,
            src,
            1,
            crate::db::downloads::DownloadState::Bad,
            None,
        )
        .expect("bad");
        apply(&c, src, &stale).expect("apply");
        let now = stored(&c, src).expect("stored");
        assert!(!now.is_empty());
        assert_eq!(now.proven, [0], "the proof stays");
        assert!(now.is_proven(0) && !now.is_proven(1));
        assert!(
            now.candidates.is_empty(),
            "no guess on a proven or ruled-out file"
        );
        let snes = PlatformId::new("snes");
        assert_eq!(
            drop_foreign_proofs(&c, src, &PlatformId::new("nes")).expect("nes"),
            0
        );
        assert_eq!(drop_foreign_proofs(&c, src, &snes).expect("snes"), 1);
        assert!(stored(&c, src).expect("stored").proven.is_empty());
    }

    #[test]
    fn confidences_rank_strongest_first() {
        assert_eq!(
            MatchConfidence::of(Confidence::Fuzzy),
            Some(MatchConfidence::Fuzzy)
        );
        assert!(MatchConfidence::Hash < MatchConfidence::Name);
        assert!(MatchConfidence::Fuzzy < MatchConfidence::Size);
        assert_eq!(MatchConfidence::parse("hash"), Some(MatchConfidence::Hash));
        assert_eq!(MatchConfidence::parse("other"), None);
    }

    #[test]
    fn confidence_sets_match_their_sql() {
        let text = |set: &[MatchConfidence]| {
            sql::text_list(&set.iter().map(|c| c.as_str()).collect::<Vec<_>>())
        };
        assert_eq!(MatchConfidence::FIRM_SQL, text(&MatchConfidence::FIRM));
        assert_eq!(
            MatchConfidence::GUESSED_SQL,
            text(&MatchConfidence::GUESSED)
        );
        for c in MatchConfidence::ALL {
            assert_ne!(
                MatchConfidence::FIRM.contains(c),
                MatchConfidence::GUESSED.contains(c),
                "{c}"
            );
        }
    }

    #[test]
    fn candidates_are_stored_ranked_listed_and_dropped() {
        let c = conn();
        let a = seed_rom(&c, &pid("nes"), "Nova Quest (World).nes", 16, &[]).expect("rom");
        let b = seed_rom(&c, &pid("nes"), "Nova Quest (World) (Alt).nes", 16, &[]).expect("rom");
        let src = source(&c, "0a", SourceState::Bound);
        let found = [
            (0, b, Confidence::Size),
            (0, a, Confidence::Size),
            (0, a, Confidence::Fuzzy),
            (1, a, Confidence::Unmatched),
        ];
        assert_eq!(put(&c, src, &found), 2);
        assert_eq!(put(&c, src, &found), 0, "unchanged");
        assert_eq!(
            of_file(&c, src, 0).expect("of"),
            [(a, MatchConfidence::Fuzzy), (b, MatchConfidence::Size)]
        );
        let (ta, tb) = (title_of(&c, a), title_of(&c, b));
        c.execute("UPDATE titles SET group_root = ?1", [ta])
            .expect("group");
        let listed = for_group(&c, ta).expect("group");
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].0, ta);
        assert_eq!(listed[0].1.path, "Set/nova.nes");
        assert_eq!(listed[0].1.source_name, "Synthetic Set");
        assert_eq!(
            (listed[1].0, listed[1].1.confidence),
            (tb, Some(MatchConfidence::Size))
        );
        let named = of_files(&c, src, 0, 10).expect("files");
        assert_eq!(named.len(), 2);
        assert_eq!(named[0].1.rom_name, "Nova Quest (World).nes");

        crate::db::fixtures::download(
            &c,
            a,
            src,
            0,
            crate::db::downloads::DownloadState::Bad,
            None,
        )
        .expect("bad");
        assert_eq!(for_group(&c, ta).expect("group").len(), 1, "ruled out");
        let change = diff(&c, src, &stored(&c, src).expect("stored"), &[], &found).expect("diff");
        assert_eq!(change.remove, [(0, a)], "a ruled-out pair is removed");
        apply(&c, src, &change).expect("apply");
        drop_pair(&c, src, 0, b).expect("drop");
        assert!(of_file(&c, src, 0).expect("of").is_empty());

        sources::set_state(&c, src, SourceState::Disabled, None).expect("disable");
        put(&c, src, &[(0, b, Confidence::Size)]);
        assert!(for_group(&c, ta).expect("group").is_empty(), "bound only");
        clear(&c, src).expect("clear");
        assert!(of_file(&c, src, 0).expect("of").is_empty());
    }

    #[test]
    fn a_mapped_or_proven_pair_is_not_a_candidate_and_files_cascade() {
        let c = conn();
        let a = seed_rom(&c, &pid("nes"), "Nova Quest (World).nes", 16, &[]).expect("rom");
        let b = seed_rom(&c, &pid("nes"), "Nova Quest (World) (Alt).nes", 16, &[]).expect("rom");
        let src = source(&c, "0b", SourceState::Bound);
        let matches = [(0, Some(a), Confidence::Name)];
        let found = [(0, a, Confidence::Fuzzy), (0, b, Confidence::Fuzzy)];
        let change =
            diff(&c, src, &stored(&c, src).expect("stored"), &matches, &found).expect("diff");
        assert_eq!(change.matches, [(0, Some(a), Some(MatchConfidence::Name))]);
        assert_eq!(change.add, [(0, b, MatchConfidence::Fuzzy)]);
        let pieces = change.clone().split(1);
        assert_eq!(pieces.len(), 2);
        assert!(pieces.iter().all(|p| !p.is_empty()));
        apply(&c, src, &change).expect("apply");
        assert!(
            diff(&c, src, &stored(&c, src).expect("stored"), &matches, &found)
                .expect("diff")
                .is_empty()
        );

        prove(&c, src, 0, b).expect("prove");
        let proven = stored(&c, src).expect("stored");
        assert_eq!(proven.proven, [0]);
        assert!(proven.candidates.is_empty());
        assert!(
            diff(&c, src, &proven, &matches, &found)
                .expect("diff")
                .is_empty(),
            "a proven file keeps its rom"
        );

        drop_pair(&c, src, 0, b).expect("drop");
        assert_eq!(
            crate::db::views::source_detail::files(
                &c,
                src,
                &crate::db::views::source_detail::FileQuery::default(),
                crate::db::sql::Page {
                    limit: 10,
                    offset: 0
                }
            )
            .expect("files")
            .items[0]
                .rom_id,
            None
        );
        put(&c, src, &[(0, a, Confidence::Fuzzy)]);
        sources::replace_files(&c, src, &[]).expect("empty");
        assert!(of_file(&c, src, 0).expect("of").is_empty(), "cascaded");
    }

    #[test]
    fn size_index_reads_live_roms_of_one_platform_with_their_group() {
        let c = conn();
        let a = seed_rom(&c, &pid("nes"), "Nova Quest (World).nes", 16, &[]).expect("rom");
        seed_rom(&c, &pid("nes"), "Boot (World).nes", 16, &["bios"]).expect("bios");
        seed_rom(&c, &pid("snes"), "Nova Quest (World).sfc", 16, &[]).expect("snes");
        let other = seed_rom(&c, &pid("nes"), "Other (World).nes", 8, &[]).expect("other");
        sources::refresh_match_keys(&c).expect("keys");
        let nes = PlatformId::new("nes");
        let index = SqlSizeIndex::new(&c, &nes);
        let group = title_of(&c, a).get();
        assert_eq!(
            index.roms_of_size(16),
            [SizedRom {
                rom: a,
                base: "nova quest".to_owned(),
                group
            }]
        );
        assert!(index.roms_of_size(u64::MAX).is_empty());
        assert_eq!(index.roms_of_size(32).len(), 1, "an iNES header on top");
        let root = title_of(&c, other).get();
        c.execute(
            "UPDATE titles SET group_root = ?1 WHERE id = ?2",
            [root, group],
        )
        .expect("link");
        assert_eq!(index.roms_of_size(16)[0].group, root, "the effective group");
        assert_eq!(header_len(&PlatformId::new("snes")), 512);
        assert!(rank("x").contains("WHEN 'fuzzy' THEN 3"));
        assert!(tier("x").contains("'base'"));
        assert!(not_bad("1", "2", "3").contains("b.rom_id = 1"));
        let before = rom_stamp(&c, &nes).expect("stamp");
        seed_rom(&c, &pid("nes"), "New (World).nes", 8, &[]).expect("new");
        assert_ne!(rom_stamp(&c, &nes).expect("stamp"), before);
        let before = rom_stamp(&c, &nes).expect("stamp");
        c.execute("UPDATE dat_versions SET loaded_at = loaded_at + 1", [])
            .expect("reload");
        assert_ne!(
            rom_stamp(&c, &nes).expect("stamp"),
            before,
            "a reload that updates roms in place moves the stamp"
        );
        let before = rom_stamp(&c, &nes).expect("stamp");
        c.execute(
            "UPDATE titles SET group_root = ?1 WHERE id = ?2",
            [group, root],
        )
        .expect("regroup");
        assert_ne!(
            rom_stamp(&c, &nes).expect("stamp"),
            before,
            "a regroup moves it"
        );
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig {
            cases: 64,
            failure_persistence: None,
            ..proptest::prelude::ProptestConfig::default()
        })]

        /// Two titles trading effective groups move the stamp, whatever their rom counts.
        #[test]
        fn two_titles_swapping_groups_move_the_stamp(
            roms in proptest::collection::vec(1usize..4, 2..7),
            roots in proptest::collection::vec(0usize..7, 7),
            a in 0usize..7,
            b in 0usize..7,
        ) {
            let c = conn();
            let n = roms.len();
            let id = |i: usize| i64::try_from(i + 1).expect("id");
            c.execute(
                "INSERT INTO dat_versions (id, platform_id, dat_name, version, source_file, loaded_at, game_count)
                 VALUES (1, 'nes', 'Test', '1', 't.dat', 0, 0)",
                [],
            ).expect("version");
            for i in 0..n {
                c.execute(
                    "INSERT INTO titles (id, platform_id, dat_version_id, name, base_name)
                     VALUES (?1, 'nes', 1, 'T' || ?1, 'T')",
                    [id(i)],
                ).expect("title");
            }
            for (i, &count) in roms.iter().enumerate() {
                c.execute(
                    "UPDATE titles SET group_root = ?2 WHERE id = ?1",
                    [id(i), id(roots[i] % n)],
                ).expect("root");
                for k in 0..count {
                    c.execute(
                        "INSERT INTO roms (title_id, name, size) VALUES (?1, ?2, 4)",
                        params![id(i), format!("r{k}")],
                    ).expect("rom");
                }
            }
            let (a, b) = (id(a % n), id(b % n));
            let root = |id: i64| -> i64 {
                c.query_row("SELECT group_root FROM titles WHERE id = ?1", [id], |r| r.get(0))
                    .expect("root")
            };
            let (ra, rb) = (root(a), root(b));
            proptest::prop_assume!(ra != rb);
            let nes = PlatformId::new("nes");
            let before = rom_stamp(&c, &nes).expect("stamp");
            c.execute("UPDATE titles SET group_root = ?2 WHERE id = ?1", params![a, rb])
                .expect("swap");
            c.execute("UPDATE titles SET group_root = ?2 WHERE id = ?1", params![b, ra])
                .expect("swap");
            proptest::prop_assert_ne!(rom_stamp(&c, &nes).expect("stamp"), before);
        }
    }
}
