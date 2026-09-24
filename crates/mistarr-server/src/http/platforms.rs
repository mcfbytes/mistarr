//! The Platforms routes of `docs/API.md`.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use super::{ApiError, Page, Paging};
use crate::app::AppState;
use crate::db::dats::{self, DatVersionId};
use crate::db::jobs::JobId;
use crate::db::platforms::{self, PlatformRow};
use crate::db::titles::{self, Counts};
use crate::jobs::dat_import::{DatImport, LOADED_DIR};
use crate::jobs::Scheduler;

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/platforms", get(list))
        .route("/platforms/{id}", put(update))
        .route("/platforms/{id}/dat", post(bind))
}

/// A platform with its catalog counts.
#[derive(Debug, Serialize)]
struct PlatformOut {
    #[serde(flatten)]
    row: PlatformRow,
    counts: Counts,
}

async fn list(
    State(app): State<Arc<AppState>>,
    paging: Result<Query<Paging>, QueryRejection>,
) -> Result<Json<Page<PlatformOut>>, ApiError> {
    let Query(paging) = paging.map_err(|e| ApiError::bad_request(e.body_text()))?;
    let (limit, offset) = paging.resolve();
    let hide = app.config().prefs.hide;
    let (rows, mut counts) = app
        .db
        .read(move |c| Ok((platforms::list(c)?, titles::counts(c, &hide)?)))
        .await?;
    let total = rows.len() as u64;
    let items = rows
        .into_iter()
        .skip(offset as usize)
        .take(limit as usize)
        .map(|row| {
            let counts = counts.remove(&row.id.0).unwrap_or_default();
            PlatformOut { row, counts }
        })
        .collect();
    Ok(Json(Page { items, total }))
}

fn body<T: for<'de> Deserialize<'de>>(bytes: &Bytes) -> Result<T, ApiError> {
    serde_json::from_slice(bytes).map_err(|e| ApiError::bad_request(e.to_string()))
}

fn path_id(id: Result<Path<String>, PathRejection>) -> Result<String, ApiError> {
    id.map(|Path(id)| id)
        .map_err(|e| ApiError::bad_request(e.body_text()))
}

/// `PUT /platforms/{id}` body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateBody {
    enabled: bool,
}

async fn update(
    State(app): State<Arc<AppState>>,
    id: Result<Path<String>, PathRejection>,
    bytes: Bytes,
) -> Result<Json<PlatformOut>, ApiError> {
    let id = path_id(id)?;
    let UpdateBody { enabled } = body(&bytes)?;
    let hide = app.config().prefs.hide;
    let found = app
        .db
        .write(move |c| {
            if !platforms::set_enabled(c, &id, enabled)? {
                return Ok(None);
            }
            let counts = titles::counts(c, &hide)?.remove(&id).unwrap_or_default();
            Ok(platforms::get(c, &id)?.map(|row| PlatformOut { row, counts }))
        })
        .await?;
    found
        .map(Json)
        .ok_or_else(|| ApiError::not_found("no such platform"))
}

/// `POST /platforms/{id}/dat` body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BindBody {
    dat_version_id: DatVersionId,
}

/// The answer to a bind: the import job that loads the titles.
#[derive(Debug, Serialize)]
#[allow(clippy::struct_field_names)] // The JSON field names.
struct Binding {
    dat_version_id: DatVersionId,
    platform_id: String,
    job_id: JobId,
}

async fn bind(
    State(app): State<Arc<AppState>>,
    id: Result<Path<String>, PathRejection>,
    bytes: Bytes,
) -> Result<(StatusCode, Json<Binding>), ApiError> {
    let platform = path_id(id)?;
    let BindBody { dat_version_id } = body(&bytes)?;
    let lookup = platform.clone();
    let (known, row) = app
        .db
        .read(move |c| {
            Ok((
                platforms::get(c, &lookup)?.is_some(),
                dats::get(c, dat_version_id)?,
            ))
        })
        .await?;
    if !known {
        return Err(ApiError::not_found("no such platform"));
    }
    let row = row.ok_or_else(|| ApiError::not_found("no such DAT version"))?;
    if row.platform_id.is_some() {
        return Err(ApiError::bad_request("that DAT version is already bound"));
    }
    if row.retired {
        return Err(ApiError::bad_request("that DAT version is retired"));
    }
    let loaded = app.config().paths.dats().join(LOADED_DIR);
    let job = DatImport::bind(&row, &platform, &loaded);
    let job_id = Scheduler::enqueue(&app, Arc::new(job)).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(Binding {
            dat_version_id,
            platform_id: platform,
            job_id,
        }),
    ))
}
