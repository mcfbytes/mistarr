//! Live job progress held in memory and sent as transient events; `docs/API.md` "Events".

use std::collections::HashMap;
use std::io::Read;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::ProgressEvent;
use crate::app::AppState;
use crate::db::jobs::{JobId, JobRow, JobState};
use crate::events::EventKind;

/// Shortest gap between two live reports of one job in the same phase.
pub const REPORT_EVERY: Duration = Duration::from_millis(250);

/// A [`Read`] that counts the bytes it hands out and passes the running total to a callback.
pub struct CountingReader<R, F> {
    inner: R,
    read: u64,
    on_read: F,
}

impl<R: Read, F: FnMut(u64)> CountingReader<R, F> {
    /// Wraps `inner`; `on_read` gets the total after every read that returned bytes.
    ///
    /// ```
    /// use std::io::Read;
    /// let mut seen = 0;
    /// let mut r = mistarr_server::jobs::progress::CountingReader::new(&b"abcd"[..], |n| seen = n);
    /// let mut out = Vec::new();
    /// r.read_to_end(&mut out).unwrap();
    /// assert_eq!(r.count(), 4);
    /// drop(r);
    /// assert_eq!(seen, 4);
    /// ```
    pub fn new(inner: R, on_read: F) -> Self {
        Self {
            inner,
            read: 0,
            on_read,
        }
    }

    /// Bytes read so far.
    #[must_use]
    pub fn count(&self) -> u64 {
        self.read
    }
}

impl<R: Read, F: FnMut(u64)> Read for CountingReader<R, F> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.read += n as u64;
            (self.on_read)(self.read);
        }
        Ok(n)
    }
}

/// Lets a report through when its phase changed or [`Throttle::every`] has passed.
#[derive(Debug, Clone)]
pub struct Throttle {
    every: Duration,
    last: Option<(Instant, String)>,
}

impl Throttle {
    /// A throttle passing at most one report per `every` within a phase.
    ///
    /// ```
    /// use std::time::{Duration, Instant};
    /// let mut t = mistarr_server::jobs::progress::Throttle::new(Duration::from_millis(250));
    /// let now = Instant::now();
    /// assert!(t.due(now, "reading"));
    /// assert!(!t.due(now + Duration::from_millis(100), "reading"));
    /// assert!(t.due(now + Duration::from_millis(100), "storing"));
    /// ```
    #[must_use]
    pub fn new(every: Duration) -> Self {
        Self { every, last: None }
    }

    /// The gap this throttle keeps.
    #[must_use]
    pub fn every(&self) -> Duration {
        self.every
    }

    /// True, and remembered as the last report, when a report in `phase` at `now` may go out.
    pub fn due(&mut self, now: Instant, phase: &str) -> bool {
        let pass = match &self.last {
            None => true,
            Some((at, last)) => last != phase || now.duration_since(*at) >= self.every,
        };
        if pass {
            self.last = Some((now, phase.to_owned()));
        }
        pass
    }
}

/// The latest live progress of each running job, never written to the database.
#[derive(Debug, Default)]
pub struct LiveProgress {
    jobs: Mutex<HashMap<JobId, Value>>,
}

impl LiveProgress {
    /// Records `progress` as job `id`'s latest.
    pub fn set(&self, id: JobId, progress: Value) {
        self.lock().insert(id, progress);
    }

    /// Job `id`'s latest live progress.
    #[must_use]
    pub fn get(&self, id: JobId) -> Option<Value> {
        self.lock().get(&id).cloned()
    }

    /// Forgets job `id`, once it has finished.
    pub fn clear(&self, id: JobId) {
        self.lock().remove(&id);
    }

    /// Replaces the stored progress of each running row with its live progress, if any.
    ///
    /// ```
    /// use mistarr_server::db::jobs::{JobId, JobRow, JobState};
    /// use mistarr_server::jobs::progress::LiveProgress;
    /// let live = LiveProgress::default();
    /// live.set(JobId(1), serde_json::json!({ "phase": "reading" }));
    /// let mut rows = vec![JobRow { id: JobId(1), kind: "dat_import".into(), lane: "background".into(),
    ///     payload: serde_json::json!({}), state: JobState::Running, progress: None,
    ///     created_at: 0, updated_at: 0 }];
    /// live.overlay(&mut rows);
    /// assert_eq!(rows[0].progress.as_ref().unwrap()["phase"], "reading");
    /// ```
    pub fn overlay(&self, rows: &mut [JobRow]) {
        let jobs = self.lock();
        for row in rows.iter_mut().filter(|r| r.state == JobState::Running) {
            if let Some(p) = jobs.get(&row.id) {
                row.progress = Some(p.clone());
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<JobId, Value>> {
        self.jobs.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Sends a running job's progress to [`LiveProgress`] and as a transient
/// `job.progress` event, through a [`Throttle`]; usable from a blocking thread.
pub struct Reporter {
    app: Arc<AppState>,
    id: JobId,
    kind: &'static str,
    detail: Option<String>,
    throttle: Mutex<Throttle>,
}

impl Reporter {
    /// A reporter for job `id` of `kind` about `detail`.
    #[must_use]
    pub fn new(app: Arc<AppState>, id: JobId, kind: &'static str, detail: Option<String>) -> Self {
        Self {
            app,
            id,
            kind,
            detail,
            throttle: Mutex::new(Throttle::new(REPORT_EVERY)),
        }
    }

    /// Reports the progress `build` makes for `phase` when the throttle lets it
    /// through, so a report held back costs nothing; returns whether it went out.
    pub fn report(&self, phase: &str, build: impl FnOnce() -> Value) -> bool {
        let due = self
            .throttle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .due(Instant::now(), phase);
        if due {
            let progress = build();
            self.app.live.set(self.id, progress.clone());
            let body = ProgressEvent {
                id: self.id,
                kind: self.kind,
                state: JobState::Running,
                detail: self.detail.as_deref(),
                progress: &progress,
            };
            self.app
                .events
                .publish_transient(EventKind::JobProgress, &body);
        }
        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::state;
    use serde_json::json;

    #[test]
    fn counting_reads_every_byte_through_a_buffer() {
        let data = vec![7u8; 20_000];
        let mut totals = Vec::new();
        {
            let mut r = CountingReader::new(&data[..], |n| totals.push(n));
            let mut buf = [0u8; 8192];
            while r.read(&mut buf).expect("read") > 0 {}
            assert_eq!(r.count(), 20_000);
        }
        assert_eq!(totals, [8192, 16_384, 20_000]);
    }

    #[test]
    fn the_throttle_passes_one_report_per_gap_and_every_phase_change() {
        let mut t = Throttle::new(Duration::from_millis(250));
        assert_eq!(t.every(), Duration::from_millis(250));
        let start = Instant::now();
        let passed: Vec<bool> = (0..10u64)
            .map(|i| t.due(start + Duration::from_millis(i * 100), "reading"))
            .collect();
        assert_eq!(
            passed,
            [true, false, false, true, false, false, true, false, false, true]
        );
        assert!(t.due(start + Duration::from_millis(901), "storing"));
        assert!(!t.due(start + Duration::from_millis(902), "storing"));
    }

    #[test]
    fn live_progress_is_kept_until_cleared() {
        let live = LiveProgress::default();
        live.set(JobId(3), json!({ "phase": "reading" }));
        assert_eq!(live.get(JobId(3)), Some(json!({ "phase": "reading" })));
        live.clear(JobId(3));
        assert_eq!(live.get(JobId(3)), None);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reports_arrive_while_the_writer_is_held() {
        let (_dir, app) = state();
        let mut sub = app.events.subscribe(None);
        let (held, is_held) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let db = app.db.clone();
        let holder = std::thread::spawn(move || {
            db.write_blocking(|c| {
                let tx = c.transaction()?;
                crate::db::settings::set(&tx, "test.held", "1")?;
                let _ = held.send(());
                let _ = released.recv();
                crate::db::commit(tx)
            })
        });
        is_held.recv().expect("held");
        let reporter = Reporter::new(
            Arc::clone(&app),
            JobId(9),
            "dat_import",
            Some("a.dat".into()),
        );
        let progress = json!({ "phase": "reading", "bytes_read": 10, "bytes_total": 100 });
        let sent = tokio::task::spawn_blocking(move || {
            let first = reporter.report("reading", || progress);
            let held_back = reporter.report("reading", || json!({ "phase": "reading" }));
            (first, held_back)
        })
        .await
        .expect("report");
        assert_eq!(sent, (true, false));
        let event = tokio::time::timeout(Duration::from_secs(1), sub.live.recv())
            .await
            .expect("in time")
            .expect("event");
        assert_eq!(event.kind, EventKind::JobProgress);
        assert_eq!(event.seq, 0, "transient events take no ring slot");
        let body: Value = serde_json::from_str(&event.data).expect("json");
        assert_eq!(body["detail"], "a.dat");
        assert_eq!(body["progress"]["bytes_read"], 10);
        assert_eq!(app.live.get(JobId(9)).expect("live")["phase"], "reading");
        release.send(()).expect("release");
        holder.join().expect("join").expect("write");
    }
}
