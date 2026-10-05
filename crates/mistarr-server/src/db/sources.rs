//! The `sources` and `torrent_files` tables, and the [`DatIndex`] binding reads roms through.

use mistarr_clients::{ClientTorrentId, SeedPolicy};
use mistarr_core::PlatformId;
use mistarr_sources::binding::{self, Confidence, DatIndex, RomRef};
use mistarr_sources::torrent::TorrentFile;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use super::candidates::MatchConfidence;
use super::downloads::DownloadState;
use super::ids::{RomId, SourceId};
use super::sql::{self, text_enum, Page, Paged};
use crate::error::Result;

/// Roms given match keys per statement batch in [`refresh_match_keys`].
const KEY_BATCH: u32 = 1000;

text_enum! {
    /// `sources.state`; the machine is in `docs/DATA-MODEL.md`.
    pub enum SourceState {
        /// A magnet whose file list the client has not fetched yet.
        Resolving = "resolving",
        /// File list known, no platform reached the threshold.
        Unbound = "unbound",
        /// Attached to a platform.
        Bound = "bound",
        /// Turned off by the user.
        Disabled = "disabled",
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

/// Why a source is in its state: `sources.reason`, stored as JSON tagged by `code`;
/// `http` words it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SourceReason {
    /// No download client is detected to read a magnet's file list.
    NoClient,
    /// The client is fetching a magnet's file list.
    WaitingMetadata,
    /// The stored infohash cannot be read, so the magnet cannot be added.
    BadInfohash,
    /// The client refused the source.
    ClientRefused {
        /// The client's error.
        error: String,
    },
    /// No platform matched enough of the files.
    NoMatch {
        /// The bind threshold, in percent.
        percent: u32,
        /// The platform its names suggest, when that platform has a DAT.
        suggested: Option<PlatformId>,
    },
    /// Its names suggest a platform whose DAT is not loaded yet.
    AwaitingDat {
        /// That platform.
        platform: PlatformId,
    },
    /// The user marked it as not a game set.
    Ignored,
}

impl SourceReason {
    /// [`SourceReason::NoMatch`] at bind threshold `threshold`, a share of the files.
    ///
    /// ```
    /// use mistarr_server::db::sources::SourceReason;
    /// let r = SourceReason::no_match(0.6, None);
    /// assert_eq!(r, SourceReason::NoMatch { percent: 60, suggested: None });
    /// ```
    #[must_use]
    pub fn no_match(threshold: f32, suggested: Option<PlatformId>) -> Self {
        // The threshold is validated to lie in 0..=1, so the percent fits.
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "The value is bounded by construction."
        )]
        let percent = (threshold.clamp(0.0, 1.0) * 100.0).round() as u32;
        Self::NoMatch { percent, suggested }
    }
}

impl rusqlite::types::ToSql for SourceReason {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        serde_json::to_string(self)
            .map(rusqlite::types::ToSqlOutput::from)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
    }
}

impl rusqlite::types::FromSql for SourceReason {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        sql::json_from_sql("sources.reason", value)
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
    /// Why the source is resolving or unbound; `http` words it.
    #[serde(skip)]
    pub reason: Option<SourceReason>,
    /// `none`, `client` or `ratio:N`.
    pub seed_policy: String,
    /// Files in the torrent.
    pub file_count: u64,
    /// Files with a matched rom or a candidate rom.
    pub matched_count: u64,
    /// Sum of file sizes.
    pub total_size: u64,
    /// Id in the download client once added; a stored id that is not an
    /// infohash names no torrent, so it reads as `None`.
    pub client_id: Option<ClientTorrentId>,
    /// Unix seconds.
    pub added_at: i64,
    /// The platform the torrent's names point at, found without any DAT.
    pub suggested_platform_id: Option<PlatformId>,
    /// True when the user chose the binding, a platform or none; automatic binding keeps it.
    pub user_binding: bool,
    /// The binding the user asked for that its `bind_source` job has not applied yet.
    pub pending_binding: Option<BindChoice>,
}

/// A binding the user chose for a source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindChoice {
    /// This platform, matching the files against it only.
    Platform(PlatformId),
    /// No platform: not a game set.
    Ignore,
    /// Whatever automatic binding decides, now and after later DAT loads.
    Automatic,
}

impl BindChoice {
    /// The `sources.bind_pending` text.
    ///
    /// ```
    /// use mistarr_core::PlatformId;
    /// use mistarr_server::db::sources::BindChoice;
    /// let nes = BindChoice::Platform(PlatformId("nes".into()));
    /// assert_eq!(nes.to_text(), "platform:nes");
    /// assert_eq!(BindChoice::parse("platform:nes"), Some(nes));
    /// assert_eq!(BindChoice::parse("none"), Some(BindChoice::Ignore));
    /// assert_eq!(BindChoice::parse("x"), None);
    /// ```
    #[must_use]
    pub fn to_text(&self) -> String {
        match self {
            Self::Platform(p) => format!("platform:{}", p.0),
            Self::Ignore => "none".to_owned(),
            Self::Automatic => "automatic".to_owned(),
        }
    }

    /// Parses the `sources.bind_pending` text.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "none" => Some(Self::Ignore),
            "automatic" => Some(Self::Automatic),
            other => other
                .strip_prefix("platform:")
                .filter(|p| !p.is_empty())
                .map(|p| Self::Platform(PlatformId(p.to_owned()))),
        }
    }
}

/// Serialises as `{ automatic, platform_id }`, `platform_id` null for [`BindChoice::Ignore`].
impl Serialize for BindChoice {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct as _;
        let mut out = s.serialize_struct("BindChoice", 2)?;
        out.serialize_field("automatic", &matches!(self, Self::Automatic))?;
        let platform = match self {
            Self::Platform(p) => Some(p.0.as_str()),
            _ => None,
        };
        out.serialize_field("platform_id", &platform)?;
        out.end()
    }
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
    pub reason: Option<SourceReason>,
    /// Unix seconds.
    pub added_at: i64,
}

const COLUMNS: &str = "s.id, s.infohash, s.display_name, s.origin_file, s.platform_id,
    s.bind_score, s.state, s.reason, s.seed_policy, s.file_count, s.total_size,
    s.client_id, s.added_at,
    (SELECT COUNT(*) FROM torrent_files f WHERE f.source_id = s.id
       AND (f.rom_id IS NOT NULL OR EXISTS (SELECT 1 FROM torrent_candidates c
             WHERE c.source_id = f.source_id AND c.file_index = f.file_index))),
    s.suggested_platform_id, s.user_binding, s.bind_pending";

/// Reads a `client_id` column; text that is not an infohash names no torrent.
pub(crate) fn client_id(r: &Row<'_>, i: usize) -> rusqlite::Result<Option<ClientTorrentId>> {
    Ok(r.get::<_, Option<String>>(i)?.and_then(|s| s.parse().ok()))
}

fn from_row(r: &Row<'_>) -> rusqlite::Result<SourceRow> {
    Ok(SourceRow {
        id: r.get(0)?,
        infohash: r.get(1)?,
        display_name: r.get(2)?,
        origin_file: r.get(3)?,
        platform_id: r.get::<_, Option<String>>(4)?.map(PlatformId),
        bind_score: r.get(5)?,
        state: r.get(6)?,
        reason: r.get(7)?,
        seed_policy: r.get(8)?,
        file_count: sql::get_u64(r, 9)?,
        total_size: sql::get_u64(r, 10)?,
        client_id: client_id(r, 11)?,
        added_at: r.get(12)?,
        matched_count: sql::get_u64(r, 13)?,
        suggested_platform_id: r.get::<_, Option<String>>(14)?.map(PlatformId),
        user_binding: r.get(15)?,
        pending_binding: r
            .get::<_, Option<String>>(16)?
            .as_deref()
            .and_then(BindChoice::parse),
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
            s.state,
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
            [id],
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
pub fn list(conn: &Connection, page: Page) -> Result<Paged<SourceRow>> {
    sql::snapshot(conn, |conn| {
        let total = conn.query_row("SELECT COUNT(*) FROM sources", [], |r| sql::get_u64(r, 0))?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM sources s ORDER BY s.id LIMIT ?1 OFFSET ?2"
        ))?;
        let items = stmt
            .query_map(params![page.limit, page.offset], from_row)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Paged { items, total })
    })
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
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
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
    reason: Option<&SourceReason>,
) -> Result<()> {
    conn.execute(
        "UPDATE sources SET state = ?2, reason = ?3 WHERE id = ?1",
        params![id, state, reason],
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
        params![id, platform.map(|p| p.0.as_str())],
    )?;
    Ok(())
}

/// Records whether the user chose the source's binding, which keeps it out of
/// [`list_unbound`] and so out of every automatic binding.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn set_user_binding(conn: &Connection, id: SourceId, chosen: bool) -> Result<()> {
    conn.execute(
        "UPDATE sources SET user_binding = ?2 WHERE id = ?1",
        params![id, chosen],
    )?;
    Ok(())
}

/// Records the binding the user asked for, which the next `bind_source` job of the
/// source applies, and whether the user chose it.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn request_binding(conn: &Connection, id: SourceId, choice: &BindChoice) -> Result<()> {
    conn.execute(
        "UPDATE sources SET bind_pending = ?2, user_binding = ?3 WHERE id = ?1",
        params![id, choice.to_text(), *choice != BindChoice::Automatic],
    )?;
    Ok(())
}

/// Takes the binding the user asked for, clearing it; `None` when none waits.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn take_binding(conn: &Connection, id: SourceId) -> Result<Option<BindChoice>> {
    let text: Option<String> = conn
        .query_row(
            "SELECT bind_pending FROM sources WHERE id = ?1",
            [id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    conn.execute("UPDATE sources SET bind_pending = NULL WHERE id = ?1", [id])?;
    Ok(text.as_deref().and_then(BindChoice::parse))
}

/// Unbound sources whose binding the user did not choose, with their
/// suggested platform, oldest first.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn list_unbound(conn: &Connection) -> Result<Vec<(SourceId, Option<PlatformId>)>> {
    let mut stmt = conn.prepare(
        "SELECT id, suggested_platform_id FROM sources
         WHERE state = 'unbound' AND user_binding = 0 ORDER BY id",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok((r.get(0)?, r.get::<_, Option<String>>(1)?.map(PlatformId)))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// Sources other than resolving ones whose platform is one of `platforms`, by id.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn list_on_platforms(conn: &Connection, platforms: &[PlatformId]) -> Result<Vec<SourceId>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id FROM sources WHERE platform_id = ?1 AND state != 'resolving' ORDER BY id",
    )?;
    let mut out: Vec<SourceId> = Vec::new();
    for p in platforms {
        let ids = stmt.query_map([&p.0], |r| r.get::<_, SourceId>(0))?;
        out.extend(ids.collect::<rusqlite::Result<Vec<_>>>()?);
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

/// Sources other than resolving ones that have a platform, by id.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn list_mapped(conn: &Connection) -> Result<Vec<SourceId>> {
    let mut stmt = conn.prepare(
        "SELECT id FROM sources WHERE platform_id IS NOT NULL AND state != 'resolving' ORDER BY id",
    )?;
    let ids = stmt
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(ids)
}

/// Sources with a readable torrent id in the client: id, the client's id
/// for it and its seed policy.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn list_in_client(conn: &Connection) -> Result<Vec<(SourceId, ClientTorrentId, SeedPolicy)>> {
    let mut stmt = conn.prepare(
        "SELECT id, client_id, seed_policy FROM sources WHERE client_id IS NOT NULL ORDER BY id",
    )?;
    let rows = stmt
        .query_map([], |r| {
            let (id, policy) = (r.get(0)?, r.get::<_, String>(2)?);
            let seed = seed_from_text(&policy).unwrap_or(SeedPolicy::None);
            Ok(client_id(r, 1)?.map(|cid| (id, cid, seed)))
        })?
        .filter_map(Result::transpose)
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// The rom stamp the source's files were last mapped against.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn map_stamp(conn: &Connection, id: SourceId) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT map_stamp FROM sources WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .optional()?
        .flatten())
}

/// The platform a source is bound to and the stamp it was mapped against,
/// `None` when it has no platform.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn mapped_against(
    conn: &Connection,
    id: SourceId,
) -> Result<Option<(PlatformId, Option<String>)>> {
    Ok(conn
        .query_row(
            "SELECT platform_id, map_stamp FROM sources WHERE id = ?1 AND platform_id IS NOT NULL",
            [id],
            |r| Ok((PlatformId(r.get(0)?), r.get(1)?)),
        )
        .optional()?)
}

/// Stores the rom stamp the source's files were mapped against.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn set_map_stamp(conn: &Connection, id: SourceId, stamp: Option<&str>) -> Result<()> {
    conn.execute(
        "UPDATE sources SET map_stamp = ?2 WHERE id = ?1",
        params![id, stamp],
    )?;
    Ok(())
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
pub fn set_reason(conn: &Connection, id: SourceId, reason: Option<&SourceReason>) -> Result<()> {
    conn.execute(
        "UPDATE sources SET reason = ?2 WHERE id = ?1",
        params![id, reason],
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
        params![id, seed_to_text(policy)],
    )?;
    Ok(())
}

/// Stores or clears the client's id for the torrent.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn set_client_id(
    conn: &Connection,
    id: SourceId,
    client_id: Option<ClientTorrentId>,
) -> Result<()> {
    conn.execute(
        "UPDATE sources SET client_id = ?2 WHERE id = ?1",
        params![id, client_id.map(|c| c.to_string())],
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
        params![id, platform.map(|p| p.0.as_str()), score],
    )?;
    Ok(())
}

/// Replaces the source's file list, clearing matches and candidates but
/// keeping each hash proof whose file keeps its index, path and size, and
/// updates its counts.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn replace_files(conn: &Connection, id: SourceId, files: &[TorrentFile]) -> Result<()> {
    let proofs: Vec<(u32, String, i64, RomId)> = conn
        .prepare_cached(
            "SELECT file_index, path, size, rom_id FROM torrent_files
             WHERE source_id = ?1 AND confidence = 'hash' AND rom_id IS NOT NULL",
        )?
        .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    conn.execute("DELETE FROM torrent_files WHERE source_id = ?1", [id])?;
    let mut stmt = conn.prepare_cached(
        "INSERT INTO torrent_files (source_id, file_index, path, size) VALUES (?1, ?2, ?3, ?4)",
    )?;
    for f in files {
        stmt.execute(params![id, f.index, f.path, sql::to_i64(f.size)])?;
    }
    let mut proven = conn.prepare_cached(
        "UPDATE torrent_files SET rom_id = ?5, confidence = 'hash'
         WHERE source_id = ?1 AND file_index = ?2 AND path = ?3 AND size = ?4",
    )?;
    for (index, path, size, rom) in proofs {
        proven.execute(params![id, index, path, size, rom])?;
    }
    let total: u64 = files.iter().map(|f| f.size).sum();
    conn.execute(
        "UPDATE sources SET file_count = ?2, total_size = ?3 WHERE id = ?1",
        params![id, sql::to_i64(files.len()), sql::to_i64(total)],
    )?;
    Ok(())
}

/// Forgets every file's matched rom and confidence, hash proofs included.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn clear_matches(conn: &Connection, id: SourceId) -> Result<()> {
    conn.execute(
        "UPDATE torrent_files SET rom_id = NULL, confidence = NULL
         WHERE source_id = ?1 AND rom_id IS NOT NULL",
        [id],
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
            id,
            index,
            rom.map(|r| RomId(r.0)),
            MatchConfidence::of(*confidence)
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
        .query_map([id], |r| {
            Ok(TorrentFile {
                index: r.get(0)?,
                path: r.get(1)?,
                size: sql::get_u64(r, 2)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(files)
}

/// Deletes a source and, by cascade, its files. Its downloads keep their rows
/// with `source_id` NULL. Returns whether it existed.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn delete(conn: &Connection, id: SourceId) -> Result<bool> {
    Ok(conn.execute("DELETE FROM sources WHERE id = ?1", [id])? > 0)
}

/// Downloads of the source that are queued, transferring, checking or importing.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn open_download_count(conn: &Connection, id: SourceId) -> Result<u64> {
    Ok(conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM downloads WHERE source_id = ?1 AND state IN {}",
            DownloadState::SELECTED_SQL
        ),
        [id],
        |r| sql::get_u64(r, 0),
    )?)
}

/// The file name part of a DAT rom name, which may carry a subdirectory.
fn leaf(name: &str) -> &str {
    name.rsplit(['/', '\\']).next().unwrap_or(name)
}

/// Fills `roms.match_name` and `roms.match_base` where missing, with the keys
/// [`binding::normalize_name`] and [`binding::base_name`] give. Returns rows keyed.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn refresh_match_keys(conn: &Connection) -> Result<usize> {
    let mut keyed = 0;
    loop {
        match key_batch(conn)? {
            0 => return Ok(keyed),
            n => keyed += n,
        }
    }
}

/// Keys at most [`KEY_BATCH`] of the roms [`refresh_match_keys`] would, so a caller can
/// commit between batches. Returns rows keyed, 0 once none is left.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn key_batch(conn: &Connection) -> Result<usize> {
    let mut select =
        conn.prepare_cached("SELECT id, name FROM roms WHERE match_name IS NULL LIMIT ?1")?;
    let mut update =
        conn.prepare_cached("UPDATE roms SET match_name = ?2, match_base = ?3 WHERE id = ?1")?;
    let batch: Vec<(RomId, String)> = select
        .query_map([KEY_BATCH], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (id, name) in &batch {
        let normalized = binding::normalize_name(leaf(name));
        update.execute(params![id, normalized, binding::base_name(&normalized)])?;
    }
    Ok(batch.len())
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
    fn by_normalized_name(&self, name: &str) -> Vec<(PlatformId, RomRef)> {
        self.lookup(
            "SELECT t.platform_id, r.id FROM roms r JOIN titles t ON t.id = r.title_id
             WHERE r.match_name = ?1 AND t.retired = 0
               AND NOT EXISTS (SELECT 1 FROM title_flags f WHERE f.title_id = t.id AND f.flag = 'bios')
             ORDER BY r.id",
            [name],
        )
    }

    fn by_base_name_and_size(&self, base_name: &str, size: u64) -> Vec<(PlatformId, RomRef)> {
        self.lookup(
            "SELECT t.platform_id, r.id FROM roms r JOIN titles t ON t.id = r.title_id
             WHERE r.match_base = ?1 AND r.size = ?2 AND t.retired = 0
               AND NOT EXISTS (SELECT 1 FROM title_flags f WHERE f.title_id = t.id AND f.flag = 'bios')
             ORDER BY r.id",
            params![base_name, sql::to_i64(size)],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::fixtures::{conn, pid, seed_rom};
    use crate::db::testutil;
    use crate::db::views::source_detail::{self, FileRow};

    fn files(c: &Connection, id: SourceId, limit: u32, offset: u32) -> Result<Paged<FileRow>> {
        source_detail::files(
            c,
            id,
            &source_detail::FileQuery::default(),
            Page { limit, offset },
        )
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
        seed_rom(&c, &pid("nes"), "Example Quest (USA).nes", 1, &[]).expect("seed");
        assert!(platform_has_dat(&c, &nes()).expect("dat"));
        set_suggestion(&c, a, None).expect("clear");
        assert_eq!(list_unbound(&c).expect("list"), [(a, None)]);
        assert!(list_on_platforms(&c, &[nes()]).expect("on").is_empty());
        set_binding(&c, a, Some(&nes()), Some(1.0)).expect("bind");
        let snes = PlatformId("snes".into());
        assert_eq!(list_on_platforms(&c, &[snes, nes()]).expect("on"), [a]);
        assert_eq!(list_mapped(&c).expect("mapped"), [a]);
        assert_eq!(map_stamp(&c, a).expect("stamp"), None);
        set_map_stamp(&c, a, Some("1:2:3")).expect("set");
        assert_eq!(map_stamp(&c, a).expect("stamp").as_deref(), Some("1:2:3"));
        assert_eq!(
            mapped_against(&c, a).expect("against"),
            Some((nes(), Some("1:2:3".to_owned())))
        );
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
        let page = list(
            &c,
            Page {
                limit: 1,
                offset: 1,
            },
        )
        .expect("list");
        assert_eq!((page.items.len(), page.total), (1, 2));
        assert_eq!(page.items[0].id, b);
        assert_eq!(list_resolving(&c).expect("resolving"), [(b, false)]);
        let cid = ClientTorrentId::new(mistarr_core::InfoHash::from_bytes([0x0b; 20]));
        set_client_id(&c, b, Some(cid)).expect("client");
        assert_eq!(list_resolving(&c).expect("resolving"), [(b, true)]);
        assert_eq!(
            list_in_client(&c).expect("in client"),
            [(b, cid, SeedPolicy::None)]
        );
        c.execute("UPDATE sources SET client_id = 'x' WHERE id = ?1", [b.0])
            .expect("unreadable id");
        assert_eq!(list_resolving(&c).expect("resolving"), [(b, true)]);
        assert!(list_in_client(&c).expect("in client").is_empty());
        assert_eq!(get(&c, b).expect("get").expect("row").client_id, None);
        assert_eq!(SourceId(3).to_string(), "3");
    }

    #[test]
    fn updates_touch_one_column_each() {
        let c = conn();
        let id = insert(&c, &new(&"03".repeat(20), SourceState::Resolving)).expect("insert");
        set_reason(&c, id, Some(&SourceReason::WaitingMetadata)).expect("reason");
        let cid = ClientTorrentId::new(mistarr_core::InfoHash::from_bytes([0xab; 20]));
        set_client_id(&c, id, Some(cid)).expect("client");
        set_seed_policy(&c, id, &SeedPolicy::Ratio { ratio: 2.0 }).expect("seed");
        set_binding(&c, id, Some(&nes()), Some(0.75)).expect("bind");
        let row = get(&c, id).expect("get").expect("row");
        assert_eq!(row.reason, Some(SourceReason::WaitingMetadata));
        assert_eq!(row.client_id, Some(cid));
        assert_eq!(row.seed_policy, "ratio:2.0");
        assert_eq!(row.platform_id, Some(nes()));
        assert_eq!(row.bind_score, Some(0.75));
        set_state(&c, id, SourceState::Disabled, None).expect("state");
        let row = get(&c, id).expect("get").expect("row");
        assert_eq!((row.state, row.reason), (SourceState::Disabled, None));
        c.execute("UPDATE sources SET reason = 'prose' WHERE id = ?1", [id])
            .expect("corrupt");
        assert!(matches!(get(&c, id), Err(crate::Error::Stored { .. })));
    }

    #[test]
    fn files_carry_matches_confidence_and_rom_names() {
        let c = conn();
        let rom = seed_rom(&c, &pid("nes"), "Example Quest (USA).nes", 16, &[]).expect("rom");
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
                (0, Some(RomRef(rom.0)), Confidence::Name),
                (1, None, Confidence::Unmatched),
            ],
        )
        .expect("matches");
        let row = get(&c, id).expect("get").expect("row");
        assert_eq!(
            (row.file_count, row.matched_count, row.total_size),
            (2, 1, 20)
        );
        let Paged { items: rows, total } = files(&c, id, 10, 0).expect("files");
        assert_eq!(total, 2);
        assert_eq!(rows[0].rom_name.as_deref(), Some("Example Quest (USA).nes"));
        assert_eq!(rows[0].confidence, Some(MatchConfidence::Name));
        assert!(rows[0].title_id.is_some());
        assert_eq!((rows[1].rom_id, rows[1].confidence), (None, None));
        assert_eq!(torrent_files(&c, id).expect("list"), list);
        crate::db::candidates::prove(&c, id, 0, rom).expect("prove");
        replace_files(&c, id, &list).expect("replace");
        let rows = files(&c, id, 10, 0).expect("files").items;
        assert_eq!(
            rows[0].confidence,
            Some(MatchConfidence::Hash),
            "a proof survives"
        );
        clear_matches(&c, id).expect("clear");
        assert_eq!(get(&c, id).expect("get").expect("row").matched_count, 0);
        crate::db::candidates::prove(&c, id, 0, rom).expect("prove");
        let moved = [file(0, "Sub/Other.nes", 16)];
        replace_files(&c, id, &moved).expect("replace");
        assert_eq!(get(&c, id).expect("get").expect("row").matched_count, 0);
        assert_eq!(open_download_count(&c, id).expect("downloads"), 0);
        assert!(delete(&c, id).expect("delete"));
        assert!(!delete(&c, id).expect("delete again"));
        assert_eq!(files(&c, id, 10, 0).expect("files").total, 0);
    }

    #[test]
    fn index_finds_by_name_and_by_base_name_and_size() {
        let c = conn();
        let a = seed_rom(&c, &pid("nes"), "Example Quest (USA).nes", 16, &[]).expect("rom");
        let b = seed_rom(&c, &pid("snes"), "Sub\\Other Tale (Europe).sfc", 32, &[]).expect("rom");
        seed_rom(&c, &pid("nes"), "Boot Code (World).nes", 8, &["bios"]).expect("rom");
        assert_eq!(refresh_match_keys(&c).expect("keys"), 3);
        assert_eq!(refresh_match_keys(&c).expect("keys"), 0);
        let index = SqlDatIndex::new(&c);
        assert_eq!(
            index.by_normalized_name("example quest (usa)"),
            [(nes(), RomRef(a.0))]
        );
        assert_eq!(
            index.by_base_name_and_size("other tale", 32),
            [(PlatformId("snes".into()), RomRef(b.0))]
        );
        assert!(index.by_base_name_and_size("other tale", 33).is_empty());
        assert!(index.by_normalized_name("boot code (world)").is_empty());
    }

    #[test]
    fn a_key_batch_keys_at_most_a_batch() {
        let c = conn();
        let count = KEY_BATCH as usize + 5;
        for i in 0..count {
            seed_rom(
                &c,
                &pid("nes"),
                &format!("Example Quest {i} (USA).nes"),
                16,
                &[],
            )
            .expect("rom");
        }
        assert_eq!(key_batch(&c).expect("first"), KEY_BATCH as usize);
        assert_eq!(key_batch(&c).expect("second"), 5);
        assert_eq!(key_batch(&c).expect("done"), 0);
    }

    #[test]
    fn binding_runs_against_the_index() {
        let c = conn();
        seed_rom(&c, &pid("nes"), "Example Quest (USA).nes", 16, &[]).expect("rom");
        seed_rom(&c, &pid("nes"), "Second Try (Japan).nes", 24, &[]).expect("rom");
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
        assert_eq!(confidences, [Confidence::Name, Confidence::Base]);
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
        assert_eq!(
            MatchConfidence::of(Confidence::Name),
            Some(MatchConfidence::Name)
        );
        assert_eq!(
            MatchConfidence::of(Confidence::Size),
            Some(MatchConfidence::Size)
        );
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
