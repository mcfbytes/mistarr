//! What tells a running job to stop or wait: shutdown, the gate on its lane, and a
//! cancel the user asked for; see `docs/ARCHITECTURE.md` "Pausing for the core".

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{watch, Notify};

use super::watch::gate::{GateState, PauseReason};
use super::Lane;
use crate::db::ids::JobId;
use crate::db::jobs::{self as rows, JobState};
use crate::db::Db;
use crate::error::{Error, Result};

/// One job's cancel request, set through [`super::Scheduler::cancel`].
#[derive(Debug, Default)]
pub struct Cancel {
    set: AtomicBool,
    notify: Notify,
}

impl Cancel {
    /// Asks the job to stop at its next check.
    ///
    /// ```
    /// let c = mistarr_server::jobs::stop::Cancel::default();
    /// c.cancel();
    /// assert!(c.is_set());
    /// ```
    pub fn cancel(&self) {
        self.set.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    /// Whether the job was asked to stop.
    #[must_use]
    pub fn is_set(&self) -> bool {
        self.set.load(Ordering::SeqCst)
    }

    /// Returns once the job is asked to stop.
    pub async fn wait(&self) {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_set() {
                return;
            }
            notified.await;
        }
    }
}

/// Whether a job must stop or wait, readable from any thread: the server's shutdown,
/// the gate on the job's lane and the job's own [`Cancel`].
#[derive(Clone)]
pub struct StopToken {
    shutdown: watch::Receiver<bool>,
    gate: watch::Receiver<GateState>,
    lane: Lane,
    cancel: Arc<Cancel>,
    row: Option<(Db, JobId)>,
}

impl StopToken {
    /// A token following `shutdown` and `gate` for a job on `lane`, with no row to mark.
    ///
    /// ```
    /// use mistarr_server::jobs::{stop::StopToken, watch::gate::GateState, Lane};
    /// let (_down, shutdown) = tokio::sync::watch::channel(false);
    /// let (_gate, gate) = tokio::sync::watch::channel(GateState::default());
    /// let token = StopToken::new(shutdown, gate, Lane::Heavy, Default::default());
    /// assert!(token.check().is_ok());
    /// ```
    #[must_use]
    pub fn new(
        shutdown: watch::Receiver<bool>,
        gate: watch::Receiver<GateState>,
        lane: Lane,
        cancel: Arc<Cancel>,
    ) -> Self {
        Self {
            shutdown,
            gate,
            lane,
            cancel,
            row: None,
        }
    }

    /// The same token, marking job `id`'s row `paused` while [`StopToken::wait_free_blocking`] waits.
    #[must_use]
    pub fn marking(self, db: Db, id: JobId) -> Self {
        Self {
            row: Some((db, id)),
            ..self
        }
    }

    /// [`Error::Cancelled`] once the server shuts down, [`Error::CancelledByUser`] once
    /// the job was cancelled; the gate is not asked.
    ///
    /// # Errors
    ///
    /// As above.
    pub fn stopped(&self) -> Result<()> {
        if *self.shutdown.borrow() {
            return Err(Error::Cancelled);
        }
        if self.cancel.is_set() {
            return Err(Error::CancelledByUser);
        }
        Ok(())
    }

    /// Whether [`StopToken::stopped`] fails.
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        self.stopped().is_err()
    }

    /// [`StopToken::stopped`], then [`Error::Paused`] while the gate holds the lane.
    ///
    /// # Errors
    ///
    /// As above.
    pub fn check(&self) -> Result<()> {
        self.stopped()?;
        match self.held() {
            Some(_) => Err(Error::Paused),
            None => Ok(()),
        }
    }

    /// Why the gate holds the job's lane, if it does.
    #[must_use]
    pub fn held(&self) -> Option<PauseReason> {
        self.gate.borrow().hold(self.lane)
    }

    /// Whether a core other than the menu is loaded.
    #[must_use]
    pub fn core_running(&self) -> bool {
        self.gate.borrow().core_running()
    }

    /// The error the job stops with, once it must stop.
    pub async fn until_stopped(&self) -> Error {
        let mut shutdown = self.shutdown.clone();
        let down = async {
            // A closed channel never shuts down; only the cancel is left to wait for.
            if shutdown.wait_for(|s| *s).await.is_err() {
                std::future::pending::<()>().await;
            }
        };
        tokio::select! {
            () = down => Error::Cancelled,
            () = self.cancel.wait() => Error::CancelledByUser,
        }
    }

    /// Waits while the gate holds the lane.
    ///
    /// # Errors
    ///
    /// As [`StopToken::stopped`], when the job must stop meanwhile.
    pub async fn wait_free(&self) -> Result<()> {
        let mut gate = self.gate.clone();
        let lane = self.lane;
        let free = async {
            if gate.wait_for(|s| s.hold(lane).is_none()).await.is_err() {
                std::future::pending::<()>().await;
            }
        };
        tokio::select! {
            () = free => self.stopped(),
            e = self.until_stopped() => Err(e),
        }
    }

    /// Waits on the calling thread while the gate holds the lane, looking again every
    /// `poll`, with the row `paused` meanwhile when the token marks one.
    ///
    /// # Errors
    ///
    /// As [`StopToken::stopped`], when the job must stop meanwhile, or
    /// [`Error::Db`] when the row cannot be marked running again.
    pub fn wait_free_blocking(&self, poll: Duration) -> Result<()> {
        let mut marked = false;
        loop {
            self.stopped()?;
            if self.held().is_none() {
                break;
            }
            if !marked {
                marked = true;
                self.mark(JobState::Paused)?;
            }
            std::thread::sleep(poll);
        }
        if marked {
            self.mark(JobState::Running)?;
        }
        Ok(())
    }

    fn mark(&self, state: JobState) -> Result<()> {
        let Some((db, id)) = &self.row else {
            return Ok(());
        };
        let id = *id;
        db.write_blocking(|c| rows::set_state(c, id, state, crate::unix_now()))
    }
}

#[cfg(test)]
impl StopToken {
    /// A token for `lane` whose shutdown stays at `down` and whose gate follows `gate`.
    pub(crate) fn fixed(down: bool, gate: watch::Receiver<GateState>, lane: Lane) -> Self {
        Self::new(watch::channel(down).1, gate, lane, Arc::default())
    }

    /// A token for `lane` whose shutdown stays at `down`, behind an open gate.
    pub(crate) fn open(down: bool, lane: Lane) -> Self {
        Self::fixed(down, watch::channel(GateState::default()).1, lane)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::state;
    use crate::jobs::watch::gate::Override;
    use crate::jobs::JobKind;

    fn token(lane: Lane) -> (watch::Sender<bool>, watch::Sender<GateState>, StopToken) {
        let (down, shutdown) = watch::channel(false);
        let (gate, rx) = watch::channel(GateState::default());
        (
            down,
            gate,
            StopToken::new(shutdown, rx, lane, Arc::default()),
        )
    }

    fn paused() -> GateState {
        GateState {
            corename: None,
            manual: Some(Override::Paused),
        }
    }

    #[test]
    fn check_names_why_a_job_stops() {
        let (down, gate, t) = token(Lane::Background);
        assert!(t.check().is_ok() && !t.is_stopped() && !t.core_running());
        gate.send_replace(paused());
        assert_eq!(t.held(), Some(PauseReason::Manual));
        assert!(matches!(t.check(), Err(Error::Paused)));
        assert!(t.stopped().is_ok(), "a pause is not a stop");
        t.cancel.cancel();
        assert!(matches!(t.check(), Err(Error::CancelledByUser)));
        down.send_replace(true);
        assert!(matches!(t.stopped(), Err(Error::Cancelled)));
        assert!(t.is_stopped());
    }

    #[test]
    fn a_fetch_lane_is_never_held() {
        let (_down, gate, t) = token(Lane::Fetch);
        gate.send_replace(paused());
        assert!(t.check().is_ok());
        gate.send_replace(GateState {
            corename: Some("SNES".into()),
            manual: None,
        });
        assert!(t.core_running());
    }

    #[tokio::test]
    async fn until_stopped_and_wait_free_follow_the_cancel_and_the_gate() {
        let (_down, gate, t) = token(Lane::Heavy);
        gate.send_replace(paused());
        let waiting = tokio::spawn({
            let t = t.clone();
            async move { t.wait_free().await }
        });
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(!waiting.is_finished());
        gate.send_replace(GateState::default());
        waiting.await.expect("join").expect("free");
        let cancel = Arc::clone(&t.cancel);
        let stopped = tokio::spawn(async move { t.until_stopped().await });
        cancel.cancel();
        let e = stopped.await.expect("join");
        assert!(matches!(e, Error::CancelledByUser), "{e:?}");
        let (down, _gate, t) = token(Lane::Heavy);
        down.send_replace(true);
        assert!(matches!(t.until_stopped().await, Error::Cancelled));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_blocking_wait_marks_the_row_paused_until_the_gate_opens() {
        let (_dir, app) = state();
        let id = app
            .db
            .write(|c| {
                let id = rows::insert(
                    c,
                    JobKind::DatImport,
                    &serde_json::json!({}),
                    Lane::Background,
                    1,
                )?;
                rows::set_state(c, id, JobState::Running, 1)?;
                Ok(id)
            })
            .await
            .expect("row");
        let (_down, gate, t) = token(Lane::Background);
        let t = t.marking(app.db.clone(), id);
        gate.send_replace(paused());
        let waiting =
            tokio::task::spawn_blocking(move || t.wait_free_blocking(Duration::from_millis(5)));
        let state = || async {
            let row = app.db.read(move |c| rows::get(c, id)).await.expect("get");
            row.expect("row").state
        };
        for _ in 0..200 {
            if state().await == JobState::Paused {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(state().await, JobState::Paused);
        gate.send_replace(GateState::default());
        waiting.await.expect("join").expect("free");
        assert_eq!(state().await, JobState::Running);
    }
}
