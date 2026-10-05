//! Tasks that follow CORENAME, the gate, the download client and the wizard until
//! shutdown; see `docs/ARCHITECTURE.md` "Pausing for the core".

pub mod core_limits;
pub mod corename;
pub mod gate;
pub mod io_priority;
pub mod poll;
pub mod wizard;

use std::future::Future;
use std::time::Duration;

use tokio::task::JoinHandle;

use crate::app::AppState;

/// `base` doubled `n` times, never above `max`: the wait before a retry after `n`
/// failures beyond the first.
///
/// ```
/// use std::time::Duration;
/// use mistarr_server::jobs::watch::doubling;
/// let (base, max) = (Duration::from_secs(30), Duration::from_secs(240));
/// assert_eq!(doubling(base, 0, max), base);
/// assert_eq!(doubling(base, 2, max), Duration::from_secs(120));
/// assert_eq!(doubling(base, 40, max), max);
/// ```
#[must_use]
pub fn doubling(base: Duration, n: u32, max: Duration) -> Duration {
    base.saturating_mul(2u32.saturating_pow(n)).min(max)
}

/// Spawns the watcher `name`, which runs `fut` until it ends or the server shuts down.
pub fn spawn_watcher(
    app: &AppState,
    name: &'static str,
    fut: impl Future<Output = ()> + Send + 'static,
) -> JoinHandle<()> {
    let mut stop = app.shutdown_signal();
    tokio::spawn(async move {
        tokio::select! {
            () = fut => tracing::debug!(watcher = name, "watcher ended"),
            _ = stop.wait_for(|s| *s) => tracing::debug!(watcher = name, "watcher stopped"),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::state;

    #[test]
    fn doubling_starts_at_base_and_stops_at_max() {
        let (base, max) = (Duration::from_secs(1), Duration::from_secs(60));
        let waits: Vec<u64> = (0..8).map(|n| doubling(base, n, max).as_secs()).collect();
        assert_eq!(waits, [1, 2, 4, 8, 16, 32, 60, 60]);
        assert_eq!(doubling(base, u32::MAX, max), max);
    }

    #[tokio::test]
    async fn a_watcher_ends_at_shutdown() {
        let (_dir, app) = state();
        let task = spawn_watcher(&app, "forever", std::future::pending());
        app.begin_shutdown();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("ended in time")
            .expect("joined");
        let done = spawn_watcher(&app, "short", async {});
        done.await.expect("a finished watcher joins");
    }
}
