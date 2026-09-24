//! Background jobs: the [`Job`] trait, two serial lanes and the gate; see `docs/ARCHITECTURE.md`.

pub mod corename;
pub mod dat_import;
pub mod detect_client;
pub mod gate;
pub mod import;
pub mod scan;
pub mod source_import;

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

/// Which serial queue a job runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    /// Hashing, scanning and importing: one at a time, held by the gate.
    Heavy,
    /// Client polling, detection and binding: one at a time, never held.
    Light,
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
}

/// The `job.progress` event body.
#[derive(Debug, Clone, Serialize)]
struct ProgressEvent<'a> {
    id: JobId,
    kind: &'a str,
    state: JobState,
    progress: &'a Value,
}

impl JobContext {
    /// Stores `progress` on the job row and publishes `job.progress`.
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
        self.publish(JobState::Running, &progress);
        Ok(())
    }

    /// Fails with [`Error::Cancelled`] once the server is shutting down. For
    /// heavy jobs, also waits while the gate is closed, marking the row `paused`.
    ///
    /// # Errors
    ///
    /// [`Error::Cancelled`] on shutdown, [`Error::Db`] when the row state cannot be updated.
    pub async fn checkpoint(&self) -> Result<()> {
        let mut stop = self.app.shutdown_signal();
        if *stop.borrow() {
            return Err(Error::Cancelled);
        }
        if self.lane == Lane::Light || !self.app.gate.state().paused() {
            return Ok(());
        }
        set_state(&self.app, self.id, JobState::Paused).await?;
        tokio::select! {
            () = self.app.gate.wait_open() => {}
            _ = stop.wait_for(|s| *s) => return Err(Error::Cancelled),
        }
        set_state(&self.app, self.id, JobState::Running).await
    }

    fn publish(&self, state: JobState, progress: &Value) {
        let body = ProgressEvent {
            id: self.id,
            kind: self.kind,
            state,
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

type Receivers = (
    mpsc::UnboundedReceiver<Queued>,
    mpsc::UnboundedReceiver<Queued>,
);

/// Queues jobs onto the two lanes and records them in `jobs`.
pub struct Scheduler {
    heavy: mpsc::UnboundedSender<Queued>,
    light: mpsc::UnboundedSender<Queued>,
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
        let (heavy, heavy_rx) = mpsc::unbounded_channel();
        let (light, light_rx) = mpsc::unbounded_channel();
        Self {
            heavy,
            light,
            receivers: Mutex::new(Some((heavy_rx, light_rx))),
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
        let Some((heavy, light)) = taken else {
            return;
        };
        let sched = &app.scheduler;
        let mut lanes = sched.lanes.lock().unwrap_or_else(PoisonError::into_inner);
        for (kind, rx) in [(Lane::Heavy, heavy), (Lane::Light, light)] {
            sched.alive.fetch_add(1, Ordering::SeqCst);
            let guard = LaneGuard(Arc::clone(&sched.alive));
            lanes.push(tokio::spawn(lane(Arc::clone(app), kind, rx, guard)));
        }
    }

    /// Records a queued job and hands it to its lane. A job with the same kind
    /// and payload that has not started yet is returned instead of a second one.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Db`] when the row cannot be written, [`crate::Error::Job`]
    /// when the lane has stopped.
    pub async fn enqueue(app: &Arc<AppState>, job: Arc<dyn Job>) -> Result<JobId> {
        let (kind, payload) = (job.kind(), job.payload());
        let (id, fresh) = app
            .db
            .write(move |c| {
                if let Some(id) = rows::find_queued(c, kind, &payload)? {
                    return Ok((id, false));
                }
                Ok((rows::insert(c, kind, &payload, crate::unix_now())?, true))
            })
            .await?;
        if fresh {
            let tx = match job.lane() {
                Lane::Heavy => &app.scheduler.heavy,
                Lane::Light => &app.scheduler.light,
            };
            tx.send(Queued { id, job })
                .map_err(|_| crate::Error::Job("scheduler has stopped".to_owned()))?;
        }
        Ok(id)
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
            .write(move |c| rows::insert(c, kind, &payload, crate::unix_now()))
            .await?;
        execute(app, id, job.as_ref(), Lane::Light).await?;
        Ok(id)
    }
}

/// Runs queued jobs one at a time until shutdown. A heavy job waits for the
/// gate before it starts and stays `queued` meanwhile.
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
        if lane == Lane::Heavy {
            tokio::select! {
                () = app.gate.wait_open() => {}
                _ = stop.wait_for(|s| *s) => break,
            }
        }
        if let Err(e) = execute(&app, id, job.as_ref(), lane).await {
            tracing::warn!(job = %id, error = %e, "cannot record job state");
        }
    }
}

/// Runs one job and records its outcome; only bookkeeping failures are returned.
async fn execute(app: &Arc<AppState>, id: JobId, job: &dyn Job, lane: Lane) -> Result<()> {
    let ctx = JobContext {
        id,
        kind: job.kind(),
        app: Arc::clone(app),
        lane,
    };
    set_state(app, id, JobState::Running).await?;
    tracing::debug!(job = %id, kind = ctx.kind, "job started");
    let (state, progress) = match job.run(&ctx).await {
        Ok(()) => (JobState::Done, None),
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
        let ev = events.recv().await.expect("progress event");
        assert_eq!(ev.kind, EventKind::JobProgress);
        assert!(ev.data.contains(r#""kind":"probe""#));
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
        assert_eq!(app.scheduler.lanes_alive(), 2);
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
        assert_eq!(row.state, JobState::Failed);
        assert_eq!(
            row.progress,
            Some(json!({"error": "cancelled by shutdown"}))
        );
    }
}
