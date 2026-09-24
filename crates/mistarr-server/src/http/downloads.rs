//! The Downloads routes of `docs/API.md`.

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use super::{ApiError, Page, Paging};
use crate::app::AppState;
use crate::db::downloads::{
    self as rows, CancelOutcome, Cancelled, DownloadId, DownloadRow, DownloadState, RetryOutcome,
};
use crate::db::sources::SourceId;
use crate::jobs::transfer::{self, Deselect};
use crate::jobs::Scheduler;

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/downloads", get(list))
        .route("/downloads/{id}", axum::routing::delete(cancel))
        .route("/downloads/{id}/retry", post(retry))
}

/// `GET /downloads` query.
#[derive(Debug, Default, Deserialize)]
struct ListQuery {
    state: Option<String>,
    limit: Option<u32>,
    offset: Option<u32>,
}

/// Parses a comma list of state names; empty means every state.
fn states(text: Option<&str>) -> Result<Vec<DownloadState>, ApiError> {
    text.unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            DownloadState::parse(s)
                .ok_or_else(|| ApiError::bad_request(format!("{s:?} is not a download state.")))
        })
        .collect()
}

async fn list(
    State(app): State<Arc<AppState>>,
    query: Result<Query<ListQuery>, QueryRejection>,
) -> Result<Json<Page<DownloadRow>>, ApiError> {
    let Query(query) = query.map_err(|e| ApiError::bad_request(e.body_text()))?;
    let states = states(query.state.as_deref())?;
    let (limit, offset) = Paging {
        limit: query.limit,
        offset: query.offset,
    }
    .resolve();
    let (items, total) = app
        .db
        .read(move |c| rows::list(c, &states, limit, offset))
        .await?;
    Ok(Json(Page { items, total }))
}

fn download_id(id: Result<Path<i64>, PathRejection>) -> Result<DownloadId, ApiError> {
    id.map(|Path(id)| DownloadId(id))
        .map_err(|e| ApiError::bad_request(e.body_text()))
}

fn conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::CONFLICT, "conflict", message)
}

async fn load(app: &AppState, id: DownloadId) -> Result<DownloadRow, ApiError> {
    app.db
        .read(move |c| rows::get(c, id))
        .await?
        .ok_or_else(|| ApiError::not_found("No such download."))
}

async fn retry(
    State(app): State<Arc<AppState>>,
    id: Result<Path<i64>, PathRejection>,
) -> Result<Json<DownloadRow>, ApiError> {
    let id = download_id(id)?;
    let outcome = app
        .db
        .write(move |c| rows::retry(c, id, crate::unix_now()))
        .await?;
    match outcome {
        RetryOutcome::Queued => {}
        RetryOutcome::Missing => return Err(ApiError::not_found("No such download.")),
        RetryOutcome::NotFailed(state) => {
            return Err(conflict(format!(
                "Only failed downloads can be retried; this one is {state}."
            )))
        }
        RetryOutcome::Busy => {
            return Err(conflict(
                "Another download already fetches this file. Cancel it first.",
            ))
        }
    }
    let row = load(&app, id).await?;
    transfer::publish(&app, row.id, row.state, row.progress);
    transfer::kick(&app).await;
    Ok(Json(row))
}

async fn cancel(
    State(app): State<Arc<AppState>>,
    id: Result<Path<i64>, PathRejection>,
) -> Result<Json<DownloadRow>, ApiError> {
    let id = download_id(id)?;
    let outcome = app
        .db
        .write(move |c| rows::cancel(c, id, crate::unix_now()))
        .await?;
    match outcome {
        CancelOutcome::Cancelled(c) => after_cancel(&app, &[c]).await,
        CancelOutcome::Missing => return Err(ApiError::not_found("No such download.")),
        CancelOutcome::Final(state) => {
            return Err(conflict(format!(
                "A download that is {state} cannot be cancelled."
            )))
        }
    }
    Ok(Json(load(&app, id).await?))
}

/// Announces cancelled downloads and, for those the client had started,
/// queues a [`Deselect`] per source so the client stops fetching them.
pub(crate) async fn after_cancel(app: &Arc<AppState>, cancelled: &[Cancelled]) {
    let ids = cancelled.iter().map(|c| c.id).collect();
    if let Err(e) = transfer::publish_ids(app, ids).await {
        tracing::warn!(error = %e, "cannot announce cancelled downloads");
    }
    let started: BTreeSet<i64> = cancelled
        .iter()
        .filter(|c| c.started)
        .filter_map(|c| c.source_id.map(|s| s.0))
        .collect();
    for source in started {
        let job = Arc::new(Deselect {
            source_id: SourceId(source),
        });
        if let Err(e) = Scheduler::enqueue(app, job).await {
            tracing::warn!(error = %e, "cannot queue a deselect");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_filters_parse() {
        assert!(states(None).expect("none").is_empty());
        assert_eq!(
            states(Some("queued, failed")).expect("two"),
            [DownloadState::Queued, DownloadState::Failed]
        );
        assert_eq!(
            states(Some("x")).map_err(|e| e.status),
            Err(StatusCode::BAD_REQUEST)
        );
        assert_eq!(conflict("c").status, StatusCode::CONFLICT);
    }
}
