//! The scheduler gate: paused while a core runs, unless the user overrides it.

use serde::Serialize;
use tokio::sync::watch;

use super::Lane;

/// The value MiSTer writes to CORENAME while the menu is loaded.
pub const MENU: &str = "MENU";

/// A manual override set through `/system/pause` or `/system/resume`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Override {
    /// Paused whatever CORENAME says.
    Paused,
    /// Running whatever CORENAME says.
    Running,
}

/// Why the gate is closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PauseReason {
    /// A core other than the menu is running.
    Core,
    /// The user paused.
    Manual,
}

/// The inputs that decide whether gated jobs may run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GateState {
    /// Last CORENAME value; `None` when the file does not exist.
    pub corename: Option<String>,
    /// The manual override, cleared whenever CORENAME changes.
    pub manual: Option<Override>,
}

impl GateState {
    /// True while a core other than the menu is loaded.
    ///
    /// ```
    /// use mistarr_server::jobs::gate::GateState;
    /// let s = GateState { corename: Some("SNES".into()), manual: None };
    /// assert!(s.core_running());
    /// ```
    #[must_use]
    pub fn core_running(&self) -> bool {
        self.corename.as_deref().is_some_and(|n| n != MENU)
    }

    /// Why gated jobs are held, or `None` when they may run.
    ///
    /// ```
    /// use mistarr_server::jobs::gate::{GateState, Override, PauseReason};
    /// let s = GateState { corename: Some("MENU".into()), manual: Some(Override::Paused) };
    /// assert_eq!(s.pause_reason(), Some(PauseReason::Manual));
    /// ```
    #[must_use]
    pub fn pause_reason(&self) -> Option<PauseReason> {
        match self.manual {
            Some(Override::Paused) => Some(PauseReason::Manual),
            Some(Override::Running) => None,
            None => self.core_running().then_some(PauseReason::Core),
        }
    }

    /// True when gated jobs are held.
    ///
    /// ```
    /// assert!(!mistarr_server::jobs::gate::GateState::default().paused());
    /// ```
    #[must_use]
    pub fn paused(&self) -> bool {
        self.pause_reason().is_some()
    }

    /// Why jobs on `lane` are held: the heavy lane follows the gate, the
    /// background lane only a manual pause, the light lane never.
    ///
    /// ```
    /// use mistarr_server::jobs::gate::{GateState, Override, PauseReason};
    /// use mistarr_server::jobs::Lane;
    /// let core = GateState { corename: Some("SNES".into()), manual: None };
    /// assert_eq!(core.hold(Lane::Heavy), Some(PauseReason::Core));
    /// assert_eq!(core.hold(Lane::Background), None);
    /// let user = GateState { corename: None, manual: Some(Override::Paused) };
    /// assert_eq!(user.hold(Lane::Background), Some(PauseReason::Manual));
    /// assert_eq!(user.hold(Lane::Light), None);
    /// ```
    #[must_use]
    pub fn hold(&self, lane: Lane) -> Option<PauseReason> {
        match lane {
            Lane::Heavy => self.pause_reason(),
            Lane::Background => {
                (self.manual == Some(Override::Paused)).then_some(PauseReason::Manual)
            }
            Lane::Light => None,
        }
    }
}

/// Shared gate state with change notification.
pub struct Gate {
    tx: watch::Sender<GateState>,
}

impl Default for Gate {
    fn default() -> Self {
        Self::new()
    }
}

impl Gate {
    /// An open gate with no CORENAME seen.
    ///
    /// ```
    /// assert!(!mistarr_server::jobs::gate::Gate::new().state().paused());
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self {
            tx: watch::Sender::new(GateState::default()),
        }
    }

    /// A copy of the current state.
    ///
    /// ```
    /// assert!(mistarr_server::jobs::gate::Gate::new().state().corename.is_none());
    /// ```
    #[must_use]
    pub fn state(&self) -> GateState {
        self.tx.borrow().clone()
    }

    /// A receiver notified on every change.
    ///
    /// ```
    /// let gate = mistarr_server::jobs::gate::Gate::new();
    /// assert!(!gate.subscribe().borrow().paused());
    /// ```
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<GateState> {
        self.tx.subscribe()
    }

    /// Records a CORENAME reading. A different value clears the manual
    /// override. Returns true if anything changed.
    ///
    /// ```
    /// let gate = mistarr_server::jobs::gate::Gate::new();
    /// assert!(gate.set_corename(Some("N64".into())));
    /// assert!(gate.state().paused());
    /// assert!(!gate.set_corename(Some("N64".into())));
    /// ```
    #[allow(clippy::must_use_candidate)] // Callers may ignore whether it changed.
    pub fn set_corename(&self, corename: Option<String>) -> bool {
        self.tx.send_if_modified(|s| {
            if s.corename == corename {
                return false;
            }
            s.corename = corename;
            s.manual = None;
            true
        })
    }

    /// Sets or clears the manual override. Returns true if it changed.
    ///
    /// ```
    /// use mistarr_server::jobs::gate::{Gate, Override};
    /// let gate = Gate::new();
    /// assert!(gate.set_override(Some(Override::Paused)));
    /// assert!(gate.state().paused());
    /// ```
    #[allow(clippy::must_use_candidate)] // Callers may ignore whether it changed.
    pub fn set_override(&self, manual: Option<Override>) -> bool {
        self.tx.send_if_modified(|s| {
            let changed = s.manual != manual;
            s.manual = manual;
            changed
        })
    }

    /// Clears a `Running` override, for when the heavy queue it was set to
    /// release has drained. Returns true if it was set.
    ///
    /// ```
    /// use mistarr_server::jobs::gate::{Gate, Override};
    /// let gate = Gate::new();
    /// gate.set_corename(Some("SNES".into()));
    /// gate.set_override(Some(Override::Running));
    /// assert!(gate.end_run_now());
    /// assert!(gate.state().paused());
    /// assert!(!gate.end_run_now());
    /// ```
    #[allow(clippy::must_use_candidate)] // Callers may ignore whether it changed.
    pub fn end_run_now(&self) -> bool {
        self.tx.send_if_modified(|s| {
            if s.manual != Some(Override::Running) {
                return false;
            }
            s.manual = None;
            true
        })
    }

    /// Returns once the gate is open; immediately if it already is.
    ///
    /// ```
    /// # tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async {
    /// mistarr_server::jobs::gate::Gate::new().wait_open().await;
    /// # });
    /// ```
    pub async fn wait_open(&self) {
        self.wait_free(Lane::Heavy).await;
    }

    /// Waits until jobs on `lane` are no longer held; see [`GateState::hold`].
    ///
    /// ```
    /// # tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async {
    /// let gate = mistarr_server::jobs::gate::Gate::new();
    /// gate.wait_free(mistarr_server::jobs::Lane::Background).await;
    /// # });
    /// ```
    pub async fn wait_free(&self, lane: Lane) {
        let mut rx = self.tx.subscribe();
        // The sender lives in `self`, so the channel cannot close while this borrow is held.
        let _ = rx.wait_for(|s| s.hold(lane).is_none()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn corename_drives_the_gate() {
        let gate = Gate::new();
        assert!(gate.set_corename(Some(MENU.into())));
        assert!(!gate.state().paused());
        gate.set_corename(Some("SNES".into()));
        assert_eq!(gate.state().pause_reason(), Some(PauseReason::Core));
        gate.set_corename(None);
        assert!(!gate.state().paused());
    }

    #[test]
    fn override_wins_until_corename_changes() {
        let gate = Gate::new();
        gate.set_corename(Some("SNES".into()));
        gate.set_override(Some(Override::Running));
        assert!(!gate.state().paused());
        assert!(!gate.set_override(Some(Override::Running)));
        gate.set_corename(Some(MENU.into()));
        assert_eq!(gate.state().manual, None);
        gate.set_override(Some(Override::Paused));
        assert_eq!(gate.state().pause_reason(), Some(PauseReason::Manual));
        gate.set_override(None);
        assert!(!gate.state().paused());
    }

    #[tokio::test]
    async fn wait_open_blocks_while_paused() {
        let gate = Arc::new(Gate::new());
        gate.set_override(Some(Override::Paused));
        let g = Arc::clone(&gate);
        let waiter = tokio::spawn(async move { g.wait_open().await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiter.is_finished());
        let rx = gate.subscribe();
        gate.set_override(None);
        assert!(rx.has_changed().expect("open"));
        tokio::time::timeout(Duration::from_secs(2), waiter)
            .await
            .expect("resumed")
            .expect("join");
    }
}
