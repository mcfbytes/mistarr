//! The `sources` and `torrent_files` tables, and the [`DatIndex`] binding reads roms through.

use std::fmt;

use mistarr_clients::SeedPolicy;
use mistarr_core::PlatformId;
use mistarr_sources::binding::{self, Confidence, DatIndex, RomRef};
use mistarr_sources::torrent::TorrentFile;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use crate::error::Result;

/// Roms given match keys per statement batch in [`refresh_match_keys`].
const KEY_BATCH: u32 = 1000;

/// A `sources.id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SourceId(pub i64);

impl fmt::Display for SourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// `sources.state`; the machine is in `docs/DATA-MODEL.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceState {
    /// A magnet whose file list the client has not fetched yet.
    Resolving,
    /// File list known, no platform reached the threshold.
    Unbound,
    /// Attached to a platform.
    Bound,
    /// Turned off by the user.
    Disabled,
}

impl SourceState {
    /// The column value.
    ///
    /// ```
    /// assert_eq!(mistarr_server::db::sources::SourceState::Bound.as_str(), "bound");
    /// ```
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Resolving => "resolving",
            Self::Unbound => "unbound",
            Self::Bound => "bound",
            Self::Disabled => "disabled",
        }
    }

    /// Parses a column value.
    ///
    /// ```
    /// use mistarr_server::db::sources::SourceState;
    /// assert_eq!(SourceState::parse("unbound"), Some(SourceState::Unbound));
    /// assert_eq!(SourceState::parse("x"), None);
    /// ```
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        [Self::Resolving, Self::Unbound, Self::Bound, Self::Disabled]
            .into_iter()
            .find(|v| v.as_str() == s)
    }
}

/// The `sources.seed_policy` text: `none`, `client` or `ratio:N`.
///
/// ```
/// use mistarr_clients::SeedPolicy;
/// use mistarr_server::db::sources::seed_to_text;
/// assert_eq!(seed_to_text(&SeedPolicy::Ratio { ratio: 1.5 }), "ratio:1.5");
/// ```
#[must_use]
pub fn seed_to_text(policy: &SeedPolicy) -> String {
    match policy {
        SeedPolicy::Ratio { ratio } => format!("ratio:{ratio:?}"),
        SeedPolicy::Client => "client".to_owned(),
        SeedPolicy::None => "none".to_owned(),
    }
}

/// Parses the `sources.seed_policy` text; `None` unless the ratio is finite and positive.
///
/// ```
/// use mistarr_clients::SeedPolicy;
/// use mistarr_server::db::sources::seed_from_text;
/// assert_eq!(seed_from_text("ratio:2"), Some(SeedPolicy::Ratio { ratio: 2.0 }));
/// assert_eq!(seed_from_text("ratio:-1"), None);
/// ```
#[must_use]
pub fn seed_from_text(text: &str) -> Option<SeedPolicy> {
    match text {
        "none" => Some(SeedPolicy::None),
        "client" => Some(SeedPolicy::Client),
        other => {
            let ratio: f32 = other.strip_prefix("ratio:")?.parse().ok()?;
            (ratio.is_finite() && ratio > 0.0).then_some(SeedPolicy::Ratio { ratio })
        }
    }
}

/// The `torrent_files.confidence` text, `None` for an unmatched file.
///
/// ```
/// use mistarr_server::db::sources::confidence_text;
/// use mistarr_sources::binding::Confidence;
/// assert_eq!(confidence_text(Confidence::Size), Some("size"));
/// assert_eq!(confidence_text(Confidence::Unmatched), None);
/// ```
#[must_use]
pub fn confidence_text(c: Confidence) -> Option<&'static str> {
    match c {
        Confidence::Name => Some("name"),
        Confidence::Size => Some("size"),
        Confidence::Unmatched => None,
    }
}

/// One source with its file and match counts, as `GET /sources` lists it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SourceRow {
    /// Row id.
    pub id: SourceId,
    /// Lowercase hex v1 infohash.
    pub infohash: String,
    /// The torrent's name.
    pub display_name: String,
    /// Basename of the file as dropped.
    pub origin_file: String,
    /// Bound platform.
    pub platform_id: Option<PlatformId>,
    /// Hit rate of the bound platform.
    pub bind_score: Option<f64>,
    /// Lifecycle state.
    pub state: SourceState,
    /// Why the source is resolving or unbound, for the user.
    pub reason: Option<String>,
    /// `none`, `client` or `ratio:N`.
    pub seed_policy: String,
    /// Files in the torrent.
    pub file_count: u64,
    /// Files with a matched rom.
    pub matched_count: u64,
    /// Sum of file sizes.
    pub total_size: u64,
    /// Id in the download client once added.
    pub client_id: Option<String>,
    /// Unix seconds.
    pub added_at: i64,
    /// The platform the torrent's names point at, found without any DAT.
    pub suggested_platform_id: Option<PlatformId>,
}

/// A source to insert.
#[derive(Debug, Clone, PartialEq)]
pub struct NewSource<'a> {
    /// Lowercase hex infohash.
    pub infohash: &'a str,
    /// The torrent's name.
    pub display_name: &'a str,
    /// Basename of the file as dropped.
    pub origin_file: &'a str,
    /// Initial state.
    pub state: SourceState,
    /// Why it is in that state, if the user should know.
    pub reason: Option<&'a str>,
    /// Unix seconds.
    pub added_at: i64,
}

/// One `torrent_files` row with its matched rom's name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileRow {
    /// Index in the torrent.
    pub file_index: u32,
    /// Path inside the torrent.
    pub path: String,
    /// Size in bytes.
    pub size: u64,
    /// Matched rom.
    pub rom_id: Option<i64>,
    /// The matched rom's DAT name.
    pub rom_name: Option<String>,
    /// The matched rom's title.
    pub title_id: Option<i64>,
    /// `name` or `size`, `None` when unmatched.
    pub confidence: Option<String>,
}

const COLUMNS: &str = "s.id, s.infohash, s.display_name, s.origin_file, s.platform_id,
    s.bind_score, s.state, s.reason, s.seed_policy, s.file_count, s.total_size,
    s.client_id, s.added_at,
    (SELECT COUNT(*) FROM torrent_files f WHERE f.source_id = s.id AND f.rom_id IS NOT NULL),
    s.suggested_platform_id";

/// Reads a non-negative integer column; SQLite stores it as `i64`.
fn uint(r: &Row<'_>, i: usize) -> rusqlite::Result<u64> {
    Ok(u64::try_from(r.get::<_, i64>(i)?).unwrap_or(0))
}

/// A size or count as SQLite stores it.
fn sql_int(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

fn from_row(r: &Row<'_>) -> rusqlite::Result<SourceRow> {
    let state: String = r.get(6)?;
    Ok(SourceRow {
        id: SourceId(r.get(0)?),
        infohash: r.get(1)?,
        display_name: r.get(2)?,
        origin_file: r.get(3)?,
        platform_id: r.get::<_, Option<String>>(4)?.map(PlatformId),
        bind_score: r.get(5)?,
        state: SourceState::parse(&state).unwrap_or(SourceState::Disabled),
        reason: r.get(7)?,
        seed_policy: r.get(8)?,
        file_count: uint(r, 9)?,
        total_size: uint(r, 10)?,
        client_id: r.get(11)?,
        added_at: r.get(12)?,
        matched_count: uint(r, 13)?,
        suggested_platform_id: r.get::<_, Option<String>>(14)?.map(PlatformId),
    })
}

/// Inserts a source with no files.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure, including a duplicate infohash.
pub fn insert(conn: &Connection, s: &NewSource<'_>) -> Result<SourceId> {
    conn.execute(
        "INSERT INTO sources (infohash, display_name, origin_file, state, reason, added_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            s.infohash,
            s.display_name,
            s.origin_file,
            s.state.as_str(),
            s.reason,
            s.added_at
        ],
    )?;
    Ok(SourceId(conn.last_insert_rowid()))
}

/// Reads one source.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn get(conn: &Connection, id: SourceId) -> Result<Option<SourceRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM sources s WHERE s.id = ?1"),
            [id.0],
            from_row,
        )
        .optional()?)
}

/// The source with this infohash, if any.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn find_by_infohash(conn: &Connection, infohash: &str) -> Result<Option<SourceRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM sources s WHERE s.infohash = ?1"),
            [infohash],
            from_row,
        )
        .optional()?)
}

/// Sources in id order with the total before paging.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn list(conn: &Connection, limit: u32, offset: u32) -> Result<(Vec<SourceRow>, u64)> {
    let total = conn.query_row("SELECT COUNT(*) FROM sources", [], |r| uint(r, 0))?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM sources s ORDER BY s.id LIMIT ?1 OFFSET ?2"
    ))?;
    let rows = stmt
        .query_map(params![limit, offset], from_row)?
        .collect::<rusqlite::Result<_>>()?;
    Ok((rows, total))
}

/// Every source in `resolving`, each with whether the client already has it.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn list_resolving(conn: &Connection) -> Result<Vec<(SourceId, bool)>> {
    let mut stmt = conn.prepare(
        "SELECT id, client_id IS NOT NULL FROM sources WHERE state = 'resolving' ORDER BY id",
    )?;
    let ids = stmt
        .query_map([], |r| Ok((SourceId(r.get(0)?), r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(ids)
}

/// Sets state and reason together.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn set_state(
    conn: &Connection,
    id: SourceId,
    state: SourceState,
    reason: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE sources SET state = ?2, reason = ?3 WHERE id = ?1",
        params![id.0, state.as_str(), reason],
    )?;
    Ok(())
}

/// Stores the platform guessed from the torrent's names.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn set_suggestion(
    conn: &Connection,
    id: SourceId,
    platform: Option<&PlatformId>,
) -> Result<()> {
    conn.execute(
        "UPDATE sources SET suggested_platform_id = ?2 WHERE id = ?1",
        params![id.0, platform.map(|p| p.0.as_str())],
    )?;
    Ok(())
}

/// Unbound sources with their suggested platform, oldest first.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn list_unbound(conn: &Connection) -> Result<Vec<(SourceId, Option<PlatformId>)>> {
    let mut stmt = conn.prepare(
        "SELECT id, suggested_platform_id FROM sources WHERE state = 'unbound' ORDER BY id",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                SourceId(r.get(0)?),
                r.get::<_, Option<String>>(1)?.map(PlatformId),
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// True when `platform` has live titles from a DAT file.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn platform_has_dat(conn: &Connection, platform: &PlatformId) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM titles WHERE platform_id = ?1 AND retired = 0 AND source = 'dat' LIMIT 1",
            [&platform.0],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Sets the user-facing reason only.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn set_reason(conn: &Connection, id: SourceId, reason: Option<&str>) -> Result<()> {
    conn.execute(
        "UPDATE sources SET reason = ?2 WHERE id = ?1",
        params![id.0, reason],
    )?;
    Ok(())
}

/// Stores a seed policy.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn set_seed_policy(conn: &Connection, id: SourceId, policy: &SeedPolicy) -> Result<()> {
    conn.execute(
        "UPDATE sources SET seed_policy = ?2 WHERE id = ?1",
        params![id.0, seed_to_text(policy)],
    )?;
    Ok(())
}

/// Stores or clears the client's id for the torrent.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn set_client_id(conn: &Connection, id: SourceId, client_id: Option<&str>) -> Result<()> {
    conn.execute(
        "UPDATE sources SET client_id = ?2 WHERE id = ?1",
        params![id.0, client_id],
    )?;
    Ok(())
}

/// Sets the platform and the score that produced it; `None` unbinds.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn set_binding(
    conn: &Connection,
    id: SourceId,
    platform: Option<&PlatformId>,
    score: Option<f64>,
) -> Result<()> {
    conn.execute(
        "UPDATE sources SET platform_id = ?2, bind_score = ?3 WHERE id = ?1",
        params![id.0, platform.map(|p| p.0.as_str()), score],
    )?;
    Ok(())
}

/// Replaces the source's file list, clearing matches, and updates its counts.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn replace_files(conn: &Connection, id: SourceId, files: &[TorrentFile]) -> Result<()> {
    conn.execute("DELETE FROM torrent_files WHERE source_id = ?1", [id.0])?;
    let mut stmt = conn.prepare_cached(
        "INSERT INTO torrent_files (source_id, file_index, path, size) VALUES (?1, ?2, ?3, ?4)",
    )?;
    for f in files {
        stmt.execute(params![id.0, f.index, f.path, sql_int(f.size)])?;
    }
    let total: u64 = files.iter().map(|f| f.size).sum();
    conn.execute(
        "UPDATE sources SET file_count = ?2, total_size = ?3 WHERE id = ?1",
        params![
            id.0,
            i64::try_from(files.len()).unwrap_or(i64::MAX),
            sql_int(total)
        ],
    )?;
    Ok(())
}

/// Records each file's matched rom and confidence, as `match_files` returns them.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn set_matches(
    conn: &Connection,
    id: SourceId,
    matches: &[(u32, Option<RomRef>, Confidence)],
) -> Result<()> {
    let mut stmt = conn.prepare_cached(
        "UPDATE torrent_files SET rom_id = ?3, confidence = ?4
         WHERE source_id = ?1 AND file_index = ?2",
    )?;
    for (index, rom, confidence) in matches {
        stmt.execute(params![
            id.0,
            index,
            rom.map(|r| r.0),
            confidence_text(*confidence)
        ])?;
    }
    Ok(())
}

/// The stored file list, in index order, for binding again.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn torrent_files(conn: &Connection, id: SourceId) -> Result<Vec<TorrentFile>> {
    let mut stmt = conn.prepare(
        "SELECT file_index, path, size FROM torrent_files WHERE source_id = ?1 ORDER BY file_index",
    )?;
    let files = stmt
        .query_map([id.0], |r| {
            Ok(TorrentFile {
                index: r.get(0)?,
                path: r.get(1)?,
                size: uint(r, 2)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(files)
}

/// A page of the source's files with matched rom names, and the total.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn files(
    conn: &Connection,
    id: SourceId,
    limit: u32,
    offset: u32,
) -> Result<(Vec<FileRow>, u64)> {
    let total = conn.query_row(
        "SELECT COUNT(*) FROM torrent_files WHERE source_id = ?1",
        [id.0],
        |r| uint(r, 0),
    )?;
    let mut stmt = conn.prepare(
        "SELECT f.file_index, f.path, f.size, f.rom_id, r.name, r.title_id, f.confidence
         FROM torrent_files f LEFT JOIN roms r ON r.id = f.rom_id
         WHERE f.source_id = ?1 ORDER BY f.file_index LIMIT ?2 OFFSET ?3",
    )?;
    let rows = stmt
        .query_map(params![id.0, limit, offset], |r| {
            Ok(FileRow {
                file_index: r.get(0)?,
                path: r.get(1)?,
                size: uint(r, 2)?,
                rom_id: r.get(3)?,
                rom_name: r.get(4)?,
                title_id: r.get(5)?,
                confidence: r.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok((rows, total))
}

/// Deletes a source and, by cascade, its files. Its downloads keep their rows
/// with `source_id` NULL. Returns whether it existed.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn delete(conn: &Connection, id: SourceId) -> Result<bool> {
    Ok(conn.execute("DELETE FROM sources WHERE id = ?1", [id.0])? > 0)
}

/// Downloads of the source that are queued, transferring, checking or importing.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn open_download_count(conn: &Connection, id: SourceId) -> Result<u64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM downloads WHERE source_id = ?1
           AND state IN ('queued', 'transferring', 'checking', 'importing')",
        [id.0],
        |r| uint(r, 0),
    )?)
}

/// Whether a platform with this id exists.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn platform_exists(conn: &Connection, id: &PlatformId) -> Result<bool> {
    Ok(conn
        .query_row("SELECT 1 FROM platforms WHERE id = ?1", [&id.0], |_| Ok(()))
        .optional()?
        .is_some())
}

/// The file name part of a DAT rom name, which may carry a subdirectory.
fn leaf(name: &str) -> &str {
    name.rsplit(['/', '\\']).next().unwrap_or(name)
}

/// Fills `roms.match_name` and `roms.match_base` where missing, with the keys
/// [`binding::normalise_name`] and [`binding::base_name`] give. Returns rows keyed.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn refresh_match_keys(conn: &Connection) -> Result<usize> {
    let mut select =
        conn.prepare_cached("SELECT id, name FROM roms WHERE match_name IS NULL LIMIT ?1")?;
    let mut update =
        conn.prepare_cached("UPDATE roms SET match_name = ?2, match_base = ?3 WHERE id = ?1")?;
    let mut keyed = 0;
    loop {
        let batch: Vec<(i64, String)> = select
            .query_map([KEY_BATCH], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        if batch.is_empty() {
            return Ok(keyed);
        }
        for (id, name) in &batch {
            let normalised = binding::normalise_name(leaf(name));
            update.execute(params![id, normalised, binding::base_name(&normalised)])?;
        }
        keyed += batch.len();
    }
}

/// Binding's view of the catalog: roms of titles that are neither retired nor
/// BIOS, looked up by the keys [`refresh_match_keys`] stores. Query failures read as no match.
pub struct SqlDatIndex<'c> {
    conn: &'c Connection,
}

impl<'c> SqlDatIndex<'c> {
    /// An index over `conn`; call [`refresh_match_keys`] on it first.
    #[must_use]
    pub fn new(conn: &'c Connection) -> Self {
        Self { conn }
    }

    fn lookup(&self, sql: &str, args: impl rusqlite::Params) -> Vec<(PlatformId, RomRef)> {
        let run = || -> rusqlite::Result<Vec<(PlatformId, RomRef)>> {
            let mut stmt = self.conn.prepare_cached(sql)?;
            let rows = stmt.query_map(args, |r| Ok((PlatformId(r.get(0)?), RomRef(r.get(1)?))))?;
            rows.collect()
        };
        run().unwrap_or_else(|e| {
            tracing::warn!(error = %e, "rom lookup failed");
            Vec::new()
        })
    }
}

impl DatIndex for SqlDatIndex<'_> {
    fn by_normalised_name(&self, name: &str) -> Vec<(PlatformId, RomRef)> {
        self.lookup(
            "SELECT t.platform_id, r.id FROM roms r JOIN titles t ON t.id = r.title_id
             WHERE r.match_name = ?1 AND t.retired = 0 AND t.flags NOT LIKE '%\"bios\"%'
             ORDER BY r.id",
            [name],
        )
    }

    fn by_base_name_and_size(&self, base_name: &str, size: u64) -> Vec<(PlatformId, RomRef)> {
        self.lookup(
            "SELECT t.platform_id, r.id FROM roms r JOIN titles t ON t.id = r.title_id
             WHERE r.match_base = ?1 AND r.size = ?2 AND t.retired = 0
               AND t.flags NOT LIKE '%\"bios\"%'
             ORDER BY r.id",
            params![base_name, sql_int(size)],
        )
    }
}

/// Catalog rows for tests, standing in for the DAT import.
#[cfg(any(test, feature = "test-support"))]
pub mod fixtures {
    use rusqlite::{params, Connection};

    use crate::error::Result;

    /// Inserts a DAT version, a title and one rom, returning the rom id.
    /// `flags` is the title's JSON flag array, e.g. `[]` or `["bios"]`.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Db`] on SQLite failure, e.g. an unknown platform.
    pub fn seed_rom(
        conn: &Connection,
        platform: &str,
        rom_name: &str,
        size: u64,
        flags: &str,
    ) -> Result<i64> {
        conn.execute(
            "INSERT INTO dat_versions (platform_id, dat_name, version, source_file, loaded_at, game_count)
             VALUES (?1, ?1 || ' test', '1', 'test.dat', 0, 0)
             ON CONFLICT (dat_name, version) DO NOTHING",
            [platform],
        )?;
        let dat: i64 = conn.query_row(
            "SELECT id FROM dat_versions WHERE dat_name = ?1 || ' test'",
            [platform],
            |r| r.get(0),
        )?;
        let title = rom_name.rsplit_once('.').map_or(rom_name, |(t, _)| t);
        conn.execute(
            "INSERT INTO titles (platform_id, dat_version_id, name, base_name, regions, languages, flags)
             VALUES (?1, ?2, ?3, ?3, '[]', '[]', ?4)",
            params![platform, dat, title, flags],
        )?;
        let title_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO roms (title_id, name, size, status) VALUES (?1, ?2, ?3, 'good')",
            params![title_id, rom_name, super::sql_int(size)],
        )?;
        Ok(conn.last_insert_rowid())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil;

    fn conn() -> Connection {
        let mut c = Connection::open_in_memory().expect("open");
        crate::db::migrate::apply(&mut c).expect("migrate");
        crate::db::platforms::seed(&mut c, &mistarr_mister::platforms::PLATFORMS).expect("seed");
        c
    }

    fn new(hash: &str, state: SourceState) -> NewSource<'_> {
        NewSource {
            infohash: hash,
            display_name: "Synthetic Set",
            origin_file: "set.torrent",
            state,
            reason: None,
            added_at: 5,
        }
    }

    fn file(index: u32, path: &str, size: u64) -> TorrentFile {
        TorrentFile {
            index,
            path: path.to_owned(),
            size,
        }
    }

    fn nes() -> PlatformId {
        PlatformId("nes".into())
    }

    #[test]
    fn suggestions_and_unbound_listing() {
        let c = conn();
        let a = insert(&c, &new(&"03".repeat(20), SourceState::Unbound)).expect("insert");
        insert(&c, &new(&"04".repeat(20), SourceState::Resolving)).expect("insert");
        assert_eq!(list_unbound(&c).expect("list"), [(a, None)]);
        set_suggestion(&c, a, Some(&nes())).expect("suggest");
        assert_eq!(list_unbound(&c).expect("list"), [(a, Some(nes()))]);
        let row = get(&c, a).expect("get").expect("row");
        assert_eq!(row.suggested_platform_id, Some(nes()));
        assert!(!platform_has_dat(&c, &nes()).expect("dat"));
        fixtures::seed_rom(&c, "nes", "Example Quest (USA).nes", 1, "[]").expect("seed");
        assert!(platform_has_dat(&c, &nes()).expect("dat"));
        set_suggestion(&c, a, None).expect("clear");
        assert_eq!(list_unbound(&c).expect("list"), [(a, None)]);
    }

    #[test]
    fn insert_get_find_and_list() {
        let c = conn();
        let a = insert(&c, &new(&"01".repeat(20), SourceState::Unbound)).expect("insert");
        let b = insert(&c, &new(&"02".repeat(20), SourceState::Resolving)).expect("insert");
        assert!(insert(&c, &new(&"01".repeat(20), SourceState::Unbound)).is_err());
        let row = get(&c, a).expect("get").expect("row");
        assert_eq!(row.state, SourceState::Unbound);
        assert_eq!(row.seed_policy, "none");
        assert_eq!((row.file_count, row.matched_count), (0, 0));
        let found = find_by_infohash(&c, &"02".repeat(20)).expect("find");
        assert_eq!(found.map(|r| r.id), Some(b));
        assert!(get(&c, SourceId(99)).expect("get").is_none());
        let (page, total) = list(&c, 1, 1).expect("list");
        assert_eq!((page.len(), total), (1, 2));
        assert_eq!(page[0].id, b);
        assert_eq!(list_resolving(&c).expect("resolving"), [(b, false)]);
        set_client_id(&c, b, Some("x")).expect("client");
        assert_eq!(list_resolving(&c).expect("resolving"), [(b, true)]);
        assert_eq!(SourceId(3).to_string(), "3");
    }

    #[test]
    fn updates_touch_one_column_each() {
        let c = conn();
        let id = insert(&c, &new(&"03".repeat(20), SourceState::Resolving)).expect("insert");
        set_reason(&c, id, Some("waiting")).expect("reason");
        set_client_id(&c, id, Some("abc")).expect("client");
        set_seed_policy(&c, id, &SeedPolicy::Ratio { ratio: 2.0 }).expect("seed");
        set_binding(&c, id, Some(&nes()), Some(0.75)).expect("bind");
        let row = get(&c, id).expect("get").expect("row");
        assert_eq!(row.reason.as_deref(), Some("waiting"));
        assert_eq!(row.client_id.as_deref(), Some("abc"));
        assert_eq!(row.seed_policy, "ratio:2.0");
        assert_eq!(row.platform_id, Some(nes()));
        assert_eq!(row.bind_score, Some(0.75));
        set_state(&c, id, SourceState::Disabled, None).expect("state");
        let row = get(&c, id).expect("get").expect("row");
        assert_eq!((row.state, row.reason), (SourceState::Disabled, None));
        assert!(platform_exists(&c, &nes()).expect("exists"));
        assert!(!platform_exists(&c, &PlatformId("none".into())).expect("exists"));
    }

    #[test]
    fn files_carry_matches_confidence_and_rom_names() {
        let c = conn();
        let rom = fixtures::seed_rom(&c, "nes", "Example Quest (USA).nes", 16, "[]").expect("rom");
        let id = insert(&c, &new(&"04".repeat(20), SourceState::Bound)).expect("insert");
        let list = [
            file(0, "Sub/Example Quest (USA).nes", 16),
            file(1, "x.txt", 4),
        ];
        replace_files(&c, id, &list).expect("files");
        set_matches(
            &c,
            id,
            &[
                (0, Some(RomRef(rom)), Confidence::Name),
                (1, None, Confidence::Unmatched),
            ],
        )
        .expect("matches");
        let row = get(&c, id).expect("get").expect("row");
        assert_eq!(
            (row.file_count, row.matched_count, row.total_size),
            (2, 1, 20)
        );
        let (rows, total) = files(&c, id, 10, 0).expect("files");
        assert_eq!(total, 2);
        assert_eq!(rows[0].rom_name.as_deref(), Some("Example Quest (USA).nes"));
        assert_eq!(rows[0].confidence.as_deref(), Some("name"));
        assert!(rows[0].title_id.is_some());
        assert_eq!(
            (rows[1].rom_id, rows[1].confidence.as_deref()),
            (None, None)
        );
        assert_eq!(torrent_files(&c, id).expect("list"), list);
        replace_files(&c, id, &list[..1]).expect("replace");
        assert_eq!(get(&c, id).expect("get").expect("row").matched_count, 0);
        assert_eq!(open_download_count(&c, id).expect("downloads"), 0);
        assert!(delete(&c, id).expect("delete"));
        assert!(!delete(&c, id).expect("delete again"));
        assert_eq!(files(&c, id, 10, 0).expect("files").1, 0);
    }

    #[test]
    fn index_finds_by_name_and_by_base_name_and_size() {
        let c = conn();
        let a = fixtures::seed_rom(&c, "nes", "Example Quest (USA).nes", 16, "[]").expect("rom");
        let b =
            fixtures::seed_rom(&c, "snes", "Sub\\Other Tale (Europe).sfc", 32, "[]").expect("rom");
        fixtures::seed_rom(&c, "nes", "Boot Code (World).nes", 8, r#"["bios"]"#).expect("rom");
        assert_eq!(refresh_match_keys(&c).expect("keys"), 3);
        assert_eq!(refresh_match_keys(&c).expect("keys"), 0);
        let index = SqlDatIndex::new(&c);
        assert_eq!(
            index.by_normalised_name("example quest (usa)"),
            [(nes(), RomRef(a))]
        );
        assert_eq!(
            index.by_base_name_and_size("other tale", 32),
            [(PlatformId("snes".into()), RomRef(b))]
        );
        assert!(index.by_base_name_and_size("other tale", 33).is_empty());
        assert!(index.by_normalised_name("boot code (world)").is_empty());
    }

    #[test]
    fn binding_runs_against_the_index() {
        let c = conn();
        fixtures::seed_rom(&c, "nes", "Example Quest (USA).nes", 16, "[]").expect("rom");
        fixtures::seed_rom(&c, "nes", "Second Try (Japan).nes", 24, "[]").expect("rom");
        refresh_match_keys(&c).expect("keys");
        let files = [
            file(0, "Set/Example Quest (USA).nes", 16),
            file(1, "Set/Second Try (Europe).nes", 24),
        ];
        let index = SqlDatIndex::new(&c);
        assert_eq!(
            binding::bind(&files, &index, 0.6),
            binding::Binding::Bound(nes(), 1.0)
        );
        let confidences: Vec<_> = binding::match_files(&files, &nes(), &index)
            .into_iter()
            .map(|(_, _, c)| c)
            .collect();
        assert_eq!(confidences, [Confidence::Name, Confidence::Size]);
    }

    #[test]
    fn seed_text_round_trips() {
        for p in [
            SeedPolicy::None,
            SeedPolicy::Client,
            SeedPolicy::Ratio { ratio: 1.0 },
        ] {
            assert_eq!(seed_from_text(&seed_to_text(&p)), Some(p));
        }
        assert_eq!(seed_from_text("ratio:0"), None);
        assert_eq!(seed_from_text("ratio:inf"), None);
        assert_eq!(seed_from_text("sometimes"), None);
    }

    #[test]
    fn states_and_confidence_round_trip() {
        for s in ["resolving", "unbound", "bound", "disabled"] {
            assert_eq!(SourceState::parse(s).map(SourceState::as_str), Some(s));
        }
        assert_eq!(confidence_text(Confidence::Name), Some("name"));
    }

    #[test]
    fn works_on_the_file_database() {
        let (_dir, db) = testutil::db();
        let id = db
            .write_blocking(|c| insert(c, &new(&"05".repeat(20), SourceState::Unbound)))
            .expect("insert");
        assert!(db.read_blocking(|c| get(c, id)).expect("get").is_some());
    }
}
