//! Background jobs: the [`Job`] trait, three serial lanes and the gate; see `docs/ARCHITECTURE.md`.

pub mod arcade;
pub mod chd;
pub mod corename;
pub mod dat_import;
pub mod detect_client;
pub mod gate;
pub mod import;
pub mod io_priority;
pub mod poll;
pub mod progress;
pub mod remap;
pub mod scan;
pub mod source_import;
pub mod transfer;
pub mod wizard;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::app::AppState;
use crate::db::jobs::{self as rows, JobId, JobState};
use crate::error::{Error, Result};
use crate::events::EventKind;

/// Finished rows kept in `jobs` for the activity screen.
const KEEP_FINISHED: u32 = 200;

/// Kinds whose whole work a paused run of the same payload still covers, so a
/// second request never queues behind it.
pub const SINGLETON_KINDS: [&str; 4] = [
    arcade::KIND,
    scan::KIND,
    dat_import::RECOMPUTE_KIND,
    chd::KIND,
];

/// Error recorded on a job a previous process left unfinished and that is not re-run.
pub const INTERRUPTED: &str = "interrupted by a restart";

/// Which serial queue a job runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    /// Hashing, scanning, placing files: one at a time, held by the gate.
    Heavy,
    /// DAT and source parsing: one at a time, held only by a manual pause, yielding while a core runs.
    Background,
    /// Client polling, detection and transfers: one at a time, never held.
    Light,
}

impl Lane {
    /// The `jobs.lane` value.
    ///
    /// ```
    /// assert_eq!(mistarr_server::jobs::Lane::Background.as_str(), "background");
    /// ```
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Heavy => "heavy",
            Self::Background => "background",
            Self::Light => "light",
        }
    }
}

/// A unit of background work. Later packages add implementations and enqueue
/// them through [`Scheduler::enqueue`].
#[async_trait]
pub trait Job: Send + Sync {
    /// The `jobs.kind` value.
    fn kind(&self) -> &'static str;

    /// The `jobs.payload` value; equal kind and payload deduplicate while queued.
    fn payload(&self) -> Value {
        json!({})
    }

    /// The lane to run on.
    fn lane(&self) -> Lane {
        Lane::Light
    }

    /// True when a request joins a job of the same kind and payload that is
    /// waiting at the gate, not only one still queued; see [`SINGLETON_KINDS`].
    fn singleton(&self) -> bool {
        SINGLETON_KINDS.contains(&self.kind())
    }

    /// Does the work. Heavy jobs call [`JobContext::checkpoint`] at file boundaries.
    async fn run(&self, ctx: &JobContext) -> Result<()>;
}

/// What a running job gets from the scheduler.
pub struct JobContext {
    /// The job's row.
    pub id: JobId,
    /// The job's kind.
    pub kind: &'static str,
    /// The server.
    pub app: Arc<AppState>,
    lane: Lane,
    detail: Option<String>,
}

/// The `job.progress` event body.
#[derive(Debug, Clone, Serialize)]
struct ProgressEvent<'a> {
    id: JobId,
    kind: &'a str,
    state: JobState,
    detail: Option<&'a str>,
    progress: &'a Value,
}

impl JobContext {
    /// Stores `progress` on the job row, replacing any live progress, and publishes `job.progress`.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Db`] when the row cannot be updated.
    pub async fn progress(&self, progress: Value) -> Result<()> {
        let id = self.id;
        let stored = progress.clone();
        self.app
            .db
            .write(move |c| rows::set_progress(c, id, &stored, crate::unix_now()))
            .await?;
        self.app.live.clear(id);
        self.publish(JobState::Running, &progress);
        Ok(())
    }

    /// Fails with [`Error::Cancelled`] once the server is shutting down. While
    /// the job's lane is held, waits, marking the row `paused`; see [`gate::GateState::hold`].
    ///
    /// # Errors
    ///
    /// [`Error::Cancelled`] on shutdown, [`Error::Db`] when the row state cannot be updated.
    pub async fn checkpoint(&self) -> Result<()> {
        let mut stop = self.app.shutdown_signal();
        if *stop.borrow() {
            return Err(Error::Cancelled);
        }
        if self.app.gate.state().hold(self.lane).is_none() {
            return Ok(());
        }
        set_state(&self.app, self.id, JobState::Paused).await?;
        tokio::select! {
            () = self.app.gate.wait_free(self.lane) => {}
            _ = stop.wait_for(|s| *s) => return Err(Error::Cancelled),
        }
        set_state(&self.app, self.id, JobState::Running).await
    }

    /// A throttled reporter of this job's live progress, for work that cannot
    /// write the database meanwhile; see [`progress::Reporter`].
    #[must_use]
    pub fn reporter(&self) -> progress::Reporter {
        progress::Reporter::new(
            Arc::clone(&self.app),
            self.id,
            self.kind,
            self.detail.clone(),
        )
    }

    fn publish(&self, state: JobState, progress: &Value) {
        let body = ProgressEvent {
            id: self.id,
            kind: self.kind,
            state,
            detail: self.detail.as_deref(),
            progress,
        };
        self.app.events.publish(EventKind::JobProgress, &body);
    }
}

async fn set_state(app: &AppState, id: JobId, state: JobState) -> Result<()> {
    app.db
        .write(move |c| rows::set_state(c, id, state, crate::unix_now()))
        .await
}

struct Queued {
    id: JobId,
    job: Arc<dyn Job>,
}

const LANES: [Lane; 3] = [Lane::Heavy, Lane::Background, Lane::Light];

type Receivers = Vec<(Lane, mpsc::UnboundedReceiver<Queued>)>;

/// Queues jobs onto the three lanes and records them in `jobs`.
pub struct Scheduler {
    senders: Vec<(Lane, mpsc::UnboundedSender<Queued>)>,
    receivers: Mutex<Option<Receivers>>,
    lanes: Mutex<Vec<JoinHandle<()>>>,
    alive: Arc<AtomicUsize>,
}

/// Decrements the live-lane count when a lane task ends or is aborted.
struct LaneGuard(Arc<AtomicUsize>);

impl Drop for LaneGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl Scheduler {
    /// A scheduler whose lanes start with [`Scheduler::start`].
    #[must_use]
    pub fn new() -> Self {
        let (senders, receivers) = LANES
            .iter()
            .map(|&lane| {
                let (tx, rx) = mpsc::unbounded_channel();
                ((lane, tx), (lane, rx))
            })
            .unzip();
        Self {
            senders,
            receivers: Mutex::new(Some(receivers)),
            lanes: Mutex::new(Vec::new()),
            alive: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Lane tasks still running.
    #[must_use]
    pub fn lanes_alive(&self) -> usize {
        self.alive.load(Ordering::SeqCst)
    }

    /// Waits up to `limit` for the lanes to finish after shutdown was
    /// signalled, then aborts any still running.
    pub async fn stop(&self, limit: Duration) {
        let handles: Vec<_> =
            std::mem::take(&mut *self.lanes.lock().unwrap_or_else(PoisonError::into_inner));
        let deadline = tokio::time::Instant::now() + limit;
        for mut h in handles {
            if tokio::time::timeout_at(deadline, &mut h).await.is_err() {
                tracing::warn!("job lane did not stop in time");
                h.abort();
                let _ = h.await;
            }
        }
    }

    /// Spawns the lane tasks. Later calls do nothing.
    pub fn start(app: &Arc<AppState>) {
        let taken = app
            .scheduler
            .receivers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let Some(receivers) = taken else {
            return;
        };
        let sched = &app.scheduler;
        let mut lanes = sched.lanes.lock().unwrap_or_else(PoisonError::into_inner);
        for (kind, rx) in receivers {
            sched.alive.fetch_add(1, Ordering::SeqCst);
            let guard = LaneGuard(Arc::clone(&sched.alive));
            lanes.push(tokio::spawn(lane(Arc::clone(app), kind, rx, guard)));
        }
    }

    /// Records a queued job and hands it to its lane. A job with the same kind
    /// and payload that has not started yet is returned instead of a second
    /// one; for a [`Job::singleton`], so is one waiting at the gate.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Db`] when the row cannot be written, [`crate::Error::Job`]
    /// when the lane has stopped.
    pub async fn enqueue(app: &Arc<AppState>, job: Arc<dyn Job>) -> Result<JobId> {
        let (kind, payload, lane) = (job.kind(), job.payload(), job.lane());
        let singleton = job.singleton();
        let (id, fresh) = app
            .db
            .write(move |c| {
                if let Some(id) = rows::find_queued(c, kind, &payload, singleton)? {
                    return Ok((id, false));
                }
                let id = rows::insert(c, kind, &payload, lane.as_str(), crate::unix_now())?;
                Ok((id, true))
            })
            .await?;
        if fresh {
            let detail = crate::status::job_detail(&job.payload());
            let queued = ProgressEvent {
                id,
                kind,
                state: JobState::Queued,
                detail: detail.as_deref(),
                progress: &Value::Null,
            };
            app.events.publish(EventKind::JobProgress, &queued);
            app.scheduler.dispatch(id, job)?;
            if app.gate.state().hold(lane).is_some() {
                publish_status(app).await;
            }
        }
        Ok(id)
    }

    /// [`Scheduler::enqueue`] on a task of its own, waiting at most `wait` for it:
    /// `None` when the writer is still busy, as while a DAT applies; the job is then
    /// recorded and dispatched once the writer is free. `keep` is dropped after that.
    ///
    /// # Errors
    ///
    /// As [`Scheduler::enqueue`], when it fails within `wait`.
    pub async fn enqueue_within<K: Send + 'static>(
        app: &Arc<AppState>,
        job: Arc<dyn Job>,
        wait: Duration,
        keep: K,
    ) -> Result<Option<JobId>> {
        let task_app = Arc::clone(app);
        let mut task = tokio::spawn(async move {
            let queued = Self::enqueue(&task_app, job).await;
            if let Err(e) = &queued {
                tracing::warn!(error = %e, "cannot queue a job");
            }
            drop(keep);
            queued
        });
        match tokio::time::timeout(wait, &mut task).await {
            Ok(joined) => joined.map_err(|e| Error::Task(e.to_string()))?.map(Some),
            Err(_) => Ok(None),
        }
    }

    /// Hands an already recorded job to its lane.
    fn dispatch(&self, id: JobId, job: Arc<dyn Job>) -> Result<()> {
        let lane = job.lane();
        let tx = self
            .senders
            .iter()
            .find_map(|(l, tx)| (*l == lane).then_some(tx))
            .ok_or_else(|| crate::Error::Job("no such lane".to_owned()))?;
        tx.send(Queued { id, job })
            .map_err(|_| crate::Error::Job("scheduler has stopped".to_owned()))
    }

    /// Records and runs a job on the calling task, bypassing the lanes and
    /// the gate, and returns its id once finished.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Db`] when the row cannot be written. The job's own
    /// failure is recorded on the row, not returned.
    pub async fn run_inline(app: &Arc<AppState>, job: Arc<dyn Job>) -> Result<JobId> {
        let (kind, payload) = (job.kind(), job.payload());
        let id = app
            .db
            .write(move |c| rows::insert(c, kind, &payload, "light", crate::unix_now()))
            .await?;
        execute(app, id, job.as_ref(), Lane::Light).await?;
        Ok(id)
    }
}

/// Runs queued jobs one at a time until shutdown. A job waits while its lane
/// is held before it starts and stays `queued` meanwhile.
async fn lane(
    app: Arc<AppState>,
    lane: Lane,
    mut rx: mpsc::UnboundedReceiver<Queued>,
    _guard: LaneGuard,
) {
    let mut stop = app.shutdown_signal();
    loop {
        let next = tokio::select! {
            q = rx.recv() => q,
            _ = stop.wait_for(|s| *s) => None,
        };
        let Some(Queued { id, job }) = next else {
            break;
        };
        tokio::select! {
            () = app.gate.wait_free(lane) => {}
            _ = stop.wait_for(|s| *s) => break,
        }
        if let Err(e) = execute(&app, id, job.as_ref(), lane).await {
            tracing::warn!(job = %id, error = %e, "cannot record job state");
        }
        if lane == Lane::Heavy {
            after_heavy(&app).await;
        }
    }
}

/// Once the heavy queue drains, ends a "Run now" override so later work
/// waits for the core again; otherwise refreshes the waiting list in `status`.
async fn after_heavy(app: &Arc<AppState>) {
    let open = app
        .db
        .read(|c| rows::open_in_lane(c, Lane::Heavy.as_str()))
        .await;
    match open {
        Ok(rows) if rows.is_empty() => {
            if app.gate.end_run_now() {
                tracing::info!("heavy queue drained; run-now override ended");
            }
        }
        Ok(_) if app.gate.state().core_running() => publish_status(app).await,
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "cannot read the heavy queue"),
    }
}

async fn publish_status(app: &AppState) {
    let status = crate::status::snapshot(app).await;
    app.events.publish(EventKind::Status, &status);
}

/// What [`reconcile`] did with the jobs a previous process left open.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Reconciled {
    /// Re-queued under their own ids.
    pub requeued: usize,
    /// Failed as [`INTERRUPTED`], for kinds that are not re-run.
    pub failed: usize,
    /// Deleted as repeats of an earlier row with the same kind and payload.
    pub dropped: usize,
}

/// Takes over the queued, running and paused rows a previous process left:
/// the first row of each kind and payload goes back on its lane under its
/// own id when [`revive`] knows the kind, else fails as [`INTERRUPTED`];
/// later repeats are deleted. Run it before anything else is enqueued.
///
/// # Errors
///
/// [`crate::Error::Db`] when the rows cannot be read or written,
/// [`crate::Error::Job`] when a lane has stopped.
pub async fn reconcile(app: &Arc<AppState>) -> Result<Reconciled> {
    let open = app.db.read(rows::open_rows).await?;
    let mut seen = std::collections::HashSet::new();
    let mut done = Reconciled::default();
    for row in open {
        let id = row.id;
        if !seen.insert((row.kind.clone(), row.payload.to_string())) {
            app.db.write(move |c| rows::delete(c, id)).await?;
            done.dropped += 1;
            continue;
        }
        let now = crate::unix_now();
        if let Some(job) = revive(&row.kind, &row.payload) {
            let lane = job.lane().as_str();
            app.db
                .write(move |c| rows::requeue(c, id, lane, now))
                .await?;
            app.scheduler.dispatch(id, job)?;
            done.requeued += 1;
        } else {
            app.db
                .write(move |c| rows::fail(c, id, INTERRUPTED, now))
                .await?;
            done.failed += 1;
        }
    }
    Ok(done)
}

/// Rebuilds a job from its stored kind and payload, for the kinds that are
/// safe to run again after a restart; `None` for the rest.
///
/// ```
/// use mistarr_server::jobs::revive;
/// assert!(revive("arcade_catalog", &serde_json::json!({})).is_some());
/// assert!(revive("chd_tracks", &serde_json::json!({})).is_some());
/// assert!(revive("dat_import", &serde_json::json!({"path": "/d/a.dat"})).is_some());
/// assert!(revive("detect_client", &serde_json::json!({})).is_none());
/// ```
#[must_use]
pub fn revive(kind: &str, payload: &Value) -> Option<Arc<dyn Job>> {
    let text = |key: &str| payload.get(key).and_then(Value::as_str);
    let job: Arc<dyn Job> = match kind {
        arcade::KIND => Arc::new(arcade::ArcadeCatalog),
        chd::KIND => Arc::new(chd::ChdTracks),
        scan::KIND => Arc::new(scan::ScanJob {
            platform_id: text("platform_id").map(|p| mistarr_core::PlatformId(p.to_owned())),
        }),
        // A bind request names a version; the user repeats it from the UI instead.
        dat_import::KIND if payload.get("dat_version_id").is_none() => Arc::new(
            dat_import::DatImport::new(std::path::Path::new(text("path")?)),
        ),
        dat_import::RECOMPUTE_KIND => Arc::new(dat_import::Recompute::new(text("platform_id")?)),
        source_import::IMPORT_KIND => Arc::new(source_import::SourceImport {
            path: text("path")?.into(),
        }),
        remap::KIND => Arc::new(remap::RemapSources::from_payload(payload)),
        import::KIND => Arc::new(import::ImportJob {
            download_id: crate::db::downloads::DownloadId(payload.get("download_id")?.as_i64()?),
        }),
        _ => return None,
    };
    Some(job)
}

/// Runs one job and records its outcome; only bookkeeping failures are returned.
async fn execute(app: &Arc<AppState>, id: JobId, job: &dyn Job, lane: Lane) -> Result<()> {
    let ctx = JobContext {
        id,
        kind: job.kind(),
        app: Arc::clone(app),
        lane,
        detail: crate::status::job_detail(&job.payload()),
    };
    set_state(app, id, JobState::Running).await?;
    tracing::debug!(job = %id, kind = ctx.kind, "job started");
    let ran = job.run(&ctx).await;
    // Cleared before any exit below, including shutdown and a failed final write.
    app.live.clear(id);
    let (state, progress) = match ran {
        Ok(()) => (JobState::Done, None),
        // Left queued so the next start runs it again; see `reconcile`.
        Err(Error::Cancelled) if *app.shutdown_signal().borrow() => {
            tracing::debug!(job = %id, kind = ctx.kind, "job stopped for shutdown");
            return set_state(app, id, JobState::Queued).await;
        }
        Err(e) => {
            tracing::warn!(job = %id, kind = ctx.kind, error = %e, "job failed");
            (JobState::Failed, Some(json!({ "error": e.to_string() })))
        }
    };
    let stored = progress.clone();
    let now = crate::unix_now();
    let last = app
        .db
        .write(move |c| {
            if let Some(p) = &stored {
                rows::set_progress(c, id, p, now)?;
            }
            rows::set_state(c, id, state, now)?;
            rows::prune(c, KEEP_FINISHED, now)?;
            rows::get(c, id)
        })
        .await?;
    let progress = progress
        .or_else(|| last.and_then(|r| r.progress))
        .unwrap_or(Value::Null);
    ctx.publish(state, &progress);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::state;
    use crate::jobs::gate::Override;
    use std::time::Duration;
    use tokio::sync::Notify;

    struct Probe {
        lane: Lane,
        fail: bool,
        ran: Arc<Notify>,
        tag: u32,
    }

    #[async_trait]
    impl Job for Probe {
        fn kind(&self) -> &'static str {
            "probe"
        }
        fn payload(&self) -> Value {
            json!({ "tag": self.tag })
        }
        fn lane(&self) -> Lane {
            self.lane
        }
        async fn run(&self, ctx: &JobContext) -> Result<()> {
            ctx.checkpoint().await?;
            ctx.progress(json!({ "step": 1 })).await?;
            self.ran.notify_one();
            if self.fail {
                return Err(crate::Error::Job("boom".into()));
            }
            Ok(())
        }
    }

    fn probe(lane: Lane, fail: bool, tag: u32) -> (Arc<Probe>, Arc<Notify>) {
        let ran = Arc::new(Notify::new());
        let job = Arc::new(Probe {
            lane,
            fail,
            ran: Arc::clone(&ran),
            tag,
        });
        (job, ran)
    }

    async fn wait_state(app: &AppState, id: JobId, want: JobState) {
        for _ in 0..200 {
            let row = app.db.read(move |c| rows::get(c, id)).await.expect("get");
            if row.is_some_and(|r| r.state == want) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("job {id} never reached {want:?}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn enqueue_within_answers_while_the_writer_is_busy() {
        let (_dir, app) = state();
        Scheduler::start(&app);
        let wait = Duration::from_millis(100);
        let (quick, _) = probe(Lane::Light, false, 10);
        let id = Scheduler::enqueue_within(&app, quick, wait, ())
            .await
            .expect("queued");
        assert!(id.is_some(), "a free writer records the job at once");
        let (held, is_held) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let db = app.db.clone();
        let holder = std::thread::spawn(move || {
            db.write_blocking(|c| {
                let tx = c.transaction()?;
                let _ = held.send(());
                let _ = released.recv();
                crate::db::commit(tx)
            })
        });
        is_held.recv().expect("held");
        let (job, ran) = probe(Lane::Light, false, 11);
        let (kept, dropped) = tokio::sync::oneshot::channel::<()>();
        let id = Scheduler::enqueue_within(&app, job, wait, kept)
            .await
            .expect("answered");
        assert_eq!(id, None);
        release.send(()).expect("release");
        holder.join().expect("join").expect("write");
        tokio::time::timeout(Duration::from_secs(5), ran.notified())
            .await
            .expect("the job ran once the writer was free");
        assert!(dropped.await.is_err(), "`keep` is dropped once recorded");
    }

    struct Reports;

    #[async_trait]
    impl Job for Reports {
        fn kind(&self) -> &'static str {
            "reports"
        }
        fn payload(&self) -> Value {
            json!({ "path": "/d/r.dat" })
        }
        async fn run(&self, ctx: &JobContext) -> Result<()> {
            assert!(ctx
                .reporter()
                .report("reading", || json!({ "phase": "reading" })));
            assert!(ctx.app.live.get(ctx.id).is_some(), "held while running");
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_reporter_sends_live_progress_that_ends_with_the_job() {
        let (_dir, app) = state();
        let mut events = app.events.subscribe(None).live;
        let id = Scheduler::run_inline(&app, Arc::new(Reports))
            .await
            .expect("run");
        let live = loop {
            let e = events.recv().await.expect("event");
            if e.seq == 0 {
                break e;
            }
        };
        let body: Value = serde_json::from_str(&live.data).expect("json");
        assert_eq!(body["detail"], "r.dat");
        assert_eq!(body["progress"]["phase"], "reading");
        assert_eq!(app.live.get(id), None, "cleared once finished");
    }

    #[tokio::test]
    async fn inline_run_records_done_and_failed() {
        let (_dir, app) = state();
        let (ok, _) = probe(Lane::Light, false, 1);
        let id = Scheduler::run_inline(&app, ok).await.expect("run");
        wait_state(&app, id, JobState::Done).await;
        let (bad, _) = probe(Lane::Light, true, 2);
        let id = Scheduler::run_inline(&app, bad).await.expect("run");
        let row = app.db.read(move |c| rows::get(c, id)).await.expect("get");
        let row = row.expect("row");
        assert_eq!(row.state, JobState::Failed);
        assert_eq!(row.progress, Some(json!({"error": "job failed: boom"})));
    }

    #[tokio::test]
    async fn heavy_lane_waits_for_the_gate() {
        let (_dir, app) = state();
        Scheduler::start(&app);
        app.gate.set_override(Some(Override::Paused));
        let mut events = app.events.subscribe(None).live;
        let (job, ran) = probe(Lane::Heavy, false, 3);
        let id = Scheduler::enqueue(&app, job).await.expect("enqueue");
        tokio::time::sleep(Duration::from_millis(100)).await;
        wait_state(&app, id, JobState::Queued).await;
        app.gate.set_override(None);
        tokio::time::timeout(Duration::from_secs(2), ran.notified())
            .await
            .expect("ran after resume");
        wait_state(&app, id, JobState::Done).await;
        let mut kinds = Vec::new();
        while let Ok(ev) = events.try_recv() {
            kinds.push(ev.kind);
            if ev.kind == EventKind::JobProgress {
                assert!(ev.data.contains(r#""kind":"probe""#));
            }
        }
        assert_eq!(kinds.first(), Some(&EventKind::JobProgress), "queued first");
        assert!(kinds.contains(&EventKind::Status), "waiting list refreshed");
    }

    #[tokio::test]
    async fn a_manual_pause_holds_the_background_lane_but_a_core_does_not() {
        let (_dir, app) = state();
        Scheduler::start(&app);
        app.gate.set_override(Some(Override::Paused));
        let (job, ran) = probe(Lane::Background, false, 5);
        let id = Scheduler::enqueue(&app, job).await.expect("enqueue");
        tokio::time::sleep(Duration::from_millis(100)).await;
        wait_state(&app, id, JobState::Queued).await;
        let status = crate::status::snapshot(&app).await;
        assert_eq!(status.waiting.len(), 1, "the held job is listed");
        app.gate.set_override(None);
        tokio::time::timeout(Duration::from_secs(2), ran.notified())
            .await
            .expect("ran after resume");
        app.gate.set_corename(Some("SNES".into()));
        let (job, ran) = probe(Lane::Background, false, 6);
        Scheduler::enqueue(&app, job).await.expect("enqueue");
        tokio::time::timeout(Duration::from_secs(2), ran.notified())
            .await
            .expect("ran while a core runs");
    }

    #[tokio::test]
    async fn light_lane_ignores_the_gate_and_dedupes() {
        let (_dir, app) = state();
        app.gate.set_override(Some(Override::Paused));
        let (a, _) = probe(Lane::Light, false, 4);
        let (b, _) = probe(Lane::Light, false, 4);
        let first = Scheduler::enqueue(&app, a).await.expect("enqueue");
        let second = Scheduler::enqueue(&app, b).await.expect("enqueue");
        assert_eq!(first, second);
        Scheduler::start(&app);
        Scheduler::start(&app);
        wait_state(&app, first, JobState::Done).await;
    }

    /// Blocks in `run` until released, checkpointing every 10 ms.
    struct Blocker {
        lane: Lane,
        started: Arc<Notify>,
        release: Arc<Notify>,
    }

    #[async_trait]
    impl Job for Blocker {
        fn kind(&self) -> &'static str {
            "blocker"
        }
        fn lane(&self) -> Lane {
            self.lane
        }
        async fn run(&self, ctx: &JobContext) -> Result<()> {
            self.started.notify_one();
            loop {
                ctx.checkpoint().await?;
                tokio::select! {
                    () = self.release.notified() => return Ok(()),
                    () = tokio::time::sleep(Duration::from_millis(10)) => {}
                }
            }
        }
    }

    fn blocker(lane: Lane) -> (Arc<Blocker>, Arc<Notify>, Arc<Notify>) {
        let (started, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
        let job = Arc::new(Blocker {
            lane,
            started: Arc::clone(&started),
            release: Arc::clone(&release),
        });
        (job, started, release)
    }

    #[tokio::test]
    async fn a_running_job_does_not_absorb_a_new_request() {
        let (_dir, app) = state();
        Scheduler::start(&app);
        let (job, started, release) = blocker(Lane::Light);
        let first = Scheduler::enqueue(&app, job).await.expect("enqueue");
        started.notified().await;
        let (again, _, release_again) = blocker(Lane::Light);
        let second = Scheduler::enqueue(&app, again).await.expect("enqueue");
        assert_ne!(first, second);
        release.notify_one();
        wait_state(&app, first, JobState::Done).await;
        release_again.notify_one();
        wait_state(&app, second, JobState::Done).await;
    }

    #[tokio::test]
    async fn stop_cancels_a_running_heavy_job_and_ends_the_lanes() {
        let (_dir, app) = state();
        Scheduler::start(&app);
        assert_eq!(app.scheduler.lanes_alive(), 3);
        let (job, started, _release) = blocker(Lane::Heavy);
        let id = Scheduler::enqueue(&app, job).await.expect("enqueue");
        started.notified().await;
        app.begin_shutdown();
        tokio::time::timeout(
            Duration::from_secs(3),
            app.scheduler.stop(Duration::from_secs(2)),
        )
        .await
        .expect("stop in time");
        assert_eq!(app.scheduler.lanes_alive(), 0);
        let row = app.db.read(move |c| rows::get(c, id)).await.expect("get");
        let row = row.expect("row");
        assert_eq!(row.state, JobState::Queued, "left for the next start");
        assert_eq!(row.lane, "heavy");
    }

    #[tokio::test]
    async fn background_lane_runs_while_a_core_holds_the_heavy_lane() {
        let (_dir, app) = state();
        Scheduler::start(&app);
        app.gate.set_corename(Some("SNES".into()));
        let (heavy, _) = probe(Lane::Heavy, false, 10);
        let held = Scheduler::enqueue(&app, heavy).await.expect("enqueue");
        let (job, ran) = probe(Lane::Background, false, 11);
        let id = Scheduler::enqueue(&app, job).await.expect("enqueue");
        tokio::time::timeout(Duration::from_secs(2), ran.notified())
            .await
            .expect("ran while the core runs");
        wait_state(&app, id, JobState::Done).await;
        wait_state(&app, held, JobState::Queued).await;
        let status = crate::status::snapshot(&app).await;
        assert_eq!(status.waiting.len(), 1);
        assert_eq!(status.waiting[0].id, held);
    }

    #[tokio::test]
    async fn run_now_lasts_until_the_heavy_queue_drains() {
        let (_dir, app) = state();
        Scheduler::start(&app);
        app.gate.set_corename(Some("SNES".into()));
        let (job, ran) = probe(Lane::Heavy, false, 12);
        let id = Scheduler::enqueue(&app, job).await.expect("enqueue");
        app.gate.set_override(Some(Override::Running));
        tokio::time::timeout(Duration::from_secs(2), ran.notified())
            .await
            .expect("ran after run now");
        wait_state(&app, id, JobState::Done).await;
        for _ in 0..200 {
            if app.gate.state().manual.is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(app.gate.state().manual, None);
        assert!(
            app.gate.state().paused(),
            "later work waits for the core again"
        );
    }

    /// Stays in `run` at a checkpoint, so the row is `paused` while the gate is closed.
    struct Singleton;

    #[async_trait]
    impl Job for Singleton {
        fn kind(&self) -> &'static str {
            arcade::KIND
        }
        fn lane(&self) -> Lane {
            Lane::Heavy
        }
        async fn run(&self, ctx: &JobContext) -> Result<()> {
            loop {
                ctx.checkpoint().await?;
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }

    #[tokio::test]
    async fn a_singleton_joins_a_paused_run() {
        let (_dir, app) = state();
        Scheduler::start(&app);
        let first = Scheduler::enqueue(&app, Arc::new(Singleton))
            .await
            .expect("enqueue");
        wait_state(&app, first, JobState::Running).await;
        app.gate.set_corename(Some("NES".into()));
        wait_state(&app, first, JobState::Paused).await;
        let again = Scheduler::enqueue(&app, Arc::new(Singleton))
            .await
            .expect("enqueue");
        assert_eq!(first, again);
        app.begin_shutdown();
        app.scheduler.stop(Duration::from_secs(2)).await;
    }

    #[tokio::test]
    async fn reconcile_requeues_known_kinds_and_drops_repeats() {
        let (dir, app) = state();
        let dat = dir.path().join("gone.dat");
        let (scan, repeat, poll) = app
            .db
            .write(move |c| {
                let path = json!({ "path": dat });
                let scan = rows::insert(c, "dat_import", &path, "heavy", 1)?;
                rows::set_state(c, scan, JobState::Paused, 1)?;
                let repeat = rows::insert(c, "dat_import", &path, "heavy", 1)?;
                let poll = rows::insert(c, "detect_client", &json!({}), "light", 1)?;
                Ok((scan, repeat, poll))
            })
            .await
            .expect("seed");
        let done = reconcile(&app).await.expect("reconcile");
        assert_eq!(
            done,
            Reconciled {
                requeued: 1,
                failed: 1,
                dropped: 1
            }
        );
        let get = |id| {
            let app = Arc::clone(&app);
            async move { app.db.read(move |c| rows::get(c, id)).await.expect("get") }
        };
        let revived = get(scan).await.expect("row");
        assert_eq!(
            (revived.state, revived.lane.as_str()),
            (JobState::Queued, "background")
        );
        assert!(get(repeat).await.is_none());
        let failed = get(poll).await.expect("row");
        assert_eq!(failed.progress, Some(json!({ "error": INTERRUPTED })));
        Scheduler::start(&app);
        wait_state(&app, scan, JobState::Done).await;
    }

    #[test]
    fn revive_covers_the_rerunnable_kinds() {
        for (kind, payload) in [
            ("scan", json!({ "platform_id": null })),
            ("scan", json!({ "platform_id": "nes" })),
            ("recompute_1g1r", json!({ "platform_id": "nes" })),
            ("source_import", json!({ "path": "/s/a.torrent" })),
            ("import", json!({ "download_id": 3 })),
            ("remap_sources", json!({ "platforms": null })),
            ("remap_sources", json!({ "platforms": ["nes"] })),
        ] {
            let job = revive(kind, &payload).expect(kind);
            assert_eq!((job.kind(), job.payload()), (kind, payload));
        }
        let bind = json!({ "path": "/d/a.dat", "dat_version_id": 1, "platform_id": "nes" });
        assert!(revive("dat_import", &bind).is_none());
        assert!(revive("import", &json!({})).is_none());
        assert_eq!(Lane::Heavy.as_str(), "heavy");
    }
}
