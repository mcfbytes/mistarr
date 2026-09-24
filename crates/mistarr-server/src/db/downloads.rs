//! The `downloads` table and its state machine; see `docs/DATA-MODEL.md`.

use std::fmt;

use mistarr_core::PlatformId;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use super::sources::SourceId;
use super::titles::TitleId;
use crate::error::Result;

/// A `downloads.id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DownloadId(pub i64);

impl fmt::Display for DownloadId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// `downloads.state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DownloadState {
    /// Title marked, no `torrent_file` chosen yet.
    Wanted,
    /// `torrent_file` chosen, not yet started in the client.
    Queued,
    /// The client is fetching the file.
    Transferring,
    /// The client has every byte and is still checking them.
    Checking,
    /// Handed to the importer.
    Importing,
    /// Placed.
    Done,
    /// Hash mismatch, quarantined.
    Bad,
    /// Stopped by an error; may be retried.
    Failed,
    /// Stopped by the user.
    Cancelled,
}

/// States a download leaves only by being worked on or cancelled.
const OPEN: &str = "'wanted', 'queued', 'transferring', 'checking', 'importing'";
/// States in which the file is, or is about to be, selected in the client.
const SELECTED: &str = "'queued', 'transferring', 'checking', 'importing'";

impl DownloadState {
    /// Every state, in state-machine order.
    pub const ALL: [Self; 9] = [
        Self::Wanted,
        Self::Queued,
        Self::Transferring,
        Self::Checking,
        Self::Importing,
        Self::Done,
        Self::Bad,
        Self::Failed,
        Self::Cancelled,
    ];

    /// The column value.
    ///
    /// ```
    /// use mistarr_server::db::downloads::DownloadState;
    /// assert_eq!(DownloadState::Checking.as_str(), "checking");
    /// ```
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Wanted => "wanted",
            Self::Queued => "queued",
            Self::Transferring => "transferring",
            Self::Checking => "checking",
            Self::Importing => "importing",
            Self::Done => "done",
            Self::Bad => "bad",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Parses a column value.
    ///
    /// ```
    /// use mistarr_server::db::downloads::DownloadState;
    /// assert_eq!(DownloadState::parse("queued"), Some(DownloadState::Queued));
    /// assert_eq!(DownloadState::parse("x"), None);
    /// ```
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }

    /// True for `done`, `bad`, `failed` and `cancelled`.
    ///
    /// ```
    /// use mistarr_server::db::downloads::DownloadState;
    /// assert!(DownloadState::Failed.is_terminal());
    /// assert!(!DownloadState::Importing.is_terminal());
    /// ```
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Done | Self::Bad | Self::Failed | Self::Cancelled
        )
    }

    /// True while the client transfers or checks the file.
    ///
    /// ```
    /// use mistarr_server::db::downloads::DownloadState;
    /// assert!(DownloadState::Checking.started());
    /// assert!(!DownloadState::Queued.started());
    /// ```
    #[must_use]
    pub fn started(self) -> bool {
        matches!(self, Self::Transferring | Self::Checking)
    }

    /// Whether the state machine has an edge from `self` to `to`.
    ///
    /// ```
    /// use mistarr_server::db::downloads::DownloadState::*;
    /// assert!(Failed.can_become(Queued));
    /// assert!(!Bad.can_become(Queued));
    /// ```
    #[must_use]
    pub fn can_become(self, to: Self) -> bool {
        use DownloadState::{
            Bad, Cancelled, Checking, Done, Failed, Importing, Queued, Transferring, Wanted,
        };
        matches!(
            (self, to),
            (Wanted, Queued | Cancelled)
                | (Queued, Transferring | Failed | Cancelled)
                | (Transferring, Checking | Importing | Failed | Cancelled)
                | (Checking, Transferring | Importing | Failed | Cancelled)
                | (Importing, Done | Bad | Failed)
                | (Failed, Queued)
        )
    }
}

impl fmt::Display for DownloadState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One download with the names the downloads screen shows.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DownloadRow {
    /// Row id.
    pub id: DownloadId,
    /// The wanted title.
    pub title_id: TitleId,
    /// Its full DAT name.
    pub title_name: String,
    /// Its platform.
    pub platform_id: PlatformId,
    /// The rom being fetched.
    pub rom_id: i64,
    /// The rom's DAT file name.
    pub rom_name: String,
    /// The rom's size in bytes.
    pub size: u64,
    /// The source holding the file; `None` while `wanted`.
    pub source_id: Option<SourceId>,
    /// The file's index in that torrent.
    pub file_index: Option<u32>,
    /// Lifecycle state.
    pub state: DownloadState,
    /// 0 to 1.
    pub progress: f64,
    /// Where the finished file sits under `staging/`, once `importing`.
    pub staged_path: Option<String>,
    /// Why it failed.
    pub error: Option<String>,
    /// Unix seconds.
    pub created_at: i64,
    /// Unix seconds.
    pub updated_at: i64,
}

const COLUMNS: &str = "d.id, d.title_id, t.name, t.platform_id, d.rom_id, r.name, r.size,
    d.source_id, d.file_index, d.state, d.progress, d.staged_path, d.error, d.created_at,
    d.updated_at";
const FROM: &str = "downloads d JOIN titles t ON t.id = d.title_id JOIN roms r ON r.id = d.rom_id";

fn uint(n: i64) -> u64 {
    u64::try_from(n).unwrap_or(0)
}

fn state_of(text: &str) -> DownloadState {
    DownloadState::parse(text).unwrap_or(DownloadState::Failed)
}

fn from_row(r: &Row<'_>) -> rusqlite::Result<DownloadRow> {
    Ok(DownloadRow {
        id: DownloadId(r.get(0)?),
        title_id: TitleId(r.get(1)?),
        title_name: r.get(2)?,
        platform_id: PlatformId(r.get(3)?),
        rom_id: r.get(4)?,
        rom_name: r.get(5)?,
        size: uint(r.get(6)?),
        source_id: r.get::<_, Option<i64>>(7)?.map(SourceId),
        file_index: r.get(8)?,
        state: state_of(&r.get::<_, String>(9)?),
        progress: r.get(10)?,
        staged_path: r.get(11)?,
        error: r.get(12)?,
        created_at: r.get(13)?,
        updated_at: r.get(14)?,
    })
}

fn states_json(states: &[DownloadState]) -> String {
    let names: Vec<&str> = states.iter().map(|s| s.as_str()).collect();
    serde_json::to_string(&names).unwrap_or_else(|_| "[]".to_owned())
}

/// A download to insert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewDownload {
    /// The wanted title.
    pub title_id: TitleId,
    /// The rom to fetch.
    pub rom_id: i64,
    /// The chosen `torrent_file`; `None` makes the row `wanted`, else `queued`.
    pub file: Option<Candidate>,
    /// Unix seconds.
    pub now: i64,
}

/// Inserts a download in `queued` when a file is chosen, else `wanted`.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure, e.g. an unknown title or rom.
pub fn create(conn: &Connection, d: &NewDownload) -> Result<DownloadId> {
    let state = if d.file.is_some() {
        DownloadState::Queued
    } else {
        DownloadState::Wanted
    };
    conn.execute(
        "INSERT INTO downloads (title_id, rom_id, source_id, file_index, state, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
        params![
            d.title_id.0,
            d.rom_id,
            d.file.map(|c| c.source_id.0),
            d.file.map(|c| c.file_index),
            state.as_str(),
            d.now
        ],
    )?;
    Ok(DownloadId(conn.last_insert_rowid()))
}

/// Reads one download.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn get(conn: &Connection, id: DownloadId) -> Result<Option<DownloadRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM {FROM} WHERE d.id = ?1"),
            [id.0],
            from_row,
        )
        .optional()?)
}

/// Downloads in any of `states`, or all when empty, most recently changed
/// first, with the total before paging.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn list(
    conn: &Connection,
    states: &[DownloadState],
    limit: u32,
    offset: u32,
) -> Result<(Vec<DownloadRow>, u64)> {
    let filter = "(?1 = '[]' OR d.state IN (SELECT value FROM json_each(?1)))";
    let states = states_json(states);
    let total: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM downloads d WHERE {filter}"),
        [&states],
        |r| r.get(0),
    )?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM {FROM} WHERE {filter}
         ORDER BY d.updated_at DESC, d.id DESC LIMIT ?2 OFFSET ?3"
    ))?;
    let rows = stmt
        .query_map(params![states, limit, offset], from_row)?
        .collect::<rusqlite::Result<_>>()?;
    Ok((rows, uint(total)))
}

/// The newest download of file `index` in `source` that is not finished,
/// else the newest one of any state.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn find_by_file(
    conn: &Connection,
    source: SourceId,
    index: u32,
) -> Result<Option<DownloadRow>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {COLUMNS} FROM {FROM} WHERE d.source_id = ?1 AND d.file_index = ?2
                 ORDER BY d.state IN ({OPEN}) DESC, d.id DESC LIMIT 1"
            ),
            params![source.0, index],
            from_row,
        )
        .optional()?)
}

/// Moves a download to `to` if the state machine allows it from its current
/// state; returns whether it moved. Entering `queued` clears the error and progress.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn set_state(conn: &Connection, id: DownloadId, to: DownloadState, now: i64) -> Result<bool> {
    let Some(from) = current(conn, id)? else {
        return Ok(false);
    };
    if !from.can_become(to) {
        return Ok(false);
    }
    let reset = to == DownloadState::Queued;
    let n = conn.execute(
        "UPDATE downloads SET state = ?3, updated_at = ?4,
           error = CASE WHEN ?5 THEN NULL ELSE error END,
           progress = CASE WHEN ?5 THEN 0 ELSE progress END
         WHERE id = ?1 AND state = ?2",
        params![id.0, from.as_str(), to.as_str(), now, reset],
    )?;
    Ok(n > 0)
}

fn current(conn: &Connection, id: DownloadId) -> Result<Option<DownloadState>> {
    let s: Option<String> = conn
        .query_row("SELECT state FROM downloads WHERE id = ?1", [id.0], |r| {
            r.get(0)
        })
        .optional()?;
    Ok(s.as_deref().map(state_of))
}

/// What the poller observed for one download.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Observed<'a> {
    /// The state it maps to.
    pub state: DownloadState,
    /// 0 to 1.
    pub progress: f64,
    /// Local path of the finished file.
    pub staged_path: Option<&'a str>,
    /// Why it failed.
    pub error: Option<&'a str>,
}

/// Stores what the poller observed; returns true only when something changed.
/// A state the machine does not allow from the current one is ignored.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn observe(conn: &Connection, id: DownloadId, o: &Observed<'_>, now: i64) -> Result<bool> {
    let Some(from) = current(conn, id)? else {
        return Ok(false);
    };
    if from != o.state && !from.can_become(o.state) {
        return Ok(false);
    }
    let n = conn.execute(
        "UPDATE downloads SET state = ?3, progress = ?4, staged_path = ?5, error = ?6,
           updated_at = ?7
         WHERE id = ?1 AND state = ?2
           AND (state IS NOT ?3 OR progress IS NOT ?4 OR staged_path IS NOT ?5 OR error IS NOT ?6)",
        params![
            id.0,
            from.as_str(),
            o.state.as_str(),
            o.progress,
            o.staged_path,
            o.error,
            now
        ],
    )?;
    Ok(n > 0)
}

/// The result of [`retry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryOutcome {
    /// Back in `queued`.
    Queued,
    /// No such download.
    Missing,
    /// The download is not `failed`; its state is given.
    NotFailed(DownloadState),
    /// Another open download already fetches the same rom.
    Busy,
}

/// Returns a `failed` download to `queued`.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn retry(conn: &Connection, id: DownloadId, now: i64) -> Result<RetryOutcome> {
    let Some(row) = get(conn, id)? else {
        return Ok(RetryOutcome::Missing);
    };
    if row.state != DownloadState::Failed {
        return Ok(RetryOutcome::NotFailed(row.state));
    }
    let busy: bool = conn.query_row(
        &format!(
            "SELECT EXISTS (SELECT 1 FROM downloads WHERE rom_id = ?1 AND id != ?2
                            AND state IN ({OPEN}))"
        ),
        params![row.rom_id, id.0],
        |r| r.get(0),
    )?;
    if busy {
        return Ok(RetryOutcome::Busy);
    }
    set_state(conn, id, DownloadState::Queued, now)?;
    Ok(RetryOutcome::Queued)
}

/// A download [`cancel`] or [`cancel_group`] stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled {
    /// The download.
    pub id: DownloadId,
    /// Its source, if one was chosen.
    pub source_id: Option<SourceId>,
    /// True when the client was already transferring or checking it.
    pub started: bool,
}

/// The result of [`cancel`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelOutcome {
    /// Now `cancelled`.
    Cancelled(Cancelled),
    /// No such download.
    Missing,
    /// Importing or finished, which cannot be cancelled; its state is given.
    Final(DownloadState),
}

/// Cancels one download that is not importing or finished.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn cancel(conn: &Connection, id: DownloadId, now: i64) -> Result<CancelOutcome> {
    let Some(row) = get(conn, id)? else {
        return Ok(CancelOutcome::Missing);
    };
    if !set_state(conn, id, DownloadState::Cancelled, now)? {
        return Ok(CancelOutcome::Final(row.state));
    }
    Ok(CancelOutcome::Cancelled(Cancelled {
        id,
        source_id: row.source_id,
        started: row.state.started(),
    }))
}

/// Cancels every download of the clone group rooted at `parent` that is not
/// importing or finished.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn cancel_group(conn: &Connection, parent: TitleId, now: i64) -> Result<Vec<Cancelled>> {
    let mut stmt = conn.prepare(
        "SELECT d.id, d.source_id, d.state IN ('transferring', 'checking') FROM downloads d
         JOIN titles t ON t.id = d.title_id
         WHERE (t.group_root = ?1 OR t.id = ?1)
           AND d.state IN ('wanted', 'queued', 'transferring', 'checking')
         ORDER BY d.id",
    )?;
    let found: Vec<Cancelled> = stmt
        .query_map([parent.0], |r| {
            Ok(Cancelled {
                id: DownloadId(r.get(0)?),
                source_id: r.get::<_, Option<i64>>(1)?.map(SourceId),
                started: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut update = conn.prepare_cached(
        "UPDATE downloads SET state = 'cancelled', updated_at = ?2 WHERE id = ?1",
    )?;
    for c in &found {
        update.execute(params![c.id.0, now])?;
    }
    Ok(found)
}

/// A `torrent_file` chosen for a rom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Candidate {
    /// The source.
    pub source_id: SourceId,
    /// The file's index in its torrent.
    pub file_index: u32,
}

/// The best `torrent_file` for `rom` across bound sources: an exact size match
/// first, then a name match over a size match, then the source with fewer
/// selected downloads, then the lowest source id. A file that already gave
/// this rom a `bad` download is never chosen.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn best_file(conn: &Connection, rom: i64) -> Result<Option<Candidate>> {
    Ok(conn
        .prepare_cached(&format!(
            "SELECT tf.source_id, tf.file_index FROM torrent_files tf
             JOIN sources s ON s.id = tf.source_id AND s.state = 'bound'
             JOIN roms r ON r.id = tf.rom_id
             WHERE tf.rom_id = ?1
               AND NOT EXISTS (SELECT 1 FROM downloads b
                               WHERE b.rom_id = tf.rom_id AND b.state = 'bad'
                                 AND b.source_id = tf.source_id AND b.file_index = tf.file_index)
             ORDER BY tf.size = r.size DESC, COALESCE(tf.confidence = 'name', 0) DESC,
               (SELECT COUNT(*) FROM downloads a
                WHERE a.source_id = s.id AND a.state IN ({SELECTED})),
               s.id, tf.file_index
             LIMIT 1"
        ))?
        .query_row([rom], |r| {
            Ok(Candidate {
                source_id: SourceId(r.get(0)?),
                file_index: r.get(1)?,
            })
        })
        .optional()?)
}

/// Creates a download for each live rom of `title` that has no verified file, is not
/// an MRA zip already present,
/// and has no open download, `queued` on its [`best_file`] or `wanted` without one.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn want_title(
    conn: &Connection,
    title: TitleId,
    now: i64,
) -> Result<Vec<(DownloadId, DownloadState)>> {
    let roms: Vec<i64> = conn
        .prepare(&format!(
            "SELECT r.id FROM roms r WHERE r.title_id = ?1 AND r.retired = 0
               AND r.present = 0
               AND NOT EXISTS (SELECT 1 FROM files f WHERE f.rom_id = r.id AND f.state = 'verified')
               AND NOT EXISTS (SELECT 1 FROM downloads d WHERE d.rom_id = r.id AND d.state IN ({OPEN}))
             ORDER BY r.id"
        ))?
        .query_map([title.0], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::with_capacity(roms.len());
    for rom_id in roms {
        let file = best_file(conn, rom_id)?;
        let id = create(
            conn,
            &NewDownload {
                title_id: title,
                rom_id,
                file,
                now,
            },
        )?;
        let state = if file.is_some() {
            DownloadState::Queued
        } else {
            DownloadState::Wanted
        };
        out.push((id, state));
    }
    Ok(out)
}

/// Chooses a `torrent_file` for every `wanted` download that now has one and
/// moves it to `queued`. Returns the downloads moved.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn promote_wanted(conn: &Connection, now: i64) -> Result<Vec<DownloadId>> {
    let waiting: Vec<(i64, i64)> = conn
        .prepare("SELECT id, rom_id FROM downloads WHERE state = 'wanted' ORDER BY id")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut moved = Vec::new();
    for (id, rom) in waiting {
        let Some(c) = best_file(conn, rom)? else {
            continue;
        };
        conn.execute(
            "UPDATE downloads SET state = 'queued', source_id = ?2, file_index = ?3, updated_at = ?4
             WHERE id = ?1 AND state = 'wanted'",
            params![id, c.source_id.0, c.file_index, now],
        )?;
        moved.push(DownloadId(id));
    }
    Ok(moved)
}

/// Sources with `queued` downloads, by id.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn queued_sources(conn: &Connection) -> Result<Vec<SourceId>> {
    let ids = conn
        .prepare(
            "SELECT DISTINCT source_id FROM downloads
             WHERE state = 'queued' AND source_id IS NOT NULL ORDER BY source_id",
        )?
        .query_map([], |r| r.get(0).map(SourceId))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(ids)
}

/// Downloads of `source` in any of `states`, by id.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn of_source(
    conn: &Connection,
    source: SourceId,
    states: &[DownloadState],
) -> Result<Vec<DownloadRow>> {
    let rows = conn
        .prepare(&format!(
            "SELECT {COLUMNS} FROM {FROM}
             WHERE d.source_id = ?1 AND d.state IN (SELECT value FROM json_each(?2))
             ORDER BY d.id"
        ))?
        .query_map(params![source.0, states_json(states)], from_row)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// File indices of `source` that should be selected in the client: those of
/// downloads queued, transferring, checking or importing, ascending.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn selected_indices(conn: &Connection, source: SourceId) -> Result<Vec<u32>> {
    let ids = conn
        .prepare(&format!(
            "SELECT DISTINCT file_index FROM downloads
             WHERE source_id = ?1 AND file_index IS NOT NULL AND state IN ({SELECTED})
             ORDER BY file_index"
        ))?
        .query_map([source.0], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(ids)
}

/// Moves each of `ids` to `to` where the state machine allows it, storing
/// `error` when given. Returns the downloads that moved.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn move_all(
    conn: &Connection,
    ids: &[DownloadId],
    to: DownloadState,
    error: Option<&str>,
    now: i64,
) -> Result<Vec<DownloadId>> {
    let mut moved = Vec::new();
    for &id in ids {
        if set_state(conn, id, to, now)? {
            if error.is_some() {
                conn.execute(
                    "UPDATE downloads SET error = ?2 WHERE id = ?1",
                    params![id.0, error],
                )?;
            }
            moved.push(id);
        }
    }
    Ok(moved)
}

/// A transferring or checking download with what the poller needs to read
/// its file's progress and name its staged path.
#[derive(Debug, Clone, PartialEq)]
pub struct PollRow {
    /// The download.
    pub id: DownloadId,
    /// Its state.
    pub state: DownloadState,
    /// Its progress.
    pub progress: f64,
    /// Its staged path, if set.
    pub staged_path: Option<String>,
    /// Its source.
    pub source_id: SourceId,
    /// The file's index.
    pub file_index: u32,
    /// The file's path inside the torrent.
    pub path: String,
    /// The source's lowercase hex infohash.
    pub infohash: String,
    /// The torrent's name.
    pub torrent_name: String,
    /// True when the torrent is one file stored under the torrent's name.
    pub single_file: bool,
    /// The source's id in the client.
    pub client_id: Option<String>,
    /// The source's seed policy text.
    pub seed_policy: String,
    /// The file's size in bytes, from the metainfo.
    pub size: u64,
}

/// Every transferring or checking download, grouped by source.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn polled(conn: &Connection) -> Result<Vec<PollRow>> {
    let rows = conn
        .prepare(
            "SELECT d.id, d.state, d.progress, d.staged_path, d.source_id, d.file_index,
                    tf.path, s.infohash, s.display_name,
                    s.file_count = 1 AND tf.path = s.display_name, s.client_id, s.seed_policy, tf.size
             FROM downloads d
             JOIN sources s ON s.id = d.source_id
             JOIN torrent_files tf ON tf.source_id = d.source_id AND tf.file_index = d.file_index
             WHERE d.state IN ('transferring', 'checking')
             ORDER BY d.source_id, d.id",
        )?
        .query_map([], |r| {
            Ok(PollRow {
                id: DownloadId(r.get(0)?),
                state: state_of(&r.get::<_, String>(1)?),
                progress: r.get(2)?,
                staged_path: r.get(3)?,
                source_id: SourceId(r.get(4)?),
                file_index: r.get(5)?,
                path: r.get(6)?,
                infohash: r.get(7)?,
                torrent_name: r.get(8)?,
                single_file: r.get(9)?,
                client_id: r.get(10)?,
                seed_policy: r.get(11)?,
                size: u64::try_from(r.get::<_, i64>(12)?).unwrap_or(0),
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// How many downloads are in any of `states`.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn count_in(conn: &Connection, states: &[DownloadState]) -> Result<u64> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM downloads WHERE state IN (SELECT value FROM json_each(?1))",
        [states_json(states)],
        |r| r.get(0),
    )?;
    Ok(uint(n))
}

#[cfg(test)]
mod tests;
