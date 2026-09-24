//! The `jobs` table.

use std::fmt;

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;

/// A `jobs.id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct JobId(pub i64);

impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// `jobs.state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    /// Waiting for its lane.
    Queued,
    /// Executing.
    Running,
    /// Waiting at the scheduler gate.
    Paused,
    /// Finished successfully.
    Done,
    /// Finished with an error, or interrupted by a shutdown.
    Failed,
}

impl JobState {
    /// The column value.
    ///
    /// ```
    /// assert_eq!(mistarr_server::db::jobs::JobState::Paused.as_str(), "paused");
    /// ```
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }

    /// Parses a column value.
    ///
    /// ```
    /// use mistarr_server::db::jobs::JobState;
    /// assert_eq!(JobState::parse("done"), Some(JobState::Done));
    /// assert_eq!(JobState::parse("x"), None);
    /// ```
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        [
            Self::Queued,
            Self::Running,
            Self::Paused,
            Self::Done,
            Self::Failed,
        ]
        .into_iter()
        .find(|v| v.as_str() == s)
    }

    /// True for `done` and `failed`.
    ///
    /// ```
    /// assert!(mistarr_server::db::jobs::JobState::Failed.is_finished());
    /// ```
    #[must_use]
    pub fn is_finished(self) -> bool {
        matches!(self, Self::Done | Self::Failed)
    }
}

/// One `jobs` row with its JSON columns parsed.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct JobRow {
    /// Row id.
    pub id: JobId,
    /// Job kind, e.g. `detect_client`.
    pub kind: String,
    /// The scheduler lane: `heavy`, `background` or `light`.
    pub lane: String,
    /// What the job was asked to do.
    pub payload: Value,
    /// Lifecycle state.
    pub state: JobState,
    /// Job-specific progress, `null` until the job reports some.
    pub progress: Option<Value>,
    /// Unix seconds.
    pub created_at: i64,
    /// Unix seconds.
    pub updated_at: i64,
}

const COLUMNS: &str = "id, kind, payload, state, progress, created_at, updated_at, lane";

fn parse_json(text: &str, column: usize) -> rusqlite::Result<Value> {
    serde_json::from_str(text).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(column, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn from_row(r: &Row<'_>) -> rusqlite::Result<JobRow> {
    let state: String = r.get(3)?;
    let progress: Option<String> = r.get(4)?;
    Ok(JobRow {
        id: JobId(r.get(0)?),
        kind: r.get(1)?,
        payload: parse_json(&r.get::<_, String>(2)?, 2)?,
        state: JobState::parse(&state).unwrap_or(JobState::Failed),
        progress: progress.as_deref().map(|p| parse_json(p, 4)).transpose()?,
        created_at: r.get(5)?,
        updated_at: r.get(6)?,
        lane: r.get(7)?,
    })
}

/// Inserts a queued job on `lane`.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::jobs;
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let id = jobs::insert(&conn, "scan", &serde_json::json!({}), "heavy", 1).unwrap();
/// assert_eq!(jobs::get(&conn, id).unwrap().unwrap().lane, "heavy");
/// ```
pub fn insert(
    conn: &Connection,
    kind: &str,
    payload: &Value,
    lane: &str,
    now: i64,
) -> Result<JobId> {
    conn.execute(
        "INSERT INTO jobs (kind, payload, state, created_at, updated_at, lane)
         VALUES (?1, ?2, 'queued', ?3, ?3, ?4)",
        params![kind, payload.to_string(), now, lane],
    )?;
    Ok(JobId(conn.last_insert_rowid()))
}

/// Reads one job.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure or unparseable JSON.
///
/// ```
/// use mistarr_server::db::jobs::{self, JobId};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert!(jobs::get(&conn, JobId(9)).unwrap().is_none());
/// ```
pub fn get(conn: &Connection, id: JobId) -> Result<Option<JobRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM jobs WHERE id = ?1"),
            [id.0],
            from_row,
        )
        .optional()?)
}

/// Sets a job's state.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::jobs::{self, JobState};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let id = jobs::insert(&conn, "scan", &serde_json::json!({}), "heavy", 1).unwrap();
/// jobs::set_state(&conn, id, JobState::Running, 2).unwrap();
/// ```
pub fn set_state(conn: &Connection, id: JobId, state: JobState, now: i64) -> Result<()> {
    conn.execute(
        "UPDATE jobs SET state = ?2, updated_at = ?3 WHERE id = ?1",
        params![id.0, state.as_str(), now],
    )?;
    Ok(())
}

/// Replaces a job's progress.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::jobs;
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let id = jobs::insert(&conn, "scan", &serde_json::json!({}), "heavy", 1).unwrap();
/// jobs::set_progress(&conn, id, &serde_json::json!({"done": 1}), 2).unwrap();
/// ```
pub fn set_progress(conn: &Connection, id: JobId, progress: &Value, now: i64) -> Result<()> {
    conn.execute(
        "UPDATE jobs SET progress = ?2, updated_at = ?3 WHERE id = ?1",
        params![id.0, progress.to_string(), now],
    )?;
    Ok(())
}

/// Queued, running and paused jobs, oldest first, with the total before paging.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure or unparseable JSON.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let (items, total) = mistarr_server::db::jobs::list_active(&conn, 10, 0).unwrap();
/// assert!(items.is_empty() && total == 0);
/// ```
pub fn list_active(conn: &Connection, limit: u32, offset: u32) -> Result<(Vec<JobRow>, u64)> {
    const ACTIVE: &str = "state IN ('queued', 'running', 'paused')";
    let total: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM jobs WHERE {ACTIVE}"),
        [],
        |r| r.get(0),
    )?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM jobs WHERE {ACTIVE} ORDER BY id LIMIT ?1 OFFSET ?2"
    ))?;
    let rows = stmt
        .query_map(params![limit, offset], from_row)?
        .collect::<rusqlite::Result<_>>()?;
    Ok((rows, u64::try_from(total).unwrap_or(0)))
}

/// The id of a job with this kind and payload that has not started, if any;
/// with `include_paused`, a started job waiting at the gate matches too.
/// Running jobs never match, so work requested after a job began runs again.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::jobs;
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let p = serde_json::json!({});
/// let id = jobs::insert(&conn, "scan", &p, "heavy", 1).unwrap();
/// assert_eq!(jobs::find_queued(&conn, "scan", &p, false).unwrap(), Some(id));
/// ```
pub fn find_queued(
    conn: &Connection,
    kind: &str,
    payload: &Value,
    include_paused: bool,
) -> Result<Option<JobId>> {
    let states = if include_paused {
        "('queued', 'paused')"
    } else {
        "('queued')"
    };
    Ok(conn
        .query_row(
            &format!(
                "SELECT id FROM jobs WHERE kind = ?1 AND payload = ?2
                   AND state IN {states} ORDER BY id LIMIT 1"
            ),
            params![kind, payload.to_string()],
            |r| r.get(0).map(JobId),
        )
        .optional()?)
}

/// The oldest job of `kind` with `payload` that has not finished: queued,
/// running or paused.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::jobs::{self, JobState};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let p = serde_json::json!({});
/// let id = jobs::insert(&conn, "import", &p, "heavy", 1).unwrap();
/// jobs::set_state(&conn, id, JobState::Running, 2).unwrap();
/// assert_eq!(jobs::find_open(&conn, "import", &p).unwrap(), Some(id));
/// jobs::set_state(&conn, id, JobState::Done, 3).unwrap();
/// assert_eq!(jobs::find_open(&conn, "import", &p).unwrap(), None);
/// ```
pub fn find_open(conn: &Connection, kind: &str, payload: &Value) -> Result<Option<JobId>> {
    Ok(conn
        .query_row(
            "SELECT id FROM jobs WHERE kind = ?1 AND payload = ?2
               AND state IN ('queued', 'running', 'paused') ORDER BY id LIMIT 1",
            params![kind, payload.to_string()],
            |r| r.get(0).map(JobId),
        )
        .optional()?)
}

/// Number of jobs of `kind` in any state.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::jobs;
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// jobs::insert(&conn, "scan", &serde_json::json!({}), "heavy", 1).unwrap();
/// assert_eq!(jobs::count_kind(&conn, "scan").unwrap(), 1);
/// ```
pub fn count_kind(conn: &Connection, kind: &str) -> Result<u64> {
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM jobs WHERE kind = ?1", [kind], |r| {
        r.get(0)
    })?;
    Ok(u64::try_from(n).unwrap_or(0))
}

/// Queued, running and paused jobs, oldest first: at startup, the ones a
/// previous process left unfinished.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure or unparseable JSON.
///
/// ```
/// use mistarr_server::db::jobs;
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// jobs::insert(&conn, "scan", &serde_json::json!({}), "heavy", 1).unwrap();
/// assert_eq!(jobs::open_rows(&conn).unwrap().len(), 1);
/// ```
pub fn open_rows(conn: &Connection) -> Result<Vec<JobRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM jobs WHERE state IN ('queued', 'running', 'paused') ORDER BY id"
    ))?;
    let rows = stmt
        .query_map([], from_row)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// Open jobs on `lane`, oldest first.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure or unparseable JSON.
///
/// ```
/// use mistarr_server::db::jobs;
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// jobs::insert(&conn, "scan", &serde_json::json!({}), "heavy", 1).unwrap();
/// assert_eq!(jobs::open_in_lane(&conn, "heavy").unwrap().len(), 1);
/// assert!(jobs::open_in_lane(&conn, "light").unwrap().is_empty());
/// ```
pub fn open_in_lane(conn: &Connection, lane: &str) -> Result<Vec<JobRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM jobs
         WHERE lane = ?1 AND state IN ('queued', 'running', 'paused') ORDER BY id"
    ))?;
    let rows = stmt
        .query_map([lane], from_row)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// Finishes a job as `failed` with `{ error }` as its progress.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::jobs::{self, JobState};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let id = jobs::insert(&conn, "poll", &serde_json::json!({}), "light", 1).unwrap();
/// jobs::fail(&conn, id, "stopped", 2).unwrap();
/// assert_eq!(jobs::get(&conn, id).unwrap().unwrap().state, JobState::Failed);
/// ```
pub fn fail(conn: &Connection, id: JobId, error: &str, now: i64) -> Result<()> {
    set_progress(conn, id, &serde_json::json!({ "error": error }), now)?;
    set_state(conn, id, JobState::Failed, now)
}

/// Puts a job back to `queued` on `lane`, keeping its id and progress.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::jobs::{self, JobState};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let id = jobs::insert(&conn, "scan", &serde_json::json!({}), "light", 1).unwrap();
/// jobs::set_state(&conn, id, JobState::Paused, 2).unwrap();
/// jobs::requeue(&conn, id, "heavy", 3).unwrap();
/// let row = jobs::get(&conn, id).unwrap().unwrap();
/// assert_eq!((row.state, row.lane.as_str()), (JobState::Queued, "heavy"));
/// ```
pub fn requeue(conn: &Connection, id: JobId, lane: &str, now: i64) -> Result<()> {
    conn.execute(
        "UPDATE jobs SET state = 'queued', lane = ?2, updated_at = ?3 WHERE id = ?1",
        params![id.0, lane, now],
    )?;
    Ok(())
}

/// Deletes one job row.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::jobs;
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let id = jobs::insert(&conn, "poll", &serde_json::json!({}), "light", 1).unwrap();
/// jobs::delete(&conn, id).unwrap();
/// assert!(jobs::get(&conn, id).unwrap().is_none());
/// ```
pub fn delete(conn: &Connection, id: JobId) -> Result<()> {
    conn.execute("DELETE FROM jobs WHERE id = ?1", [id.0])?;
    Ok(())
}

/// Deletes finished jobs beyond the `keep` most recently updated, sparing any
/// updated within [`PRUNE_GRACE_SECS`] of `now`. Returns how many went.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert_eq!(mistarr_server::db::jobs::prune(&conn, 10, 0).unwrap(), 0);
/// ```
pub fn prune(conn: &Connection, keep: u32, now: i64) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM jobs WHERE state IN ('done', 'failed') AND updated_at < ?2 AND id NOT IN (
           SELECT id FROM jobs WHERE state IN ('done', 'failed')
           ORDER BY updated_at DESC, id DESC LIMIT ?1)",
        params![keep, now - PRUNE_GRACE_SECS],
    )?)
}

/// Finished jobs updated this recently are never pruned.
pub const PRUNE_GRACE_SECS: i64 = 3600;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn conn() -> Connection {
        let mut c = Connection::open_in_memory().expect("open");
        crate::db::migrate::apply(&mut c).expect("migrate");
        c
    }

    #[test]
    fn lifecycle_round_trip() {
        let c = conn();
        let id = insert(&c, "detect_client", &json!({"a": 1}), "heavy", 10).expect("insert");
        set_state(&c, id, JobState::Running, 11).expect("state");
        set_progress(&c, id, &json!({"step": 2}), 12).expect("progress");
        let row = get(&c, id).expect("get").expect("row");
        assert_eq!(row.state, JobState::Running);
        assert_eq!(row.payload, json!({"a": 1}));
        assert_eq!(row.progress, Some(json!({"step": 2})));
        assert_eq!((row.created_at, row.updated_at), (10, 12));
        assert_eq!(count_kind(&c, "detect_client").expect("count"), 1);
        assert_eq!(count_kind(&c, "scan").expect("count"), 0);
    }

    #[test]
    fn active_listing_pages_and_skips_finished() {
        let c = conn();
        let ids: Vec<_> = (0..3)
            .map(|i| insert(&c, "scan", &json!({ "i": i }), "heavy", 1).expect("insert"))
            .collect();
        set_state(&c, ids[0], JobState::Done, 2).expect("done");
        let (items, total) = list_active(&c, 1, 1).expect("list");
        assert_eq!(total, 2);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, ids[2]);
    }

    #[test]
    fn queued_lookup_matches_kind_and_payload_only_before_start() {
        let c = conn();
        let a = json!({"p": "a"});
        let id = insert(&c, "scan", &a, "heavy", 1).expect("insert");
        assert_eq!(find_queued(&c, "scan", &a, false).expect("find"), Some(id));
        assert_eq!(
            find_queued(&c, "scan", &json!({"p": "b"}), false).expect("find"),
            None
        );
        set_state(&c, id, JobState::Paused, 2).expect("paused");
        assert_eq!(find_queued(&c, "scan", &a, false).expect("find"), None);
        assert_eq!(find_queued(&c, "scan", &a, true).expect("find"), Some(id));
        set_state(&c, id, JobState::Running, 2).expect("running");
        assert_eq!(find_queued(&c, "scan", &a, true).expect("find"), None);
    }

    #[test]
    fn interrupted_jobs_fail_and_old_ones_prune() {
        let c = conn();
        let a = insert(&c, "scan", &json!({}), "heavy", 1).expect("insert");
        let b = insert(&c, "scan", &json!({}), "heavy", 1).expect("insert");
        set_state(&c, b, JobState::Done, 1).expect("done");
        let open = open_rows(&c).expect("open");
        assert_eq!(open.iter().map(|r| r.id).collect::<Vec<_>>(), [a]);
        assert_eq!(open_in_lane(&c, "heavy").expect("lane").len(), 1);
        fail(&c, a, "interrupted", 5).expect("fail");
        assert!(open_rows(&c).expect("open").is_empty());
        assert_eq!(
            get(&c, a).expect("get").expect("row").state,
            JobState::Failed
        );
        assert_eq!(prune(&c, 1, 5 + PRUNE_GRACE_SECS + 1).expect("prune"), 1);
        assert!(get(&c, a).expect("get").is_some());
        assert!(get(&c, b).expect("get").is_none());
    }

    #[test]
    fn prune_keeps_recently_finished_long_jobs() {
        let c = conn();
        let now = 100_000;
        let long = insert(&c, "scan", &json!({}), "heavy", 0).expect("insert");
        for i in 0..5 {
            let id = insert(&c, "poll", &json!({ "i": i }), "heavy", 10).expect("insert");
            set_state(&c, id, JobState::Done, 10 + i).expect("done");
        }
        set_state(&c, long, JobState::Done, now).expect("done");
        assert_eq!(prune(&c, 2, now).expect("prune"), 4);
        assert!(
            get(&c, long).expect("get").is_some(),
            "long job pruned on finish"
        );
        let recent = insert(&c, "poll", &json!({}), "heavy", now).expect("insert");
        set_state(&c, recent, JobState::Done, now - 10).expect("done");
        prune(&c, 1, now).expect("prune");
        assert!(
            get(&c, recent).expect("get").is_some(),
            "within the grace period"
        );
    }

    #[test]
    fn states_round_trip() {
        for s in ["queued", "running", "paused", "done", "failed"] {
            assert_eq!(JobState::parse(s).map(JobState::as_str), Some(s));
        }
        assert!(!JobState::Queued.is_finished());
        assert_eq!(JobId(4).to_string(), "4");
    }
}
