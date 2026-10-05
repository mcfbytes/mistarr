//! The `downloads` table and its state machine; see `docs/DATA-MODEL.md`.

use mistarr_clients::ClientTorrentId;
use mistarr_core::PlatformId;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row};
use serde::Serialize;

use super::candidates;
use super::ids::{DownloadId, RomId, SourceId, TitleId};
use super::sources;
use super::sql::{self, text_enum, Page, Paged};
use crate::error::Result;

text_enum! {
    /// `downloads.state`.
    pub enum DownloadState {
        /// Title marked, no `torrent_file` chosen yet.
        Wanted = "wanted",
        /// `torrent_file` chosen, not yet started in the client.
        Queued = "queued",
        /// The client is fetching the file.
        Transferring = "transferring",
        /// The client has every byte and is still checking them.
        Checking = "checking",
        /// Handed to the importer.
        Importing = "importing",
        /// Placed.
        Done = "done",
        /// Hash mismatch, quarantined.
        Bad = "bad",
        /// Stopped by an error; may be retried.
        Failed = "failed",
        /// Stopped by the user.
        Cancelled = "cancelled",
    }
}

impl DownloadState {
    /// States a download leaves only by being worked on or cancelled; every other is terminal.
    pub const OPEN: [Self; 5] = [
        Self::Wanted,
        Self::Queued,
        Self::Transferring,
        Self::Checking,
        Self::Importing,
    ];
    /// [`DownloadState::OPEN`] as an SQL list.
    pub const OPEN_SQL: &'static str =
        "('wanted', 'queued', 'transferring', 'checking', 'importing')";

    /// States in which the file is, or is about to be, selected in the client.
    pub const SELECTED: [Self; 4] = [
        Self::Queued,
        Self::Transferring,
        Self::Checking,
        Self::Importing,
    ];
    /// [`DownloadState::SELECTED`] as an SQL list.
    pub const SELECTED_SQL: &'static str = "('queued', 'transferring', 'checking', 'importing')";

    /// States in which the client transfers or checks the file.
    pub const STARTED: [Self; 2] = [Self::Transferring, Self::Checking];
    /// [`DownloadState::STARTED`] as an SQL list.
    pub const STARTED_SQL: &'static str = "('transferring', 'checking')";

    /// States a cancel stops: every open one but `importing`.
    pub const CANCELLABLE: [Self; 4] = [
        Self::Wanted,
        Self::Queued,
        Self::Transferring,
        Self::Checking,
    ];
    /// [`DownloadState::CANCELLABLE`] as an SQL list.
    pub const CANCELLABLE_SQL: &'static str = "('wanted', 'queued', 'transferring', 'checking')";

    /// States in which the client has not been asked for the file yet.
    pub const UNSTARTED: [Self; 2] = [Self::Wanted, Self::Queued];
    /// [`DownloadState::UNSTARTED`] as an SQL list.
    pub const UNSTARTED_SQL: &'static str = "('wanted', 'queued')";

    /// True for `done`, `bad`, `failed` and `cancelled`.
    ///
    /// ```
    /// use mistarr_server::db::downloads::DownloadState;
    /// assert!(DownloadState::Failed.is_terminal());
    /// assert!(!DownloadState::Importing.is_terminal());
    /// ```
    #[must_use]
    pub fn is_terminal(self) -> bool {
        !Self::OPEN.contains(&self)
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
        Self::STARTED.contains(&self)
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
    pub rom_id: RomId,
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

fn from_row(r: &Row<'_>) -> rusqlite::Result<DownloadRow> {
    Ok(DownloadRow {
        id: r.get(0)?,
        title_id: r.get(1)?,
        title_name: r.get(2)?,
        platform_id: PlatformId(r.get(3)?),
        rom_id: r.get(4)?,
        rom_name: r.get(5)?,
        size: sql::get_u64(r, 6)?,
        source_id: r.get(7)?,
        file_index: r.get(8)?,
        state: r.get(9)?,
        progress: r.get(10)?,
        staged_path: r.get(11)?,
        error: r.get(12)?,
        created_at: r.get(13)?,
        updated_at: r.get(14)?,
    })
}

/// A download to insert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewDownload {
    /// The wanted title.
    pub title_id: TitleId,
    /// The rom to fetch.
    pub rom_id: RomId,
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
            d.title_id,
            d.rom_id,
            d.file.map(|c| c.source_id),
            d.file.map(|c| c.file_index),
            state,
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
            [id],
            from_row,
        )
        .optional()?)
}

/// Downloads in any of `states`, or all when empty, most recently changed
/// first, with the total before paging, both read in one snapshot.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn list(conn: &Connection, states: &[DownloadState], page: Page) -> Result<Paged<DownloadRow>> {
    let (filter, list) = if states.is_empty() {
        ("1", None)
    } else {
        let list = sql::json_list(states)?;
        ("d.state IN (SELECT value FROM json_each(?1))", Some(list))
    };
    sql::snapshot(conn, |c| {
        let total = c.query_row(
            &format!("SELECT COUNT(*) FROM downloads d WHERE {filter}"),
            params_from_iter(&list),
            |r| sql::get_u64(r, 0),
        )?;
        let items = c
            .prepare(&format!(
                "SELECT {COLUMNS} FROM {FROM} WHERE {filter}
                 ORDER BY d.updated_at DESC, d.id DESC LIMIT ?2 OFFSET ?3"
            ))?
            .query_map(params![list, page.limit, page.offset], from_row)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Paged { items, total })
    })
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
                 ORDER BY d.state IN {} DESC, d.id DESC LIMIT 1",
                DownloadState::OPEN_SQL
            ),
            params![source, index],
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
        params![id, from, to, now, reset],
    )?;
    Ok(n > 0)
}

fn current(conn: &Connection, id: DownloadId) -> Result<Option<DownloadState>> {
    Ok(conn
        .query_row("SELECT state FROM downloads WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .optional()?)
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
        params![id, from, o.state, o.progress, o.staged_path, o.error, now],
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
                            AND state IN {})",
            DownloadState::OPEN_SQL
        ),
        params![row.rom_id, id],
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
    let mut stmt = conn.prepare(&format!(
        "SELECT d.id, d.source_id, d.state IN {} FROM downloads d
         JOIN titles t ON t.id = d.title_id
         WHERE (t.group_root = ?1 OR (t.id = ?1 AND t.group_root IS NULL))
           AND d.state IN {}
         ORDER BY d.id",
        DownloadState::STARTED_SQL,
        DownloadState::CANCELLABLE_SQL
    ))?;
    let found: Vec<Cancelled> = stmt
        .query_map([parent], |r| {
            Ok(Cancelled {
                id: r.get(0)?,
                source_id: r.get(1)?,
                started: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut update = conn.prepare_cached(
        "UPDATE downloads SET state = 'cancelled', updated_at = ?2 WHERE id = ?1",
    )?;
    for c in &found {
        update.execute(params![c.id, now])?;
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

/// The best `torrent_file` for `rom` across bound sources: a hash-proven or
/// name-tier match before any fuzzy or size-only candidate, then, within
/// that tier, a size match (exact, or with the header the platform's hashing
/// skips on top), then the stronger confidence (hash, name, base, fuzzy,
/// size), then the source with fewer selected downloads, then the lowest
/// source id. A file that already gave this rom a `bad` download is never chosen.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn best_file(conn: &Connection, rom: RomId) -> Result<Option<Candidate>> {
    let platform: Option<String> = conn
        .query_row(
            "SELECT t.platform_id FROM roms r JOIN titles t ON t.id = r.title_id WHERE r.id = ?1",
            [rom],
            |r| r.get(0),
        )
        .optional()?;
    let header = platform.map_or(0, |p| super::candidates::header_len(&PlatformId(p)));
    let header = sql::to_i64(header);
    Ok(conn
        .prepare_cached(&format!(
            "SELECT m.source_id, m.file_index FROM (
               SELECT tf.source_id, tf.file_index, tf.size, tf.confidence
               FROM torrent_files tf WHERE tf.rom_id = ?1
               UNION ALL
               SELECT c.source_id, c.file_index, tf.size, c.confidence
               FROM torrent_candidates c
               JOIN torrent_files tf ON tf.source_id = c.source_id AND tf.file_index = c.file_index
               WHERE c.rom_id = ?1
             ) m
             JOIN sources s ON s.id = m.source_id AND s.state = 'bound'
             JOIN roms r ON r.id = ?1
             WHERE {bad}
             ORDER BY {tier}, (m.size = r.size OR m.size = r.size + ?2) DESC, {rank},
               (SELECT COUNT(*) FROM downloads a
                WHERE a.source_id = s.id AND a.state IN {selected}),
               s.id, m.file_index
             LIMIT 1",
            bad = super::candidates::not_bad("?1", "m.source_id", "m.file_index"),
            rank = super::candidates::rank("m.confidence"),
            tier = super::candidates::tier("m.confidence"),
            selected = DownloadState::SELECTED_SQL,
        ))?
        .query_row(params![rom, header], |r| {
            Ok(Candidate {
                source_id: r.get(0)?,
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
    let roms: Vec<RomId> = conn
        .prepare(&format!(
            "SELECT r.id FROM roms r WHERE r.title_id = ?1 AND r.retired = 0
               AND r.present = 0
               AND NOT EXISTS (SELECT 1 FROM files f WHERE f.rom_id = r.id AND f.state = 'verified')
               AND NOT EXISTS (SELECT 1 FROM downloads d WHERE d.rom_id = r.id AND d.state IN {})
             ORDER BY r.id",
            DownloadState::OPEN_SQL
        ))?
        .query_map([title], |r| r.get(0))?
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
    let waiting: Vec<(DownloadId, RomId)> = conn
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
            params![id, c.source_id, c.file_index, now],
        )?;
        moved.push(id);
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
        .query_map([], |r| r.get(0))?
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
             WHERE d.source_id = ?1 AND d.state IN (SELECT value FROM json_each(?2)) ORDER BY d.id"
        ))?
        .query_map(params![source, sql::json_list(states)?], from_row)?
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
             WHERE source_id = ?1 AND file_index IS NOT NULL AND state IN {}
             ORDER BY file_index",
            DownloadState::SELECTED_SQL
        ))?
        .query_map([source], |r| r.get(0))?
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
                    params![id, error],
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
    pub client_id: Option<ClientTorrentId>,
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
        .prepare(&format!(
            "SELECT d.id, d.state, d.progress, d.staged_path, d.source_id, d.file_index,
                    tf.path, s.infohash, s.display_name,
                    s.file_count = 1 AND tf.path = s.display_name, s.client_id, s.seed_policy, tf.size
             FROM downloads d
             JOIN sources s ON s.id = d.source_id
             JOIN torrent_files tf ON tf.source_id = d.source_id AND tf.file_index = d.file_index
             WHERE d.state IN {}
             ORDER BY d.source_id, d.id",
            DownloadState::STARTED_SQL
        ))?
        .query_map([], |r| {
            Ok(PollRow {
                id: r.get(0)?,
                state: r.get(1)?,
                progress: r.get(2)?,
                staged_path: r.get(3)?,
                source_id: r.get(4)?,
                file_index: r.get(5)?,
                path: r.get(6)?,
                infohash: r.get(7)?,
                torrent_name: r.get(8)?,
                single_file: r.get(9)?,
                client_id: sources::client_id(r, 10)?,
                seed_policy: r.get(11)?,
                size: sql::get_u64(r, 12)?,
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
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM downloads WHERE state IN (SELECT value FROM json_each(?1))",
        [sql::json_list(states)?],
        |r| sql::get_u64(r, 0),
    )?)
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
    pub rom_id: RomId,
    /// The rom the file hashed to, when it was another version.
    pub proven: Option<RomId>,
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
        moved: move_all(conn, &[id], DownloadState::Bad, Some(&e.reason), now)?,
        ..Settled::default()
    };
    if let Some(index) = e.file_index {
        if let (Some(proven), true) = (e.proven, e.whole) {
            candidates::prove(conn, e.source, index, proven)?;
        }
        candidates::drop_pair(conn, e.source, index, e.rom_id)?;
    }
    if let (Some(proven), true) = (e.proven, e.placed) {
        let redundant: Vec<DownloadId> = conn
            .prepare(&format!(
                "SELECT id FROM downloads WHERE rom_id = ?1
                   AND state IN {}
                   AND NOT (source_id IS ?2 AND file_index IS ?3)",
                DownloadState::CANCELLABLE_SQL
            ))?
            .query_map(params![proven, e.source, e.file_index], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for other in redundant {
            if let CancelOutcome::Cancelled(c) = cancel(conn, other, now)? {
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
    let Some(row) = get(conn, bad)? else {
        return Ok(None);
    };
    let open: bool = conn.query_row(
        &format!(
            "SELECT (SELECT wanted = 0 OR retired = 1 FROM titles WHERE id = ?1)
                 OR EXISTS (SELECT 1 FROM files WHERE rom_id = ?2 AND state = 'verified')
                 OR EXISTS (SELECT 1 FROM downloads WHERE rom_id = ?2 AND state IN {})",
            DownloadState::OPEN_SQL
        ),
        params![row.title_id, row.rom_id],
        |r| r.get(0),
    )?;
    if row.state != DownloadState::Bad || open {
        return Ok(None);
    }
    let file = best_file(conn, row.rom_id)?;
    let again = create(
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
        params![again, note],
    )?;
    let state = if file.is_some() {
        DownloadState::Queued
    } else {
        DownloadState::Wanted
    };
    Ok(Some((again, state)))
}

#[cfg(test)]
mod tests;
