//! The Platforms routes of `docs/API.md`.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use mistarr_core::PlatformId;
use serde::{Deserialize, Serialize};

use super::{ApiError, ApiJson, ApiPath, ApiQuery, Paging};
use crate::app::AppState;
use crate::db::dats;
use crate::db::files::{self, UnidentifiedFile};
use crate::db::ids::{DatVersionId, JobId};
use crate::db::platforms::{self, PlatformRow};
use crate::db::sql::Paged;
use crate::db::titles;
use crate::db::titles::browse::Counts;
use crate::jobs::dat_import::DatImport;
use crate::jobs::Scheduler;
use mistarr_sources::intake::LOADED_DIR;

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/platforms", get(list))
        .route("/platforms/{id}", put(update))
        .route("/platforms/{id}/dat", post(bind))
        .route("/platforms/{id}/unidentified", get(unidentified))
}

/// `GET /platforms/{id}/unidentified`: the files not identified, with why, by path.
async fn unidentified(
    State(app): State<Arc<AppState>>,
    ApiPath(id): ApiPath<PlatformId>,
    ApiQuery(paging): ApiQuery<Paging>,
) -> Result<Json<Paged<UnidentifiedFile>>, ApiError> {
    let page = paging.resolve();
    let found = app
        .db
        .read(move |c| {
            if platforms::find(c, &id)?.is_none() {
                return Ok(None);
            }
            files::unidentified(c, &id, page).map(Some)
        })
        .await?;
    Ok(Json(found.ok_or_else(|| ApiError::no_such("platform"))?))
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
    ApiQuery(paging): ApiQuery<Paging>,
) -> Result<Json<Paged<PlatformOut>>, ApiError> {
    let hide = app.config().prefs.hidden_names();
    let (rows, mut counts) = app
        .db
        .read(move |c| Ok((platforms::list(c)?, titles::browse::counts(c, &hide)?)))
        .await?;
    let Paged { items, total } = paging.resolve().slice(rows);
    let items = items
        .into_iter()
        .map(|row| {
            let counts = counts.remove(&row.id.0).unwrap_or_default();
            PlatformOut { row, counts }
        })
        .collect();
    Ok(Json(Paged { items, total }))
}

/// `PUT /platforms/{id}` body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateBody {
    enabled: bool,
}

async fn update(
    State(app): State<Arc<AppState>>,
    ApiPath(id): ApiPath<PlatformId>,
    ApiJson(UpdateBody { enabled }): ApiJson<UpdateBody>,
) -> Result<Json<PlatformOut>, ApiError> {
    let hide = app.config().prefs.hidden_names();
    let found = app
        .db
        .write(move |c| {
            if !platforms::set_enabled(c, &id, enabled)? {
                return Ok(None);
            }
            let counts = titles::browse::counts(c, &hide)?
                .remove(&id.0)
                .unwrap_or_default();
            Ok(platforms::find(c, &id)?.map(|row| PlatformOut { row, counts }))
        })
        .await?;
    found.map(Json).ok_or_else(|| ApiError::no_such("platform"))
}

/// `POST /platforms/{id}/dat` body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BindBody {
    dat_version_id: DatVersionId,
}

/// The answer to a bind: the import job that loads the titles.
#[derive(Debug, Serialize)]
#[expect(clippy::struct_field_names, reason = "The JSON field names.")]
struct Binding {
    dat_version_id: DatVersionId,
    platform_id: PlatformId,
    job_id: JobId,
}

async fn bind(
    State(app): State<Arc<AppState>>,
    ApiPath(platform): ApiPath<PlatformId>,
    ApiJson(BindBody { dat_version_id }): ApiJson<BindBody>,
) -> Result<(StatusCode, Json<Binding>), ApiError> {
    let lookup = platform.clone();
    let (known, row) = app
        .db
        .read(move |c| {
            Ok((
                platforms::find(c, &lookup)?.is_some(),
                dats::get(c, dat_version_id)?,
            ))
        })
        .await?;
    if !known {
        return Err(ApiError::no_such("platform"));
    }
    let row = row.ok_or_else(|| ApiError::no_such("DAT version"))?;
    if row.platform_id.is_some() {
        return Err(ApiError::bad_request("That DAT version is already bound."));
    }
    if row.retired {
        return Err(ApiError::bad_request("That DAT version is retired."));
    }
    let loaded = app.config().paths.dats().join(LOADED_DIR);
    let job = DatImport::bind(&row, &platform.0, &loaded);
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
