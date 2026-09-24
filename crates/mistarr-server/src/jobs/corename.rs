//! Polls CORENAME and feeds the scheduler gate; see `docs/ARCHITECTURE.md` "Pausing for the core".

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use mistarr_mister::corename::{read_corename, CoreState};

use super::gate::{Gate, MENU};

/// Reads CORENAME: `MENU`, a core name, or `None` when the file cannot be read.
///
/// ```
/// let path = std::env::temp_dir().join("mistarr-doc-corename-read");
/// std::fs::write(&path, "MENU\n").unwrap();
/// assert_eq!(mistarr_server::jobs::corename::read(&path).as_deref(), Some("MENU"));
/// assert_eq!(mistarr_server::jobs::corename::read(&path.join("absent")), None);
/// ```
#[must_use]
pub fn read(path: &Path) -> Option<String> {
    match read_corename(path) {
        Ok(CoreState::Menu) => Some(MENU.to_owned()),
        Ok(CoreState::Running(name)) => Some(name),
        Err(_) => None,
    }
}

/// Reads `path` every `every` and records the value on `gate`, forever.
pub async fn watch(path: &Path, every: Duration, gate: Arc<Gate>) {
    let mut tick = tokio::time::interval(every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let value = read(path);
        let before = gate.state().corename;
        if gate.set_corename(value.clone()) {
            tracing::info!(from = ?before, to = ?value, "CORENAME changed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn watcher_follows_the_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("CORENAME");
        std::fs::write(&path, "MENU").expect("write");
        let gate = Arc::new(Gate::new());
        let task = tokio::spawn({
            let (path, gate) = (path.clone(), Arc::clone(&gate));
            async move { watch(&path, Duration::from_millis(10), gate).await }
        });
        let rx = gate.subscribe();
        let wait = |want: bool| {
            let mut rx = rx.clone();
            async move {
                tokio::time::timeout(Duration::from_secs(2), rx.wait_for(|s| s.paused() == want))
                    .await
                    .expect("timely")
                    .map(|_| ())
                    .expect("open");
            }
        };
        gate.subscribe()
            .wait_for(|s| s.corename.is_some())
            .await
            .expect("first read");
        assert!(!gate.state().paused());
        std::fs::write(&path, "SNES").expect("write");
        wait(true).await;
        std::fs::write(&path, "MENU").expect("write");
        wait(false).await;
        task.abort();
    }
}
