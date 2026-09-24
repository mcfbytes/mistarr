//! Enqueues one full scan the first time every wizard step reports done; see
//! `docs/ARCHITECTURE.md` "Library scan".

use std::sync::Arc;

use super::scan::ScanJob;
use super::Scheduler;
use crate::app::AppState;
use crate::db::jobs::JobId;
use crate::db::settings::{self, keys};
use crate::error::Result;
use crate::status::wizard_status;

/// Checks whether the wizard just became complete and, the first time it
/// does, marks it in settings and enqueues a full scan. A later call is a
/// no-op even if a step regresses, since the mark is never cleared.
///
/// # Errors
///
/// [`crate::Error::Db`] or [`crate::Error::Stored`] when settings or the job
/// cannot be recorded.
pub async fn on_change(app: &Arc<AppState>) -> Result<Option<JobId>> {
    let done = app
        .db
        .read(|c| settings::get_json::<bool>(c, keys::WIZARD_SCAN_DONE))
        .await?
        .unwrap_or(false);
    if done || !wizard_status(app).await?.complete() {
        return Ok(None);
    }
    app.db
        .write(|c| settings::set_json(c, keys::WIZARD_SCAN_DONE, &true))
        .await?;
    Scheduler::enqueue(app, Arc::new(ScanJob { platform_id: None }))
        .await
        .map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::state;
    use crate::db::jobs as job_rows;
    use crate::jobs::detect_client::ClientStatus;
    use mistarr_clients::ClientKind;

    async fn complete_everything(app: &Arc<AppState>) {
        std::fs::create_dir_all(app.config().paths.games).expect("mkdir");
        app.db
            .write(|c| {
                c.execute_batch(
                    "INSERT INTO dat_versions (dat_name, version, source_file, loaded_at, game_count)
                     VALUES ('Test Console', '1', 'a.dat', 0, 0);
                     INSERT INTO sources (infohash, display_name, origin_file, state, added_at)
                     VALUES ('00', 'n', 'a.torrent', 'unbound', 0);",
                )?;
                let status = ClientStatus {
                    kind: Some(ClientKind::Transmission),
                    url: Some("127.0.0.1:1".into()),
                    reachable: true,
                    version: None,
                    rtorrent_on_path: false,
                    checked_at: 0,
                };
                settings::set_json(c, keys::CLIENT_DETECTED, &status)
            })
            .await
            .expect("seed");
    }

    #[tokio::test]
    async fn fires_once_when_every_step_completes() {
        let (_dir, app) = state();
        assert_eq!(
            on_change(&app).await.expect("run"),
            None,
            "nothing done yet"
        );
        complete_everything(&app).await;
        let id = on_change(&app).await.expect("run").expect("queued");
        let row = app
            .db
            .read(move |c| job_rows::get(c, id))
            .await
            .expect("read")
            .expect("row");
        assert_eq!(row.kind, "scan");
        assert_eq!(
            on_change(&app).await.expect("run"),
            None,
            "already marked done"
        );
    }
}
