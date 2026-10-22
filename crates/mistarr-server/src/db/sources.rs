//! The `sources` and `torrent_files` tables, and the [`DatIndex`] binding reads roms through.

use std::collections::HashMap;

use mistarr_clients::{ClientTorrentId, SeedPolicy};
use mistarr_core::{PlatformId, PlatformKey, RomId};
use mistarr_sources::binding::{self, Confidence, DatIndex};
use mistarr_sources::torrent::TorrentFile;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use super::candidates::MatchConfidence;
use super::downloads::DownloadState;
use super::ids::SourceId;
use super::platforms;
use super::sql::{self, code_enum, Page, Paged};
use crate::error::{Error, Result};

/// Roms given match keys per statement batch in [`refresh_match_keys`].
const KEY_BATCH: u32 = 1000;

code_enum! {
    /// `sources.state`; the machine is in `docs/DATA-MODEL.md`.
    pub enum SourceState {
        /// A magnet whose file list the client has not fetched yet.
        Resolving = ("resolving", 0),
        /// File list known, no platform reached the threshold.
        Unbound = ("unbound", 1),
        /// Attached to a platform.
        Bound = ("bound", 2),
        /// Turned off by the user.
        Disabled = ("disabled", 3),
    }
}

code_enum! {
    /// `sources.seed_policy`, which carries the ratio in `sources.seed_ratio`.
    pub(crate) enum SeedKind {
        /// Stop once every wanted file is imported.
        None = ("none", 0),
        /// Seed until the client reports a ratio.
        Ratio = ("ratio", 1),
        /// Leave seeding to the client.
        Client = ("client", 2),
    }
}

code_enum! {
    /// `sources.bind_pending`, which carries the platform in `sources.bind_platform`.
    pub(crate) enum BindKind {
        /// Whatever automatic binding decides.
        Automatic = ("automatic", 0),
        /// Not a game set.
        Ignore = ("none", 1),
        /// The platform in `sources.bind_platform`.
        Platform = ("platform", 2),
    }
}

/// A [`SeedPolicy`] read from its row code and ratio.
fn seed_from_columns(kind: SeedKind, ratio: Option<f32>) -> SeedPolicy {
    match kind {
        SeedKind::None => SeedPolicy::None,
        SeedKind::Ratio => SeedPolicy::Ratio {
            ratio: ratio.unwrap_or_default(),
        },
        SeedKind::Client => SeedPolicy::Client,
    }
}

/// The `seed_policy` text of a row code and ratio.
pub(crate) fn seed_text(kind: SeedKind, ratio: Option<f32>) -> String {
    seed_to_text(&seed_from_columns(kind, ratio))
}

/// The row code and ratio of a [`SeedPolicy`].
fn seed_columns(policy: &SeedPolicy) -> (SeedKind, Option<f32>) {
    match policy {
        SeedPolicy::None => (SeedKind::None, None),
        SeedPolicy::Ratio { ratio } => (SeedKind::Ratio, Some(*ratio)),
        SeedPolicy::Client => (SeedKind::Client, None),
    }
}

/// The key of `platform`, which must have a row.
fn platform_key(conn: &Connection, platform: &PlatformId) -> Result<PlatformKey> {
    platforms::key(conn, platform)?
        .ok_or_else(|| Error::Job(format!("unknown platform `{}`", platform.as_str())))
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
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "The threshold is validated to lie in 0..=1, so the percent fits."
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

/// Serialises as `{ automatic, platform_id }`, `platform_id` null for [`BindChoice::Ignore`].
impl Serialize for BindChoice {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct as _;
        let mut out = s.serialize_struct("BindChoice", 2)?;
        out.serialize_field("automatic", &matches!(self, Self::Automatic))?;
        let platform = match self {
            Self::Platform(p) => Some(p.as_str()),
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

const COLUMNS: &str = "s.id, s.infohash, s.display_name, s.origin_file,
    (SELECT p.slug FROM platforms p WHERE p.id = s.platform),
    s.bind_score, s.state, s.reason, s.seed_policy, s.seed_ratio, s.file_count, s.total_size,
    s.client_id, s.added_at,
    ((SELECT COUNT(*) FROM torrent_files f WHERE f.source_id = s.id AND f.rom_id IS NOT NULL)
     + (SELECT COUNT(*) FROM torrent_files f WHERE f.source_id = s.id AND f.rom_id IS NULL
          AND EXISTS (SELECT 1 FROM torrent_candidates c
                WHERE c.source_id = f.source_id AND c.file_index = f.file_index))),
    (SELECT p.slug FROM platforms p WHERE p.id = s.suggested_platform), s.user_binding,
    s.bind_pending, (SELECT p.slug FROM platforms p WHERE p.id = s.bind_platform)";

/// Reads a `client_id` column; text that is not an infohash names no torrent.
pub(crate) fn client_id(r: &Row<'_>, i: usize) -> rusqlite::Result<Option<ClientTorrentId>> {
    Ok(r.get::<_, Option<String>>(i)?.and_then(|s| s.parse().ok()))
}

/// A pending binding read from its row code and platform slug.
fn pending_binding(kind: Option<BindKind>, platform: Option<String>) -> Option<BindChoice> {
    match kind? {
        BindKind::Automatic => Some(BindChoice::Automatic),
        BindKind::Ignore => Some(BindChoice::Ignore),
        BindKind::Platform => platform.map(|p| BindChoice::Platform(PlatformId::new(p))),
    }
}

fn from_row(r: &Row<'_>) -> rusqlite::Result<SourceRow> {
    Ok(SourceRow {
        id: r.get(0)?,
        infohash: r.get(1)?,
        display_name: r.get(2)?,
        origin_file: r.get(3)?,
        platform_id: r.get(4)?,
        bind_score: r.get(5)?,
        state: r.get(6)?,
        reason: r.get(7)?,
        seed_policy: seed_to_text(&seed_from_columns(r.get(8)?, r.get(9)?)),
        file_count: sql::get_u64(r, 10)?,
        total_size: sql::get_u64(r, 11)?,
        client_id: client_id(r, 12)?,
        added_at: r.get(13)?,
        matched_count: sql::get_u64(r, 14)?,
        suggested_platform_id: r.get(15)?,
        user_binding: r.get(16)?,
        pending_binding: pending_binding(r.get(17)?, r.get(18)?),
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
    Ok(SourceId::new(conn.last_insert_rowid()))
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
    let mut stmt =
        conn.prepare("SELECT id, client_id IS NOT NULL FROM sources WHERE state = ?1 ORDER BY id")?;
    let ids = stmt
        .query_map([SourceState::Resolving], |r| Ok((r.get(0)?, r.get(1)?)))?
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
    let key = platform.map(|p| platform_key(conn, p)).transpose()?;
    conn.execute(
        "UPDATE sources SET suggested_platform = ?2 WHERE id = ?1",
        params![id, key],
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
    let (kind, platform) = match choice {
        BindChoice::Automatic => (BindKind::Automatic, None),
        BindChoice::Ignore => (BindKind::Ignore, None),
        BindChoice::Platform(p) => (BindKind::Platform, Some(platform_key(conn, p)?)),
    };
    conn.execute(
        "UPDATE sources SET bind_pending = ?2, bind_platform = ?3, user_binding = ?4 WHERE id = ?1",
        params![id, kind, platform, *choice != BindChoice::Automatic],
    )?;
    Ok(())
}

/// Takes the binding the user asked for, clearing it; `None` when none waits.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn take_binding(conn: &Connection, id: SourceId) -> Result<Option<BindChoice>> {
    let (kind, platform) = conn
        .query_row(
            "SELECT s.bind_pending,
                    (SELECT p.slug FROM platforms p WHERE p.id = s.bind_platform)
             FROM sources s WHERE s.id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .unwrap_or((None, None));
    conn.execute(
        "UPDATE sources SET bind_pending = NULL, bind_platform = NULL WHERE id = ?1",
        [id],
    )?;
    Ok(pending_binding(kind, platform))
}

/// Unbound sources whose binding the user did not choose, with their
/// suggested platform, oldest first.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn list_unbound(conn: &Connection) -> Result<Vec<(SourceId, Option<PlatformId>)>> {
    let mut stmt = conn.prepare(
        "SELECT id, (SELECT p.slug FROM platforms p WHERE p.id = s.suggested_platform)
         FROM sources s WHERE state = ?1 AND user_binding = 0 ORDER BY id",
    )?;
    let rows = stmt
        .query_map([SourceState::Unbound], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// Sources other than resolving ones whose platform is one of `platforms`, by id.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn list_on_platforms(conn: &Connection, platforms: &[PlatformId]) -> Result<Vec<SourceId>> {
    let mut stmt = conn
        .prepare_cached("SELECT id FROM sources WHERE platform = ?1 AND state != ?2 ORDER BY id")?;
    let mut out: Vec<SourceId> = Vec::new();
    for p in platforms {
        let Some(key) = platforms::key(conn, p)? else {
            continue;
        };
        let ids = stmt.query_map(params![key, SourceState::Resolving], |r| {
            r.get::<_, SourceId>(0)
        })?;
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
    let mut stmt = conn
        .prepare("SELECT id FROM sources WHERE platform IS NOT NULL AND state != ?1 ORDER BY id")?;
    let ids = stmt
        .query_map([SourceState::Resolving], |r| r.get(0))?
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
        "SELECT id, client_id, seed_policy, seed_ratio FROM sources
         WHERE client_id IS NOT NULL ORDER BY id",
    )?;
    let rows = stmt
        .query_map([], |r| {
            let (id, policy) = (r.get(0)?, seed_from_columns(r.get(2)?, r.get(3)?));
            Ok(client_id(r, 1)?.map(|cid| (id, cid, policy)))
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
            "SELECT p.slug, s.map_stamp FROM sources s JOIN platforms p ON p.id = s.platform
             WHERE s.id = ?1 AND s.platform IS NOT NULL",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
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
            [platform],
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
    let (kind, ratio) = seed_columns(policy);
    conn.execute(
        "UPDATE sources SET seed_policy = ?2, seed_ratio = ?3 WHERE id = ?1",
        params![id, kind, ratio],
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
    let key = platform.map(|p| platform_key(conn, p)).transpose()?;
    conn.execute(
        "UPDATE sources SET platform = ?2, bind_score = ?3 WHERE id = ?1",
        params![id, key, score],
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
            "SELECT f.file_index, td.path || f.leaf, f.size, f.rom_id
             FROM torrent_files f JOIN torrent_dirs td ON td.id = f.dir_id
             WHERE f.source_id = ?1 AND f.confidence = ?2 AND f.rom_id IS NOT NULL",
        )?
        .query_map(params![id, MatchConfidence::Hash], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    conn.execute("DELETE FROM torrent_files WHERE source_id = ?1", [id])?;
    let mut add_dir = conn
        .prepare_cached("INSERT OR IGNORE INTO torrent_dirs (source_id, path) VALUES (?1, ?2)")?;
    let mut find_dir =
        conn.prepare_cached("SELECT id FROM torrent_dirs WHERE source_id = ?1 AND path = ?2")?;
    let mut stmt = conn.prepare_cached(
        "INSERT INTO torrent_files (source_id, file_index, dir_id, leaf, size)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    // One lookup per distinct directory, whose row is stored once per source.
    let mut seen: HashMap<&str, i64> = HashMap::new();
    for f in files {
        let (directory, leaf) = split_path(&f.path);
        let dir_id = if let Some(found) = seen.get(directory) {
            *found
        } else {
            add_dir.execute(params![id, directory])?;
            let found = find_dir.query_row(params![id, directory], |r| r.get(0))?;
            seen.insert(directory, found);
            found
        };
        stmt.execute(params![id, f.index, dir_id, leaf, sql::to_i64(f.size)])?;
    }
    // A directory the new tree does not use goes with the files that did.
    conn.execute(
        "DELETE FROM torrent_dirs WHERE source_id = ?1
           AND id NOT IN (SELECT dir_id FROM torrent_files WHERE source_id = ?1)",
        [id],
    )?;
    let mut proven = conn.prepare_cached(
        "UPDATE torrent_files SET rom_id = ?6, confidence = ?7
         WHERE source_id = ?1 AND file_index = ?2 AND size = ?5
           AND dir_id = (SELECT id FROM torrent_dirs WHERE source_id = ?1 AND path = ?3)
           AND leaf = ?4",
    )?;
    for (index, path, size, rom) in proofs {
        let (directory, leaf) = split_path(&path);
        proven.execute(params![
            id,
            index,
            directory,
            leaf,
            size,
            rom,
            MatchConfidence::Hash
        ])?;
    }
    let total: u64 = files.iter().map(|f| f.size).sum();
    conn.execute(
        "UPDATE sources SET file_count = ?2, total_size = ?3 WHERE id = ?1",
        params![id, sql::to_i64(files.len()), sql::to_i64(total)],
    )?;
    Ok(())
}

/// A torrent path as its directory and leaf: the directory is everything through the
/// last `/`, `''` for a top-level file, and the leaf is the text after it.
///
/// ```
/// use mistarr_server::db::sources::split_path;
/// assert_eq!(split_path("Set/Sub/pack.nes"), ("Set/Sub/", "pack.nes"));
/// assert_eq!(split_path("pack.nes"), ("", "pack.nes"));
/// ```
#[must_use]
pub fn split_path(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(at) => (&path[..=at], &path[at + 1..]),
        None => ("", path),
    }
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
    matches: &[(u32, Option<RomId>, Confidence)],
) -> Result<()> {
    let mut stmt = conn.prepare_cached(
        "UPDATE torrent_files SET rom_id = ?3, confidence = ?4
         WHERE source_id = ?1 AND file_index = ?2",
    )?;
    for (index, rom, confidence) in matches {
        stmt.execute(params![id, index, rom, MatchConfidence::of(*confidence)])?;
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
        "SELECT file_index, path, size FROM torrent_file_view WHERE source_id = ?1
         ORDER BY file_index",
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
    let mut select = conn.prepare_cached(&format!(
        "SELECT r.id, {} FROM roms r JOIN titles t ON t.id = r.title_id
         WHERE r.match_name IS NULL LIMIT ?1",
        sql::ROM_NAME
    ))?;
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

/// A rom's base name as SQL over the roms alias `r`, for every statement that compares
/// it, so the column and its index change in one place.
macro_rules! match_base {
    () => {
        "r.match_base"
    };
}

/// The predicate on the titles alias `t` that keeps roms binding may match: titles
/// neither retired nor BIOS.
macro_rules! live_title {
    () => {
        "t.retired = 0
         AND NOT EXISTS (SELECT 1 FROM title_flags f WHERE f.title_id = t.id AND f.flag = 'bios')"
    };
}

/// One row per bound key and platform a rom matches it on by name. The key list is a
/// JSON array of `[name, base, size]`, and a key is its position in it.
macro_rules! name_tier {
    () => {
        concat!(
            "SELECT json_each.key AS file, t.platform_id AS platform_id
             FROM json_each(?1)
             JOIN roms r ON r.match_name = json_extract(json_each.value, '$[0]')
             JOIN titles t ON t.id = r.title_id
             WHERE ",
            live_title!()
        )
    };
}

/// As `name_tier!`, by base name and size, through `roms_match_base` whatever the
/// sizes, which a lookup ordered by rom id would otherwise trade for `roms_size`.
macro_rules! base_tier {
    () => {
        concat!(
            "SELECT json_each.key AS file, t.platform_id AS platform_id
             FROM json_each(?1)
             JOIN roms r INDEXED BY roms_match_base
               ON ",
            match_base!(),
            " = json_extract(json_each.value, '$[1]')
              AND r.size = json_extract(json_each.value, '$[2]')
             JOIN titles t ON t.id = r.title_id
             WHERE ",
            live_title!()
        )
    };
}

/// Which name tiers [`platform_hits`] scores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Tiers {
    /// Name, and base name with size: binding's rule.
    All,
    /// Name alone.
    Name,
    /// Base name with size alone.
    Base,
}

impl Tiers {
    fn sql(self) -> &'static str {
        match self {
            Self::All => concat!(
                "SELECT platform_id, COUNT(DISTINCT file) FROM (",
                name_tier!(),
                " UNION ALL ",
                base_tier!(),
                ") GROUP BY platform_id ORDER BY platform_id"
            ),
            Self::Name => concat!(
                "SELECT platform_id, COUNT(DISTINCT file) FROM (",
                name_tier!(),
                ") GROUP BY platform_id ORDER BY platform_id"
            ),
            Self::Base => concat!(
                "SELECT platform_id, COUNT(DISTINCT file) FROM (",
                base_tier!(),
                ") GROUP BY platform_id ORDER BY platform_id"
            ),
        }
    }
}

/// `keys` as the JSON list [`platform_hits`] binds.
fn key_list(keys: &[binding::MatchKeys]) -> Result<String> {
    let rows: Vec<(&str, &str, i64)> = keys
        .iter()
        .map(|k| (k.name.as_str(), k.base(), sql::to_i64(k.size)))
        .collect();
    Ok(serde_json::to_string(&rows)?)
}

/// How many of `keys` each platform matches under `tiers`, by platform id, in one
/// statement over the keys bound as a list; with [`Tiers::All`] it equals
/// [`binding::platform_hits`] through [`SqlDatIndex`]. Reads only and stores nothing.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn platform_hits(
    conn: &Connection,
    keys: &[binding::MatchKeys],
    tiers: Tiers,
) -> Result<Vec<(PlatformId, u64)>> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let hits = conn
        .prepare_cached(tiers.sql())?
        .query_map([key_list(keys)?], |r| Ok((r.get(0)?, sql::get_u64(r, 1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(hits)
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

    fn lookup(&self, sql: &str, args: impl rusqlite::Params) -> Vec<(PlatformId, RomId)> {
        let run = || -> rusqlite::Result<Vec<(PlatformId, RomId)>> {
            let mut stmt = self.conn.prepare_cached(sql)?;
            let rows = stmt.query_map(args, |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect()
        };
        run().unwrap_or_else(|e| {
            tracing::warn!(error = %e, "rom lookup failed");
            Vec::new()
        })
    }
}

impl DatIndex for SqlDatIndex<'_> {
    fn by_normalized_name(&self, name: &str) -> Vec<(PlatformId, RomId)> {
        self.lookup(
            concat!(
                "SELECT t.platform_id, r.id FROM roms r JOIN titles t ON t.id = r.title_id
                 WHERE r.match_name = ?1 AND ",
                live_title!(),
                " ORDER BY r.id"
            ),
            [name],
        )
    }

    fn by_base_name_and_size(&self, base_name: &str, size: u64) -> Vec<(PlatformId, RomId)> {
        self.lookup(
            concat!(
                "SELECT t.platform_id, r.id
                 FROM roms r INDEXED BY roms_match_base JOIN titles t ON t.id = r.title_id
                 WHERE ",
                match_base!(),
                " = ?1 AND r.size = ?2 AND ",
                live_title!(),
                " ORDER BY r.id"
            ),
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
        PlatformId::new("nes")
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
        let snes = PlatformId::new("snes");
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
        assert!(get(&c, SourceId::new(99)).expect("get").is_none());
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
        c.execute(
            "UPDATE sources SET client_id = 'x' WHERE id = ?1",
            [b.get()],
        )
        .expect("unreadable id");
        assert_eq!(list_resolving(&c).expect("resolving"), [(b, true)]);
        assert!(list_in_client(&c).expect("in client").is_empty());
        assert_eq!(get(&c, b).expect("get").expect("row").client_id, None);
        assert_eq!(SourceId::new(3).to_string(), "3");
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
                (0, Some(rom), Confidence::Name),
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
        let dirs: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM torrent_dirs WHERE source_id = ?1",
                [id],
                |r| r.get(0),
            )
            .expect("dirs");
        assert_eq!(dirs, 0, "the source's directories go with it");
    }

    #[test]
    fn a_re_import_keeps_only_the_directories_its_files_use() {
        let c = conn();
        let id = insert(&c, &new(&"06".repeat(20), SourceState::Bound)).expect("insert");
        let dirs = |id| {
            c.prepare("SELECT path FROM torrent_dirs WHERE source_id = ?1 ORDER BY path")
                .expect("prepare")
                .query_map([id], |r| r.get(0))
                .expect("query")
                .collect::<rusqlite::Result<Vec<String>>>()
                .expect("rows")
        };
        replace_files(&c, id, &[file(0, "One/a.nes", 8), file(1, "Two/b.nes", 8)]).expect("files");
        assert_eq!(dirs(id), ["One/", "Two/"]);
        replace_files(&c, id, &[file(0, "One/a.nes", 8)]).expect("re-import");
        assert_eq!(dirs(id), ["One/"], "the tree that is gone leaves nothing");
    }

    /// A file with a candidate and no matched rom counts as matched in `get` and in
    /// `list`, as the full walk over the source's files counts it.
    #[test]
    fn a_candidate_only_file_counts_as_matched() {
        let c = conn();
        let rom = seed_rom(&c, &pid("nes"), "Example Quest (USA).nes", 16, &[]).expect("rom");
        let other = seed_rom(&c, &pid("nes"), "Second Try (Japan).nes", 24, &[]).expect("rom");
        let id = insert(&c, &new(&"05".repeat(20), SourceState::Bound)).expect("insert");
        let set = [
            file(0, "Sub/Example Quest (USA).nes", 16),
            file(1, "Sub/Second Guess (USA).nes", 24),
            file(2, "Sub/Unlisted (USA).nes", 8),
        ];
        replace_files(&c, id, &set).expect("files");
        set_matches(
            &c,
            id,
            &[
                (0, Some(rom), Confidence::Name),
                (1, None, Confidence::Unmatched),
            ],
        )
        .expect("matches");
        c.execute(
            "INSERT INTO torrent_candidates (source_id, file_index, rom_id, confidence)
             VALUES (?1, 1, ?2, ?3)",
            params![id, other, MatchConfidence::Fuzzy],
        )
        .expect("candidate");
        // The full walk over every file row, as the reference for the split count.
        let walked: u64 = c
            .query_row(
                "SELECT COUNT(*) FROM torrent_files f WHERE f.source_id = ?1
                   AND (f.rom_id IS NOT NULL OR EXISTS (SELECT 1 FROM torrent_candidates c
                         WHERE c.source_id = f.source_id AND c.file_index = f.file_index))",
                [id],
                |r| sql::get_u64(r, 0),
            )
            .expect("count");
        assert_eq!(walked, 2, "the match and the candidate");
        let row = get(&c, id).expect("get").expect("row");
        assert_eq!(row.matched_count, walked);
        let page = list(
            &c,
            Page {
                limit: 10,
                offset: 0,
            },
        )
        .expect("list");
        assert_eq!(page.items[0].matched_count, walked);
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
            [(nes(), a)]
        );
        assert_eq!(
            index.by_base_name_and_size("other tale", 32),
            [(PlatformId::new("snes"), b)]
        );
        assert!(index.by_base_name_and_size("other tale", 33).is_empty());
        assert!(index.by_normalized_name("boot code (world)").is_empty());
    }

    #[test]
    fn platform_hits_count_each_file_once_per_platform_and_tier() {
        let c = conn();
        seed_rom(&c, &pid("nes"), "Example Quest (USA).nes", 16, &[]).expect("rom");
        seed_rom(&c, &pid("nes"), "Example Quest (Europe).nes", 16, &[]).expect("rom");
        seed_rom(&c, &pid("snes"), "Example Quest (Japan).sfc", 16, &[]).expect("rom");
        seed_rom(&c, &pid("gb"), "Example Quest (USA).gb", 16, &["bios"]).expect("rom");
        refresh_match_keys(&c).expect("keys");
        let keys: Vec<binding::MatchKeys> = [
            file(0, "Set/Example Quest (USA).nes", 16),
            file(1, "Set/Example Quest (Beta).nes", 16),
            file(2, "Set/Example Quest (Beta).nes", 17),
        ]
        .iter()
        .map(binding::MatchKeys::of)
        .collect();
        let hits = |tiers| platform_hits(&c, &keys, tiers).expect("hits");
        let snes = PlatformId::new("snes");
        assert_eq!(hits(Tiers::All), [(nes(), 2), (snes.clone(), 2)]);
        assert_eq!(hits(Tiers::Name), [(nes(), 1)]);
        assert_eq!(hits(Tiers::Base), [(nes(), 2), (snes, 2)]);
        assert!(platform_hits(&c, &[], Tiers::All).expect("none").is_empty());
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig {
            cases: 48,
            failure_persistence: None,
            ..proptest::prelude::ProptestConfig::default()
        })]

        /// The preview's one statement scores a chunk exactly as binding's per-file
        /// lookups do, over roms with shared names, bases and sizes, BIOS and retired.
        #[test]
        fn one_statement_scores_as_the_per_file_lookups(
            roms in proptest::collection::vec(
                ("[ab]{1,2}", 0u8..3, 0u8..3, 1u64..3, 0u8..6), 1..14),
            files in proptest::collection::vec(("[ab]{1,2}", 0u8..4, 1u64..3), 0..14),
        ) {
            const TAGS: [&str; 4] = ["", " (USA)", " (Europe)", " (Beta)"];
            const PLATFORMS: [&str; 3] = ["nes", "snes", "gb"];
            let c = conn();
            let mut seeded = std::collections::HashSet::new();
            for (name, tag, p, size, kind) in &roms {
                let rom_name = format!("{name}{}.bin", TAGS[usize::from(*tag)]);
                // A DAT names each title once.
                if !seeded.insert((rom_name.clone(), *p)) {
                    continue;
                }
                let flags: &[&str] = if *kind == 0 { &["bios"] } else { &[] };
                let id = seed_rom(&c, &pid(PLATFORMS[usize::from(*p)]), &rom_name, *size, flags)
                    .expect("rom");
                if *kind == 1 {
                    c.execute(
                        "UPDATE titles SET retired = 1
                         WHERE id = (SELECT title_id FROM roms WHERE id = ?1)",
                        [id],
                    )
                    .expect("retire");
                }
            }
            refresh_match_keys(&c).expect("keys");
            let files: Vec<TorrentFile> = files
                .iter()
                .enumerate()
                .map(|(i, (name, tag, size))| {
                    let path = format!("Set/{name}{}.bin", TAGS[usize::from(*tag)]);
                    file(u32::try_from(i).unwrap_or(0), &path, *size)
                })
                .collect();
            let keys: Vec<binding::MatchKeys> = files.iter().map(binding::MatchKeys::of).collect();
            let per_file: Vec<(PlatformId, u64)> =
                binding::platform_hits(&files, &SqlDatIndex::new(&c))
                    .into_iter()
                    .map(|(p, n)| (p, n as u64))
                    .collect();
            let statement = platform_hits(&c, &keys, Tiers::All).expect("hits");
            proptest::prop_assert_eq!(statement, per_file);
        }
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
    fn seed_and_bind_codes_round_trip() {
        for kind in SeedKind::ALL {
            assert_eq!(SeedKind::parse(kind.as_str()), Some(*kind));
        }
        for kind in BindKind::ALL {
            assert_eq!(BindKind::parse(kind.as_str()), Some(*kind));
        }
        let mut seed: Vec<i64> = SeedKind::ALL.iter().map(|k| k.code()).collect();
        seed.sort_unstable();
        assert_eq!(seed, [0, 1, 2]);
        let mut bind: Vec<i64> = BindKind::ALL.iter().map(|k| k.code()).collect();
        bind.sort_unstable();
        assert_eq!(bind, [0, 1, 2]);
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
    fn a_path_splits_into_its_directory_and_leaf() {
        assert_eq!(split_path(""), ("", ""));
        assert_eq!(split_path("Set/Sub/pack.nes"), ("Set/Sub/", "pack.nes"));
        assert_eq!(split_path("pack.nes"), ("", "pack.nes"));
        assert_eq!(
            split_path("Set 1.0/pack (USA) v1.2.nes"),
            ("Set 1.0/", "pack (USA) v1.2.nes")
        );
        assert_eq!(
            split_path("Sét/Ünïcode/но́ва.nes"),
            ("Sét/Ünïcode/", "но́ва.nes")
        );
        assert_eq!(split_path("Set/"), ("Set/", ""));
        for path in ["", "a", "a/b", "Set/1.0/x.nes", "Sét/но́ва.nes"] {
            let (directory, leaf) = split_path(path);
            assert_eq!(
                format!("{directory}{leaf}"),
                path,
                "{path} does not round trip"
            );
        }
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
