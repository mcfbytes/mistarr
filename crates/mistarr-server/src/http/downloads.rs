//! The Downloads routes of `docs/API.md`.

use std::sync::Arc;

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use super::{ApiError, ApiPath, ApiQuery, Paging};
use crate::app::AppState;
use crate::db::downloads::{self as rows, CancelOutcome, DownloadRow, DownloadState, RetryOutcome};
use crate::db::ids::DownloadId;
use crate::db::sql::Paged;
use crate::jobs::transfer;

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
    #[serde(flatten)]
    paging: Paging,
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
    ApiQuery(query): ApiQuery<ListQuery>,
) -> Result<Json<Paged<DownloadRow>>, ApiError> {
    let states = states(query.state.as_deref())?;
    let page = query.paging.resolve();
    let rows = app.db.read(move |c| rows::list(c, &states, page)).await?;
    Ok(Json(rows))
}

async fn load(app: &AppState, id: DownloadId) -> Result<DownloadRow, ApiError> {
    app.db
        .read(move |c| rows::get(c, id))
        .await?
        .ok_or_else(|| ApiError::no_such("download"))
}

async fn retry(
    State(app): State<Arc<AppState>>,
    ApiPath(id): ApiPath<DownloadId>,
) -> Result<Json<DownloadRow>, ApiError> {
    let outcome = app
        .db
        .write(move |c| rows::retry(c, id, crate::unix_now()))
        .await?;
    match outcome {
        RetryOutcome::Queued => {}
        RetryOutcome::Missing => return Err(ApiError::no_such("download")),
        RetryOutcome::NotFailed(state) => {
            return Err(ApiError::conflict(format!(
                "Only failed downloads can be retried; this one is {state}."
            )))
        }
        RetryOutcome::Busy => {
            return Err(ApiError::conflict(
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
    ApiPath(id): ApiPath<DownloadId>,
) -> Result<Json<DownloadRow>, ApiError> {
    let outcome = app
        .db
        .write(move |c| rows::cancel(c, id, crate::unix_now()))
        .await?;
    match outcome {
        CancelOutcome::Cancelled(c) => transfer::after_cancel(&app, &[c]).await,
        CancelOutcome::Missing => return Err(ApiError::no_such("download")),
        CancelOutcome::Final(state) => {
            return Err(ApiError::conflict(format!(
                "A download that is {state} cannot be cancelled."
            )))
        }
    }
    Ok(Json(load(&app, id).await?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    #[test]
    fn state_filters_parse() {
        assert!(states(None).expect("none").is_empty());
        assert_eq!(
            states(Some("queued, failed")).expect("two"),
            [DownloadState::Queued, DownloadState::Failed]
        );
        assert_eq!(
            states(Some("x")).map_err(|e| e.status()),
            Err(StatusCode::BAD_REQUEST)
        );
    }
}
