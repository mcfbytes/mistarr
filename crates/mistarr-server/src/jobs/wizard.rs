//! Enqueues one full scan the first time every wizard step reports done; see
//! `docs/ARCHITECTURE.md` "Library scan".

use std::sync::Arc;

use rusqlite::Connection;

use super::scan::ScanJob;
use super::Scheduler;
use crate::app::AppState;
use crate::db::jobs::JobId;
use crate::db::settings::{self, keys};
use crate::error::Result;
use crate::status::wizard_status;

/// Checks whether the wizard just became complete and, the first time it
/// does, enqueues a full scan. A later call is a no-op even if a step
/// regresses, since the mark is never cleared once the scan is queued.
///
/// # Errors
///
/// [`crate::Error::Db`] or [`crate::Error::Stored`] when settings or the job
/// cannot be recorded.
pub async fn on_change(app: &Arc<AppState>) -> Result<Option<JobId>> {
    if !wizard_status(app).await?.complete() {
        return Ok(None);
    }
    // Claim the one-time scan atomically, so two concurrent callers cannot
    // both pass the check; only the claiming call enqueues.
    if !app.db.write(claim_scan).await? {
        return Ok(None);
    }
    match Scheduler::enqueue(app, Arc::new(ScanJob { platform_id: None })).await {
        Ok(id) => Ok(Some(id)),
        Err(e) => {
            // The scan never got queued: release the claim so a later call retries.
            if let Err(unclaim) = app
                .db
                .write(|c| settings::set_json(c, keys::WIZARD_SCAN_DONE, &false))
                .await
            {
                tracing::warn!(error = %unclaim, "cannot release the wizard scan claim");
            }
            Err(e)
        }
    }
}

/// Sets `keys::WIZARD_SCAN_DONE` from unset or false to true and reports
/// whether this call was the one that set it.
fn claim_scan(conn: &mut Connection) -> Result<bool> {
    if settings::get_json::<bool>(conn, keys::WIZARD_SCAN_DONE)?.unwrap_or(false) {
        return Ok(false);
    }
    settings::set_json(conn, keys::WIZARD_SCAN_DONE, &true)?;
    Ok(true)
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
                    ..ClientStatus::default()
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

    #[tokio::test]
    async fn concurrent_calls_enqueue_only_one_scan() {
        let (_dir, app) = state();
        complete_everything(&app).await;
        let (a, b) = tokio::join!(on_change(&app), on_change(&app));
        let ids: Vec<_> = [a.expect("run"), b.expect("run")]
            .into_iter()
            .flatten()
            .collect();
        assert_eq!(ids.len(), 1, "exactly one caller claims the scan");
        let n = app
            .db
            .read(|c| job_rows::count_kind(c, "scan"))
            .await
            .expect("count");
        assert_eq!(n, 1);
    }
}
