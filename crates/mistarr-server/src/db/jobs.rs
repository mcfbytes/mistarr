//! The `jobs` table.

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::Serialize;
use serde_json::Value;

use super::ids::JobId;
use super::sql::{self, text_enum, Page, Paged};
use crate::error::Result;
use crate::jobs::{JobKind, Lane};

text_enum! {
    /// `jobs.state`.
    pub enum JobState {
        /// Waiting for its lane.
        Queued = "queued",
        /// Executing.
        Running = "running",
        /// Waiting at the scheduler gate.
        Paused = "paused",
        /// Finished successfully.
        Done = "done",
        /// Finished with an error, or interrupted by a shutdown.
        Failed = "failed",
    }
}

impl JobState {
    /// States of a job that has not finished.
    pub const ACTIVE: [Self; 3] = [Self::Queued, Self::Running, Self::Paused];
    /// [`JobState::ACTIVE`] as an SQL list.
    pub const ACTIVE_SQL: &'static str = "('queued', 'running', 'paused')";

    /// States of a finished job.
    pub const FINISHED: [Self; 2] = [Self::Done, Self::Failed];
    /// [`JobState::FINISHED`] as an SQL list.
    pub const FINISHED_SQL: &'static str = "('done', 'failed')";

    /// True for `done` and `failed`.
    ///
    /// ```
    /// assert!(mistarr_server::db::jobs::JobState::Failed.is_finished());
    /// ```
    #[must_use]
    pub fn is_finished(self) -> bool {
        Self::FINISHED.contains(&self)
    }
}

/// One `jobs` row with its JSON columns parsed.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct JobRow {
    /// Row id.
    pub id: JobId,
    /// What the job does.
    pub kind: JobKind,
    /// The scheduler lane.
    pub lane: Lane,
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

/// One row of [`COLUMNS`]; JSON that does not parse becomes [`crate::Error::Stored`].
fn from_row(r: &Row<'_>) -> rusqlite::Result<JobRow> {
    Ok(JobRow {
        id: r.get(0)?,
        kind: r.get(1)?,
        lane: r.get(7)?,
        payload: sql::get_json(r, 2, "jobs.payload")?,
        state: r.get(3)?,
        progress: sql::get_json(r, 4, "jobs.progress")?,
        created_at: r.get(5)?,
        updated_at: r.get(6)?,
    })
}

/// The dedupe key of a job payload: each top-level field as `name`, 0x1F, value, sorted
/// by name and joined with 0x1E; strings are bare and other values are JSON. The
/// `title_groups` migration derives the same key for rows written before the column existed.
///
/// ```
/// use serde_json::json;
/// let key = mistarr_server::db::jobs::subject_of(&json!({"platform_id": "nes", "a": 1}));
/// assert_eq!(key, "a\u{1f}1\u{1e}platform_id\u{1f}nes");
/// assert_eq!(mistarr_server::db::jobs::subject_of(&json!({})), "");
/// ```
#[must_use]
pub fn subject_of(payload: &Value) -> String {
    let Some(fields) = payload.as_object() else {
        return String::new();
    };
    let mut pairs: Vec<(&String, String)> = fields
        .iter()
        .map(|(k, v)| {
            let v = match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            (k, v)
        })
        .collect();
    pairs.sort();
    pairs
        .iter()
        .map(|(k, v)| format!("{k}\u{1f}{v}"))
        .collect::<Vec<_>>()
        .join("\u{1e}")
}

/// Inserts a queued job on `lane`.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::jobs;
/// use mistarr_server::jobs::{JobKind, Lane};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let id = jobs::insert(&conn, JobKind::Scan, &serde_json::json!({}), Lane::Heavy, 1).unwrap();
/// assert_eq!(jobs::get(&conn, id).unwrap().unwrap().lane, Lane::Heavy);
/// ```
pub fn insert(
    conn: &Connection,
    kind: JobKind,
    payload: &Value,
    lane: Lane,
    now: i64,
) -> Result<JobId> {
    conn.execute(
        "INSERT INTO jobs (kind, payload, subject, state, created_at, updated_at, lane)
         VALUES (?1, ?2, ?3, 'queued', ?4, ?4, ?5)",
        params![kind, payload.to_string(), subject_of(payload), now, lane],
    )?;
    Ok(JobId(conn.last_insert_rowid()))
}

/// Reads one job.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure, [`crate::Error::Stored`] on unparseable JSON.
///
/// ```
/// use mistarr_server::db::jobs;
/// use mistarr_server::db::ids::JobId;
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert!(jobs::get(&conn, JobId(9)).unwrap().is_none());
/// ```
pub fn get(conn: &Connection, id: JobId) -> Result<Option<JobRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM jobs WHERE id = ?1"),
            [id],
            from_row,
        )
        .optional()?)
}

/// Every row `sql` selects with `args`.
fn select(conn: &Connection, sql: &str, args: impl rusqlite::Params) -> Result<Vec<JobRow>> {
    Ok(conn
        .prepare(sql)?
        .query_map(args, from_row)?
        .collect::<rusqlite::Result<_>>()?)
}

/// Sets a job's state.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::jobs::{self, JobState};
/// use mistarr_server::jobs::{JobKind, Lane};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let id = jobs::insert(&conn, JobKind::Scan, &serde_json::json!({}), Lane::Heavy, 1).unwrap();
/// jobs::set_state(&conn, id, JobState::Running, 2).unwrap();
/// ```
pub fn set_state(conn: &Connection, id: JobId, state: JobState, now: i64) -> Result<()> {
    conn.execute(
        "UPDATE jobs SET state = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, state, now],
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
/// use mistarr_server::jobs::{JobKind, Lane};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let id = jobs::insert(&conn, JobKind::Scan, &serde_json::json!({}), Lane::Heavy, 1).unwrap();
/// jobs::set_progress(&conn, id, &serde_json::json!({"done": 1}), 2).unwrap();
/// ```
pub fn set_progress(conn: &Connection, id: JobId, progress: &Value, now: i64) -> Result<()> {
    conn.execute(
        "UPDATE jobs SET progress = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, progress.to_string(), now],
    )?;
    Ok(())
}

/// Queued, running and paused jobs, oldest first, with the total before paging, both
/// read in one snapshot.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure, [`crate::Error::Stored`] on unparseable JSON.
///
/// ```
/// use mistarr_server::db::sql::Page;
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let page = mistarr_server::db::jobs::list_active(&conn, Page { limit: 10, offset: 0 }).unwrap();
/// assert!(page.items.is_empty() && page.total == 0);
/// ```
pub fn list_active(conn: &Connection, page: Page) -> Result<Paged<JobRow>> {
    let active = JobState::ACTIVE_SQL;
    sql::snapshot(conn, |c| {
        let total = c.query_row(
            &format!("SELECT COUNT(*) FROM jobs WHERE state IN {active}"),
            [],
            |r| sql::get_u64(r, 0),
        )?;
        let items = select(
            c,
            &format!(
                "SELECT {COLUMNS} FROM jobs WHERE state IN {active} ORDER BY id LIMIT ?1 OFFSET ?2"
            ),
            params![page.limit, page.offset],
        )?;
        Ok(Paged { items, total })
    })
}

/// The oldest job with this kind and payload in one of `states`, if any; see
/// [`crate::jobs::Dedupe::states`].
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::jobs::{self, JobState};
/// use mistarr_server::jobs::{JobKind, Lane};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let p = serde_json::json!({});
/// let id = jobs::insert(&conn, JobKind::Scan, &p, Lane::Heavy, 1).unwrap();
/// assert_eq!(jobs::find_in(&conn, JobKind::Scan, &p, &[JobState::Queued]).unwrap(), Some(id));
/// ```
pub fn find_in(
    conn: &Connection,
    kind: JobKind,
    payload: &Value,
    states: &[JobState],
) -> Result<Option<JobId>> {
    Ok(conn
        .query_row(
            "SELECT id FROM jobs WHERE kind = ?1 AND subject = ?2
               AND state IN (SELECT value FROM json_each(?3)) ORDER BY id LIMIT 1",
            params![kind, subject_of(payload), sql::json_list(states)?],
            |r| r.get(0),
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
/// use mistarr_server::jobs::{JobKind, Lane};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// jobs::insert(&conn, JobKind::Scan, &serde_json::json!({}), Lane::Heavy, 1).unwrap();
/// assert_eq!(jobs::count_kind(&conn, JobKind::Scan).unwrap(), 1);
/// ```
pub fn count_kind(conn: &Connection, kind: JobKind) -> Result<u64> {
    Ok(
        conn.query_row("SELECT COUNT(*) FROM jobs WHERE kind = ?1", [kind], |r| {
            sql::get_u64(r, 0)
        })?,
    )
}

/// Queued, running and paused jobs, oldest first: at startup, the ones a
/// previous process left unfinished.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure, [`crate::Error::Stored`] on unparseable JSON.
///
/// ```
/// use mistarr_server::db::jobs;
/// use mistarr_server::jobs::{JobKind, Lane};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// jobs::insert(&conn, JobKind::Scan, &serde_json::json!({}), Lane::Heavy, 1).unwrap();
/// assert_eq!(jobs::open_rows(&conn).unwrap().len(), 1);
/// ```
pub fn open_rows(conn: &Connection) -> Result<Vec<JobRow>> {
    select(
        conn,
        &format!(
            "SELECT {COLUMNS} FROM jobs WHERE state IN {} ORDER BY id",
            JobState::ACTIVE_SQL
        ),
        [],
    )
}

/// Fails, with `error`, every queued, running or paused job whose row this binary
/// cannot read: a kind or lane it does not list, or JSON that does not parse. A lane
/// it does not list becomes `light` and a payload that does not parse becomes `{}`, so
/// a failed row of a listed kind reads back. Returns how many, so [`open_rows`] can
/// read the rest.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert_eq!(mistarr_server::db::jobs::fail_unreadable(&conn, "x", 1).unwrap(), 0);
/// ```
pub fn fail_unreadable(conn: &Connection, error: &str, now: i64) -> Result<usize> {
    let bad = conn
        .prepare(&format!(
            "SELECT {COLUMNS} FROM jobs WHERE state IN {} ORDER BY id",
            JobState::ACTIVE_SQL
        ))?
        .query_map([], |r| {
            let lane = r.get::<_, Lane>(7).is_err().then_some(Lane::Light);
            let payload = sql::get_json::<Value>(r, 2, "jobs.payload")
                .is_err()
                .then_some("{}");
            Ok((r.get::<_, JobId>(0)?, from_row(r).is_err(), lane, payload))
        })?
        .filter_map(|row| match row {
            Ok((id, true, lane, payload)) => Some(Ok((id, lane, payload))),
            Ok((_, false, ..)) => None,
            Err(e) => Some(Err(e)),
        })
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let progress = serde_json::json!({ "error": error }).to_string();
    for (id, lane, payload) in &bad {
        conn.execute(
            "UPDATE jobs SET state = ?2, progress = ?3, updated_at = ?4,
                 lane = COALESCE(?5, lane), payload = COALESCE(?6, payload)
             WHERE id = ?1",
            params![id, JobState::Failed, progress, now, lane, payload],
        )?;
    }
    Ok(bad.len())
}

/// The kinds `/system/jobs/recent` lists: work a user starts or waits on.
pub const RECENT_KINDS: [JobKind; 6] = [
    JobKind::Scan,
    JobKind::ArcadeCatalog,
    JobKind::DatImport,
    JobKind::Recompute,
    JobKind::Import,
    JobKind::UrlFetch,
];

/// Up to `limit` finished jobs of [`RECENT_KINDS`], most recently updated first.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure, [`crate::Error::Stored`] on unparseable JSON.
///
/// ```
/// use mistarr_server::db::jobs;
/// use mistarr_server::jobs::{JobKind, Lane};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let id = jobs::insert(&conn, JobKind::Scan, &serde_json::json!({}), Lane::Heavy, 1).unwrap();
/// assert!(jobs::recent_finished(&conn, 5).unwrap().is_empty());
/// jobs::set_state(&conn, id, jobs::JobState::Done, 2).unwrap();
/// assert_eq!(jobs::recent_finished(&conn, 5).unwrap()[0].id, id);
/// ```
pub fn recent_finished(conn: &Connection, limit: u32) -> Result<Vec<JobRow>> {
    select(
        conn,
        &format!(
            "SELECT {COLUMNS} FROM jobs
             WHERE state IN {} AND kind IN (SELECT value FROM json_each(?1))
             ORDER BY updated_at DESC, id DESC LIMIT ?2",
            JobState::FINISHED_SQL
        ),
        params![sql::json_list(&RECENT_KINDS)?, limit],
    )
}

/// Open jobs on `lane`, oldest first.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure, [`crate::Error::Stored`] on unparseable JSON.
///
/// ```
/// use mistarr_server::db::jobs;
/// use mistarr_server::jobs::{JobKind, Lane};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// jobs::insert(&conn, JobKind::Scan, &serde_json::json!({}), Lane::Heavy, 1).unwrap();
/// assert_eq!(jobs::open_in_lane(&conn, Lane::Heavy).unwrap().len(), 1);
/// assert!(jobs::open_in_lane(&conn, Lane::Light).unwrap().is_empty());
/// ```
pub fn open_in_lane(conn: &Connection, lane: Lane) -> Result<Vec<JobRow>> {
    select(
        conn,
        &format!(
            "SELECT {COLUMNS} FROM jobs WHERE lane = ?1 AND state IN {} ORDER BY id",
            JobState::ACTIVE_SQL
        ),
        [lane],
    )
}

/// Whether a job of a kind other than `except_kind` is queued on `lane`, so a long job
/// of `except_kind` can hand the lane over between items.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::jobs::{JobKind, Lane};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let other = mistarr_server::db::jobs::queued_other_in_lane(&conn, Lane::Heavy, JobKind::Scan);
/// assert!(!other.unwrap());
/// ```
pub fn queued_other_in_lane(conn: &Connection, lane: Lane, except_kind: JobKind) -> Result<bool> {
    Ok(conn
        .prepare_cached(
            "SELECT EXISTS(SELECT 1 FROM jobs WHERE state = 'queued' AND lane = ?1 AND kind <> ?2)",
        )?
        .query_row(params![lane, except_kind], |r| r.get(0))?)
}

/// Finishes a job as `failed` with `{ error }` as its progress.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::jobs::{self, JobState};
/// use mistarr_server::jobs::{JobKind, Lane};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let id = jobs::insert(&conn, JobKind::DetectClient, &serde_json::json!({}), Lane::Light, 1).unwrap();
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
/// use mistarr_server::jobs::{JobKind, Lane};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let id = jobs::insert(&conn, JobKind::Scan, &serde_json::json!({}), Lane::Light, 1).unwrap();
/// jobs::set_state(&conn, id, JobState::Paused, 2).unwrap();
/// jobs::requeue(&conn, id, Lane::Heavy, 3).unwrap();
/// let row = jobs::get(&conn, id).unwrap().unwrap();
/// assert_eq!((row.state, row.lane), (JobState::Queued, Lane::Heavy));
/// ```
pub fn requeue(conn: &Connection, id: JobId, lane: Lane, now: i64) -> Result<()> {
    conn.execute(
        "UPDATE jobs SET state = 'queued', lane = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, lane, now],
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
/// use mistarr_server::jobs::{JobKind, Lane};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let id = jobs::insert(&conn, JobKind::DetectClient, &serde_json::json!({}), Lane::Light, 1).unwrap();
/// jobs::delete(&conn, id).unwrap();
/// assert!(jobs::get(&conn, id).unwrap().is_none());
/// ```
pub fn delete(conn: &Connection, id: JobId) -> Result<()> {
    conn.execute("DELETE FROM jobs WHERE id = ?1", [id])?;
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
        &format!(
            "DELETE FROM jobs WHERE state IN {finished} AND updated_at < ?2 AND id NOT IN (
               SELECT id FROM jobs WHERE state IN {finished}
               ORDER BY updated_at DESC, id DESC LIMIT ?1)",
            finished = JobState::FINISHED_SQL
        ),
        params![keep, now - PRUNE_GRACE_SECS],
    )?)
}

/// Finished jobs updated this recently are never pruned.
pub const PRUNE_GRACE_SECS: i64 = 3600;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::fixtures::conn;
    use crate::jobs::Dedupe;
    use serde_json::json;

    #[test]
    fn queued_other_in_lane_ignores_its_own_kind_and_other_lanes() {
        let c = conn();
        let own = insert(&c, JobKind::ChdTracks, &json!({}), Lane::Heavy, 0).expect("insert");
        assert!(!queued_other_in_lane(&c, Lane::Heavy, JobKind::ChdTracks).expect("query"));
        insert(
            &c,
            JobKind::DatImport,
            &json!({"path": "a"}),
            Lane::Background,
            0,
        )
        .expect("insert");
        assert!(!queued_other_in_lane(&c, Lane::Heavy, JobKind::ChdTracks).expect("query"));
        let scan = insert(
            &c,
            JobKind::Scan,
            &json!({"platform_id": "psx"}),
            Lane::Heavy,
            0,
        )
        .expect("insert");
        assert!(queued_other_in_lane(&c, Lane::Heavy, JobKind::ChdTracks).expect("query"));
        set_state(&c, scan, JobState::Running, 1).expect("running");
        assert!(!queued_other_in_lane(&c, Lane::Heavy, JobKind::ChdTracks).expect("query"));
        set_state(&c, own, JobState::Running, 1).expect("running");
        assert_eq!(
            find_in(
                &c,
                JobKind::ChdTracks,
                &json!({}),
                Dedupe::QueuedOrPaused.states()
            )
            .expect("find"),
            None,
            "a running singleton is never joined, so a yield queues a fresh row"
        );
    }

    #[test]
    fn lifecycle_round_trip() {
        let c = conn();
        let id =
            insert(&c, JobKind::DetectClient, &json!({"a": 1}), Lane::Heavy, 10).expect("insert");
        set_state(&c, id, JobState::Running, 11).expect("state");
        set_progress(&c, id, &json!({"step": 2}), 12).expect("progress");
        let row = get(&c, id).expect("get").expect("row");
        assert_eq!(row.state, JobState::Running);
        assert_eq!(row.payload, json!({"a": 1}));
        assert_eq!(row.progress, Some(json!({"step": 2})));
        assert_eq!((row.created_at, row.updated_at), (10, 12));
        assert_eq!(count_kind(&c, JobKind::DetectClient).expect("count"), 1);
        assert_eq!(count_kind(&c, JobKind::Scan).expect("count"), 0);
    }

    #[test]
    fn active_listing_pages_and_skips_finished() {
        let c = conn();
        let ids: Vec<_> = (0..3)
            .map(|i| insert(&c, JobKind::Scan, &json!({ "i": i }), Lane::Heavy, 1).expect("insert"))
            .collect();
        set_state(&c, ids[0], JobState::Done, 2).expect("done");
        let Paged { items, total } = list_active(
            &c,
            Page {
                limit: 1,
                offset: 1,
            },
        )
        .expect("list");
        assert_eq!(total, 2);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, ids[2]);
    }

    #[test]
    fn queued_lookup_matches_kind_and_payload_only_before_start() {
        let c = conn();
        let a = json!({"p": "a"});
        let id = insert(&c, JobKind::Scan, &a, Lane::Heavy, 1).expect("insert");
        assert_eq!(
            find_in(&c, JobKind::Scan, &a, Dedupe::Queued.states()).expect("find"),
            Some(id)
        );
        assert_eq!(
            find_in(
                &c,
                JobKind::Scan,
                &json!({"p": "b"}),
                Dedupe::Queued.states()
            )
            .expect("find"),
            None
        );
        set_state(&c, id, JobState::Paused, 2).expect("paused");
        assert_eq!(
            find_in(&c, JobKind::Scan, &a, Dedupe::Queued.states()).expect("find"),
            None
        );
        assert_eq!(
            find_in(&c, JobKind::Scan, &a, Dedupe::QueuedOrPaused.states()).expect("find"),
            Some(id)
        );
        set_state(&c, id, JobState::Running, 2).expect("running");
        assert_eq!(
            find_in(&c, JobKind::Scan, &a, Dedupe::QueuedOrPaused.states()).expect("find"),
            None
        );
        assert_eq!(
            find_in(&c, JobKind::Scan, &a, Dedupe::Open.states()).expect("find"),
            Some(id)
        );
    }

    #[test]
    fn interrupted_jobs_fail_and_old_ones_prune() {
        let c = conn();
        let a = insert(&c, JobKind::Scan, &json!({}), Lane::Heavy, 1).expect("insert");
        let b = insert(&c, JobKind::Scan, &json!({}), Lane::Heavy, 1).expect("insert");
        set_state(&c, b, JobState::Done, 1).expect("done");
        let open = open_rows(&c).expect("open");
        assert_eq!(open.iter().map(|r| r.id).collect::<Vec<_>>(), [a]);
        assert_eq!(open_in_lane(&c, Lane::Heavy).expect("lane").len(), 1);
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
        let long = insert(&c, JobKind::Scan, &json!({}), Lane::Heavy, 0).expect("insert");
        for i in 0..5 {
            let id = insert(
                &c,
                JobKind::DetectClient,
                &json!({ "i": i }),
                Lane::Heavy,
                10,
            )
            .expect("insert");
            set_state(&c, id, JobState::Done, 10 + i).expect("done");
        }
        set_state(&c, long, JobState::Done, now).expect("done");
        assert_eq!(prune(&c, 2, now).expect("prune"), 4);
        assert!(
            get(&c, long).expect("get").is_some(),
            "long job pruned on finish"
        );
        let recent =
            insert(&c, JobKind::DetectClient, &json!({}), Lane::Heavy, now).expect("insert");
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

    #[test]
    fn state_sets_match_their_sql() {
        let text = |states: &[JobState]| {
            sql::text_list(&states.iter().map(|s| s.as_str()).collect::<Vec<_>>())
        };
        assert_eq!(JobState::ACTIVE_SQL, text(&JobState::ACTIVE));
        assert_eq!(JobState::FINISHED_SQL, text(&JobState::FINISHED));
        for s in JobState::ALL {
            assert_ne!(JobState::ACTIVE.contains(s), s.is_finished(), "{s}");
        }
    }

    #[test]
    fn unreadable_rows_are_refused() {
        let c = conn();
        let id = insert(&c, JobKind::Scan, &json!({}), Lane::Heavy, 1).expect("insert");
        c.execute("UPDATE jobs SET payload = '{' WHERE id = ?1", [id])
            .expect("corrupt");
        assert!(matches!(
            get(&c, id),
            Err(crate::Error::Stored { ref key, .. }) if key == "jobs.payload"
        ));
        c.execute(
            "UPDATE jobs SET payload = '{}', kind = 'nope' WHERE id = ?1",
            [id],
        )
        .expect("unknown kind");
        assert!(matches!(get(&c, id), Err(crate::Error::Db(_))));
    }

    #[test]
    fn unreadable_open_rows_fail_and_the_rest_stay_open() {
        let c = conn();
        let good = insert(&c, JobKind::Scan, &json!({}), Lane::Heavy, 1).expect("insert");
        let kind = insert(&c, JobKind::Scan, &json!({"a": 1}), Lane::Heavy, 1).expect("insert");
        let lane = insert(&c, JobKind::Scan, &json!({"a": 2}), Lane::Heavy, 1).expect("insert");
        let json = insert(&c, JobKind::Scan, &json!({"a": 3}), Lane::Heavy, 1).expect("insert");
        let unknown_done = insert(&c, JobKind::Scan, &json!({"a": 4}), Lane::Heavy, 1).expect("x");
        c.execute_batch(&format!(
            "UPDATE jobs SET kind = 'nope' WHERE id IN ({kind}, {unknown_done});
             UPDATE jobs SET lane = 'nope' WHERE id = {lane};
             UPDATE jobs SET progress = '{{' WHERE id = {json};
             UPDATE jobs SET state = 'done' WHERE id = {unknown_done};"
        ))
        .expect("corrupt");
        assert!(open_rows(&c).is_err());
        assert_eq!(fail_unreadable(&c, "unreadable", 2).expect("fail"), 3);
        let open = open_rows(&c).expect("open");
        assert_eq!(open.iter().map(|r| r.id).collect::<Vec<_>>(), [good]);
        let failed = get(&c, json).expect("get").expect("row");
        assert_eq!(failed.state, JobState::Failed);
        assert_eq!(failed.progress, Some(json!({ "error": "unreadable" })));
        assert_eq!(fail_unreadable(&c, "unreadable", 3).expect("again"), 0);
    }

    #[test]
    fn failed_unreadable_rows_of_a_listed_kind_read_back() {
        let c = conn();
        let done = insert(&c, JobKind::Scan, &json!({}), Lane::Heavy, 1).expect("insert");
        let lane = insert(&c, JobKind::Import, &json!({"a": 1}), Lane::Heavy, 1).expect("x");
        let payload = insert(&c, JobKind::DatImport, &json!({"a": 2}), Lane::Heavy, 1).expect("x");
        c.execute_batch(&format!(
            "UPDATE jobs SET state = 'done' WHERE id = {done};
             UPDATE jobs SET lane = 'nope' WHERE id = {lane};
             UPDATE jobs SET payload = '{{' WHERE id = {payload};"
        ))
        .expect("corrupt");
        assert!(get(&c, lane).is_err() && get(&c, payload).is_err());
        assert_eq!(fail_unreadable(&c, "unreadable", 2).expect("fail"), 2);
        let recent = recent_finished(&c, 10).expect("recent");
        let ids = recent.iter().map(|r| r.id).collect::<Vec<_>>();
        assert_eq!(ids, [payload, lane, done]);
        assert_eq!(
            (recent[0].lane, &recent[0].payload),
            (Lane::Heavy, &json!({}))
        );
        assert_eq!(
            (recent[1].lane, &recent[1].payload),
            (Lane::Light, &json!({"a": 1}))
        );
        assert_eq!(recent[0].progress, Some(json!({ "error": "unreadable" })));
    }
}
