//! The System routes of `docs/API.md`.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use mistarr_core::PlatformId;
use serde::{Deserialize, Serialize};

use super::{ApiError, Page, Paging};
use crate::app::AppState;
use crate::config::{RuntimeSettings, SettingsPatch};
use crate::db::jobs::{self, JobId, JobRow};
use crate::db::platforms;
use crate::db::settings::{self, keys};
use crate::events::EventKind;
use crate::jobs::dat_import::Recompute;
use crate::jobs::detect_client::{detect_and_store, ClientStatus, DetectClient};
use crate::jobs::gate::{GateState, Override};
use crate::jobs::scan::{is_arcade, ScanJob};
use crate::jobs::Scheduler;
use crate::status::{hold_reason, snapshot, wizard_status, Status};
use axum::http::StatusCode;
use mistarr_clients::launch::Launcher;
use mistarr_clients::ClientKind;

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/system/status", get(status))
        .route("/system/wizard", get(wizard))
        .route("/system/wizard/done", post(wizard_done))
        .route("/system/scan", post(scan))
        .route("/system/cores", post(cores))
        .route("/system/pause", post(pause))
        .route("/system/resume", post(resume))
        .route("/system/jobs", get(list_jobs))
        .route("/system/jobs/recent", get(recent_jobs))
        .route("/system/client/start", post(start_client))
        .route("/system/settings", get(get_settings).put(put_settings))
}

/// `POST /system/scan` body: an omitted or empty body scans every platform.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ScanBody {
    platform_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct ScanResponse {
    /// `null` for a request naming `arcade` alone: it never gets a generic
    /// scan job, only the arcade catalogue queued as `arcade_job_id`.
    job_id: Option<JobId>,
    /// The arcade catalogue queued with a scan of every platform or of `arcade`.
    #[serde(skip_serializing_if = "Option::is_none")]
    arcade_job_id: Option<JobId>,
}

async fn scan(
    State(app): State<Arc<AppState>>,
    body: Bytes,
) -> Result<Json<ScanResponse>, ApiError> {
    let body: ScanBody = if body.is_empty() {
        ScanBody::default()
    } else {
        serde_json::from_slice(&body).map_err(|e| ApiError::bad_request(e.to_string()))?
    };
    let platform_id = match body.platform_id {
        Some(raw) => Some(validate_platform(&app, raw).await?),
        None => None,
    };
    let arcade_only = platform_id.as_ref().is_some_and(is_arcade);
    let covers_arcade = platform_id.is_none() || arcade_only;
    // Arcade never gets a generic scan job: its presence and verification
    // come from the arcade catalogue, queued below as `arcade_job_id`.
    let job_id = if arcade_only {
        None
    } else {
        Some(Scheduler::enqueue(&app, Arc::new(ScanJob { platform_id })).await?)
    };
    let arcade_job_id = if covers_arcade {
        crate::jobs::arcade::enqueue_if_relevant(&app).await?
    } else {
        None
    };
    Ok(Json(ScanResponse {
        job_id,
        arcade_job_id,
    }))
}

/// `POST /system/cores` answer.
#[derive(Debug, Serialize)]
struct CoresResponse {
    platforms: Vec<PlatformId>,
    arcade_job_id: Option<JobId>,
}

/// Detects installed cores again, for the wizard's detected-cores step, and
/// queues the arcade catalogue when there are MRA files to read.
async fn cores(State(app): State<Arc<AppState>>) -> Result<Json<CoresResponse>, ApiError> {
    let platforms = {
        let app = Arc::clone(&app);
        crate::threads::blocking(crate::threads::label::DETECT, move || {
            crate::app::detect_cores(&app)
        })
        .await
        .map_err(|e| crate::Error::Task(e.to_string()))??
    };
    let arcade_job_id = crate::jobs::arcade::enqueue_if_relevant(&app).await?;
    Ok(Json(CoresResponse {
        platforms,
        arcade_job_id,
    }))
}

/// Looks up `raw` among the seeded platforms, for `POST /system/scan`.
///
/// # Errors
///
/// [`ApiError`] 404 when no such platform exists, 400 when it is disabled.
async fn validate_platform(app: &AppState, raw: String) -> Result<PlatformId, ApiError> {
    let id = PlatformId(raw);
    let row = app
        .db
        .read({
            let id = id.clone();
            move |c| platforms::find(c, &id)
        })
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no such platform `{}`", id.0)))?;
    if !row.enabled {
        return Err(ApiError::bad_request(format!(
            "platform `{}` is disabled",
            id.0
        )));
    }
    Ok(id)
}

async fn status(State(app): State<Arc<AppState>>) -> Json<Status> {
    Json(snapshot(&app).await)
}

/// `GET /system/wizard`: which first-run steps are complete.
#[derive(Debug, Serialize)]
#[allow(clippy::struct_excessive_bools)] // One flag per wizard step is the JSON shape.
struct Wizard {
    paths: bool,
    dats: bool,
    client: bool,
    sources: bool,
    open_on_start: bool,
}

async fn wizard(State(app): State<Arc<AppState>>) -> Result<Json<Wizard>, ApiError> {
    let w = wizard_status(&app).await?;
    let dismissed = app
        .db
        .read(|c| settings::get_json::<bool>(c, keys::WIZARD_DISMISSED))
        .await?
        .unwrap_or(false);
    Ok(Json(Wizard {
        paths: w.paths,
        dats: w.dats,
        client: w.client,
        sources: w.sources,
        open_on_start: !dismissed && !w.dats,
    }))
}

/// `POST /system/wizard/done`: records that the user finished or dismissed the
/// wizard, which then stays closed on load.
async fn wizard_done(State(app): State<Arc<AppState>>) -> Result<Json<Wizard>, ApiError> {
    app.db
        .write(|c| settings::set_json(c, keys::WIZARD_DISMISSED, &true))
        .await?;
    wizard(State(app)).await
}

/// Rejects a remote path map entry whose remote path is blank, which would
/// match every path the client reports, or whose local path is not absolute.
/// The remote side is the client's own spelling, so `C:\\x` or `C:/x` pass.
fn check_path_map(patch: &SettingsPatch) -> Result<(), ApiError> {
    let Some(client) = &patch.client else {
        return Ok(());
    };
    if client.remote_path_map.iter().any(|m| !path_map_entry_ok(m)) {
        return Err(ApiError::bad_request(
            "Each remote path map entry needs a remote path and an absolute local path.",
        ));
    }
    Ok(())
}

fn path_map_entry_ok(m: &mistarr_clients::PathMapping) -> bool {
    !m.remote.to_string_lossy().trim().is_empty() && m.local.is_absolute()
}

/// `POST /system/client/start` body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StartBody {
    kind: ClientKind,
}

/// Starts an installed client that is not running, then detects again until it
/// answers or `client_start_wait` passes; see `docs/DOWNLOAD-CLIENTS.md`.
async fn start_client(
    State(app): State<Arc<AppState>>,
    body: Bytes,
) -> Result<Json<Status>, ApiError> {
    let body: StartBody =
        serde_json::from_slice(&body).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let Ok(_starting) = app.client_start.try_lock() else {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "busy",
            "A download client is already being started.",
        ));
    };
    let current = app
        .db
        .read(|c| settings::get_json::<ClientStatus>(c, keys::CLIENT_DETECTED))
        .await?;
    if current.is_some_and(|c| c.usable()) {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "conflict",
            "A download client is already running.",
        ));
    }
    let launcher = app.launcher();
    if !launcher_offers(&launcher, body.kind) {
        return Err(ApiError::bad_request(format!(
            "{} is not installed on this system.",
            body.kind
        )));
    }
    tracing::info!(kind = body.kind.as_str(), "starting the download client");
    let priority = app.io_priority.clone();
    // The client runs at the default I/O class; the gate's rate limit slows it while a core runs.
    crate::threads::blocking(crate::threads::label::LAUNCH, move || match priority {
        Some(p) => p.at_default(|| launcher.start(body.kind)),
        None => launcher.start(body.kind),
    })
    .await
    .map_err(|e| crate::Error::Task(e.to_string()))?
    .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string()))?;
    let deadline = tokio::time::Instant::now() + app.options.client_start_wait;
    loop {
        let found = detect_and_store(&app, true).await?;
        if found.usable() || tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    Ok(Json(snapshot(&app).await))
}

fn launcher_offers(launcher: &Launcher, kind: ClientKind) -> bool {
    let installed = launcher.installed();
    match kind {
        ClientKind::Transmission => {
            installed.transmission_on_path || installed.transmission_service
        }
        ClientKind::Rtorrent => installed.rtorrent_on_path,
        _ => false,
    }
}

async fn pause(State(app): State<Arc<AppState>>) -> Json<Status> {
    app.gate.set_override(Some(Override::Paused));
    Json(snapshot(&app).await)
}

async fn resume(State(app): State<Arc<AppState>>) -> Json<Status> {
    app.gate.set_override(Some(Override::Running));
    Json(snapshot(&app).await)
}

/// A `/system/jobs` item: the row plus why it is not running.
#[derive(Debug, Serialize)]
struct JobItem {
    #[serde(flatten)]
    row: JobRow,
    reason: Option<String>,
}

async fn list_jobs(
    State(app): State<Arc<AppState>>,
    paging: Result<Query<Paging>, QueryRejection>,
) -> Result<Json<Page<JobItem>>, ApiError> {
    let Query(paging) = paging.map_err(|e| ApiError::bad_request(e.body_text()))?;
    let (limit, offset) = paging.resolve();
    let ((mut rows, total), open) = app
        .db
        .read(move |c| Ok((jobs::list_active(c, limit, offset)?, jobs::open_rows(c)?)))
        .await?;
    app.live.overlay(&mut rows);
    let gate = app.gate.state();
    let items = rows
        .into_iter()
        .map(|row| JobItem {
            reason: job_reason(&gate, &row, &open),
            row,
        })
        .collect();
    Ok(Json(Page { items, total }))
}

/// Why an open job is not running: the gate's hold, else, while queued, what it waits for.
fn job_reason(gate: &GateState, row: &JobRow, open: &[JobRow]) -> Option<String> {
    hold_reason(gate, &row.lane, row.state).or_else(|| {
        (row.state == jobs::JobState::Queued).then(|| crate::incoming::queued_reason(row, open))
    })
}

/// Finished jobs `/system/jobs/recent` lists.
const RECENT_JOBS: u32 = 10;

async fn recent_jobs(State(app): State<Arc<AppState>>) -> Result<Json<Page<JobItem>>, ApiError> {
    let rows = app
        .db
        .read(|c| jobs::recent_finished(c, RECENT_JOBS))
        .await?;
    let items: Vec<JobItem> = rows
        .into_iter()
        .map(|row| JobItem { row, reason: None })
        .collect();
    let total = u64::try_from(items.len()).unwrap_or(0);
    Ok(Json(Page { items, total }))
}

async fn get_settings(State(app): State<Arc<AppState>>) -> Json<RuntimeSettings> {
    Json(app.config().runtime())
}

async fn put_settings(
    State(app): State<Arc<AppState>>,
    body: Bytes,
) -> Result<Json<RuntimeSettings>, ApiError> {
    let patch: SettingsPatch =
        serde_json::from_slice(&body).map_err(|e| ApiError::bad_request(e.to_string()))?;
    check_path_map(&patch)?;
    let before = app.config();
    let (prefs_before, scan_before) = (before.prefs, before.scan);
    let (runtime, client_changed) = app.update_settings(&patch).await?;
    if runtime.scan != Some(scan_before) {
        crate::jobs::chd::apply_setting(&app).await?;
    }
    if client_changed {
        Scheduler::enqueue(&app, Arc::new(DetectClient)).await?;
    }
    if runtime.transfer != Some(before.transfer) {
        app.limits_wake.notify_one();
    }
    if !runtime.prefs.same_selection(&prefs_before) {
        Recompute::enqueue_all(&app).await?;
    }
    if runtime.prefs.launch != prefs_before.launch {
        let status = snapshot(&app).await;
        app.events.publish(EventKind::Status, &status);
    }
    Ok(Json(runtime))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mistarr_clients::PathMapping;

    #[test]
    fn a_queued_job_says_what_it_waits_for() {
        let row = |id, kind: &str, state| JobRow {
            id: JobId(id),
            kind: kind.into(),
            lane: "background".into(),
            payload: serde_json::json!({ "path": "/d/a.dat" }),
            state,
            progress: None,
            created_at: 0,
            updated_at: 0,
        };
        let open = [
            row(1, "dat_import", jobs::JobState::Running),
            row(2, "source_import", jobs::JobState::Queued),
        ];
        let free = GateState::default();
        assert_eq!(job_reason(&free, &open[0], &open), None);
        assert_eq!(
            job_reason(&free, &open[1], &open).as_deref(),
            Some("Waiting for the DAT import of a.dat to finish.")
        );
        let paused = GateState {
            corename: None,
            manual: Some(Override::Paused),
        };
        assert_eq!(
            job_reason(&paused, &open[1], &open).as_deref(),
            Some("Paused by the user")
        );
    }

    #[test]
    fn path_map_entries_need_a_remote_and_an_absolute_local() {
        let ok = |r: &str, l: &str| path_map_entry_ok(&PathMapping::new(r, l));
        assert!(ok("/downloads", "/media/fat/mistarr/staging"));
        assert!(ok("C:\\Downloads", "/media/fat/mistarr/staging"));
        assert!(ok("C:/Downloads", "/media/fat/mistarr/staging"));
        assert!(!ok("", "/media/fat/mistarr/staging"));
        assert!(!ok("  ", "/media/fat/mistarr/staging"));
        assert!(!ok("/downloads", "staging"));
        assert!(!ok("/downloads", ""));
    }
}
