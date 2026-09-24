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
use crate::db::settings::{self, keys};
use crate::db::system::wizard_counts;
use crate::jobs::detect_client::DetectClient;
use crate::jobs::gate::Override;
use crate::jobs::scan::ScanJob;
use crate::jobs::Scheduler;
use crate::status::{snapshot, Status};

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/system/status", get(status))
        .route("/system/wizard", get(wizard))
        .route("/system/scan", post(scan))
        .route("/system/pause", post(pause))
        .route("/system/resume", post(resume))
        .route("/system/jobs", get(list_jobs))
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
    job_id: JobId,
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
    let job = ScanJob {
        platform_id: body.platform_id.map(PlatformId),
    };
    let job_id = Scheduler::enqueue(&app, Arc::new(job)).await?;
    Ok(Json(ScanResponse { job_id }))
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
    let counts = app.db.read(wizard_counts).await?;
    let client = app
        .db
        .read(|c| {
            settings::get_json::<crate::jobs::detect_client::ClientStatus>(c, keys::CLIENT_DETECTED)
        })
        .await?;
    let dats = counts.dat_versions > 0;
    Ok(Json(Wizard {
        paths: app.config().paths.games.is_dir(),
        dats,
        client: client.is_some_and(|c| c.kind.is_some()),
        sources: counts.sources > 0,
        open_on_start: !dats,
    }))
}

async fn pause(State(app): State<Arc<AppState>>) -> Json<Status> {
    app.gate.set_override(Some(Override::Paused));
    Json(snapshot(&app).await)
}

async fn resume(State(app): State<Arc<AppState>>) -> Json<Status> {
    app.gate.set_override(Some(Override::Running));
    Json(snapshot(&app).await)
}

async fn list_jobs(
    State(app): State<Arc<AppState>>,
    paging: Result<Query<Paging>, QueryRejection>,
) -> Result<Json<Page<JobRow>>, ApiError> {
    let Query(paging) = paging.map_err(|e| ApiError::bad_request(e.body_text()))?;
    let (limit, offset) = paging.resolve();
    let (items, total) = app
        .db
        .read(move |c| jobs::list_active(c, limit, offset))
        .await?;
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
    let (runtime, client_changed) = app.update_settings(&patch).await?;
    if client_changed {
        Scheduler::enqueue(&app, Arc::new(DetectClient)).await?;
    }
    Ok(Json(runtime))
}
