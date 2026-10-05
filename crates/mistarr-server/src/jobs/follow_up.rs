//! The work that follows a change to the catalogue, queued in one order; see
//! `docs/ARCHITECTURE.md` "DAT import" step 3.

use std::sync::Arc;

use mistarr_core::PlatformId;

use super::dat_import::Recompute;
use super::remap::RemapSources;
use super::{chd, scan, source_import, Scheduler};
use crate::app::AppState;

/// Queues what follows a change to the roms of `platforms`, in order: each platform's
/// recompute, or when `recomputed` says it already ran, what a recompute ends with
/// ([`recomputed`]); a scan of each platform whose games directory exists; then the
/// binding of the sources waiting for a DAT. A failure is logged and the rest goes on.
pub async fn catalogue_changed(app: &Arc<AppState>, platforms: &[PlatformId], recomputed: bool) {
    for platform in platforms {
        if recomputed {
            self::recomputed(app, platform).await;
        } else {
            Scheduler::submit(app, Arc::new(Recompute::new(&platform.0))).await;
        }
    }
    for platform in platforms {
        if let Err(e) = scan::enqueue_if_games_dir_exists(app, platform).await {
            tracing::warn!(platform = %platform.0, error = %e, "cannot enqueue automatic scan");
        }
    }
    if platforms.is_empty() {
        return;
    }
    if let Err(e) = source_import::rebind_waiting(app).await {
        tracing::warn!(error = %e, "cannot bind waiting sources");
    }
}

/// Queues what a recompute of `platform` ends with: a re-map of its bound sources, whose
/// groups are settled now, then CHD decoding, since new roms may fit track layouts no
/// DAT had before.
pub async fn recomputed(app: &Arc<AppState>, platform: &PlatformId) {
    let remap = RemapSources {
        platforms: Some(vec![platform.clone()]),
    };
    Scheduler::submit(app, Arc::new(remap)).await;
    if let Err(e) = chd::queue_for(app, platform, true).await {
        tracing::warn!(platform = %platform.0, error = %e, "cannot queue CHD decoding");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::state;
    use crate::db::jobs::{self as rows, JobRow};
    use crate::jobs::JobKind;

    async fn open(app: &AppState) -> Vec<JobRow> {
        app.db.read(rows::open_rows).await.expect("rows")
    }

    #[tokio::test]
    async fn a_change_not_yet_recomputed_queues_the_recompute_and_the_scan() {
        let (_dir, app) = state();
        let nes = PlatformId("nes".into());
        let games = app.config().paths.games.join("NES");
        std::fs::create_dir_all(&games).expect("games");
        catalogue_changed(&app, std::slice::from_ref(&nes), false).await;
        let kinds: Vec<JobKind> = open(&app).await.iter().map(|r| r.kind).collect();
        assert_eq!(kinds, [JobKind::Recompute, JobKind::Scan]);
    }

    #[tokio::test]
    async fn a_recomputed_change_queues_the_remap_instead() {
        let (_dir, app) = state();
        let nes = PlatformId("nes".into());
        catalogue_changed(&app, std::slice::from_ref(&nes), true).await;
        let found = open(&app).await;
        assert_eq!(found.len(), 1, "no games directory, so no scan");
        assert_eq!(found[0].kind, JobKind::RemapSources);
        assert_eq!(
            found[0].payload,
            serde_json::json!({ "platforms": ["nes"] })
        );
        catalogue_changed(&app, &[], true).await;
        assert_eq!(open(&app).await.len(), 1);
    }
}
