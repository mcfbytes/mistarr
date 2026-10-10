//! The System routes of `docs/API.md`.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use mistarr_clients::launch::Launcher;
use mistarr_clients::ClientKind;
use mistarr_core::PlatformId;
use serde::{Deserialize, Serialize};

use super::extract::{from_value, unknown_field};
use super::{ApiError, ApiJson, ApiQuery, OptionalJson, Paging};
use crate::app::AppState;
use crate::config::{dropped_flags, hideable_flags, unknown_prefs_fields, RuntimeSettings};
use crate::db::ids::JobId;
use crate::db::jobs::{self, JobRow};
use crate::db::platforms;
use crate::db::settings::{self, keys};
use crate::db::sql::Paged;
use crate::jobs::detect_client::{detect_and_store, ClientStatus};
use crate::jobs::scan::{is_arcade, ScanJob};
use crate::jobs::watch::gate::{GateState, Override};
use crate::jobs::Scheduler;
use crate::status::{hold_reason, snapshot, wizard_status, Status};

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
#[serde(default, deny_unknown_fields)]
struct ScanBody {
    platform_id: Option<PlatformId>,
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

/// `POST /system/scan`: 202 with the queued jobs.
async fn scan(
    State(app): State<Arc<AppState>>,
    OptionalJson(body): OptionalJson<ScanBody>,
) -> Result<(StatusCode, Json<ScanResponse>), ApiError> {
    let platform_id = match body.platform_id {
        Some(id) => Some(validate_platform(&app, id).await?),
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
    let queued = ScanResponse {
        job_id,
        arcade_job_id,
    };
    Ok((StatusCode::ACCEPTED, Json(queued)))
}

/// `POST /system/cores` answer.
#[derive(Debug, Serialize)]
struct CoresResponse {
    platforms: Vec<PlatformId>,
    arcade_job_id: Option<JobId>,
}

/// Detects installed cores again, for the wizard's detected-cores step, and
/// queues the arcade catalogue when there are MRA files to read; 202, since the
/// catalogue is the job it answers with.
async fn cores(
    State(app): State<Arc<AppState>>,
) -> Result<(StatusCode, Json<CoresResponse>), ApiError> {
    let platforms = crate::app::detect_cores(&app).await?;
    let arcade_job_id = crate::jobs::arcade::enqueue_if_relevant(&app).await?;
    let found = CoresResponse {
        platforms,
        arcade_job_id,
    };
    Ok((StatusCode::ACCEPTED, Json(found)))
}

/// Looks up `id` among the seeded platforms, for `POST /system/scan`.
///
/// # Errors
///
/// [`ApiError`] 404 when no such platform exists, 400 when it is disabled.
async fn validate_platform(app: &AppState, id: PlatformId) -> Result<PlatformId, ApiError> {
    let lookup = id.clone();
    let row = app
        .db
        .read(move |c| platforms::find(c, &lookup))
        .await?
        .ok_or_else(|| ApiError::no_such(&format!("platform `{}`", id.as_str())))?;
    if !row.enabled {
        return Err(ApiError::bad_request(format!(
            "The platform `{}` is disabled.",
            id.as_str()
        )));
    }
    Ok(id)
}

async fn status(State(app): State<Arc<AppState>>) -> Json<Status> {
    Json(snapshot(&app).await)
}

/// `GET /system/wizard`: which first-run steps are complete.
#[derive(Debug, Serialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "One flag per wizard step is the JSON shape."
)]
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
    ApiJson(body): ApiJson<StartBody>,
) -> Result<Json<Status>, ApiError> {
    let Ok(_starting) = app.client.start_lock.try_lock() else {
        return Err(ApiError::busy(
            "A download client is already being started.",
        ));
    };
    let current = app
        .db
        .read(|c| settings::get_json::<ClientStatus>(c, keys::CLIENT_DETECTED))
        .await?;
    if current.is_some_and(|c| c.usable()) {
        return Err(ApiError::conflict("A download client is already running."));
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
    crate::threads::run(crate::threads::label::LAUNCH, move || match priority {
        Some(p) => p.at_default(|| launcher.start(body.kind)),
        None => launcher.start(body.kind),
    })
    .await?
    .map_err(|e| ApiError::internal(e.to_string()))?;
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
    ApiQuery(paging): ApiQuery<Paging>,
) -> Result<Json<Paged<JobItem>>, ApiError> {
    let page = paging.resolve();
    let (
        Paged {
            items: mut rows,
            total,
        },
        open,
    ) = app
        .db
        .read(move |c| Ok((jobs::list_active(c, page)?, jobs::open_rows(c)?)))
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
    Ok(Json(Paged { items, total }))
}

/// Why an open job is not running: the gate's hold, else, while queued, what it waits for.
fn job_reason(gate: &GateState, row: &JobRow, open: &[JobRow]) -> Option<String> {
    hold_reason(gate, row.lane, row.state).or_else(|| {
        (row.state == jobs::JobState::Queued).then(|| crate::incoming::queued_reason(row, open))
    })
}

/// Finished jobs `/system/jobs/recent` lists.
const RECENT_JOBS: u32 = 10;

/// A `/system/jobs/recent` item: a finished job, or a folded run of them, with the
/// newest row's fields.
#[derive(Debug, Serialize)]
struct RecentItem {
    #[serde(flatten)]
    row: JobRow,
    reason: Option<String>,
    /// Rows in the run, 1 when it stands alone.
    count: u32,
    /// The run's oldest row's `updated_at`.
    first_updated_at: i64,
}

async fn recent_jobs(
    State(app): State<Arc<AppState>>,
) -> Result<Json<Paged<RecentItem>>, ApiError> {
    let runs = app
        .db
        .read(|c| jobs::recent_finished(c, RECENT_JOBS))
        .await?;
    let items = runs
        .into_iter()
        .map(|run| RecentItem {
            row: run.row,
            reason: None,
            count: run.count,
            first_updated_at: run.first_updated_at,
        })
        .collect();
    Ok(Json(Paged::all(items)))
}

async fn get_settings(State(app): State<Arc<AppState>>) -> Json<RuntimeSettings> {
    Json(app.config().runtime())
}

/// `PUT /system/settings`: replaces the sections the body carries, refusing a key no
/// field claims and a `prefs.hide` name that is not a flag that can be hidden.
async fn put_settings(
    State(app): State<Arc<AppState>>,
    ApiJson(body): ApiJson<serde_json::Value>,
) -> Result<Json<RuntimeSettings>, ApiError> {
    if let Some(name) = dropped_flags(&body).first() {
        let only = hideable_flags();
        let msg = format!("Only {only} can be hidden, not \"{name}\".");
        return Err(ApiError::bad_request(msg));
    }
    if let Some(path) = unknown_prefs_fields(&body).first() {
        return Err(unknown_field(path));
    }
    let patch = from_value(body)?;
    Ok(Json(app.update_settings(patch).await?.settings))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::JobKind;

    #[test]
    fn a_queued_job_says_what_it_waits_for() {
        let row = |id, kind, state| JobRow {
            id: JobId::new(id),
            kind,
            lane: crate::jobs::Lane::Background,
            payload: serde_json::json!({ "path": "/d/a.dat" }),
            state,
            progress: None,
            created_at: 0,
            updated_at: 0,
        };
        let open = [
            row(1, JobKind::DatImport, jobs::JobState::Running),
            row(2, JobKind::SourceImport, jobs::JobState::Queued),
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
}
