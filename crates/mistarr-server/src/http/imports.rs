//! `GET /imports` of `docs/API.md` "Downloads".

use std::sync::Arc;

use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};

use super::{ApiError, Page, Paging};
use crate::app::AppState;
use crate::db::imports::{self, LogRow};

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/imports", get(list))
}

async fn list(
    State(app): State<Arc<AppState>>,
    paging: Result<Query<Paging>, QueryRejection>,
) -> Result<Json<Page<LogRow>>, ApiError> {
    let Query(paging) = paging.map_err(|e| ApiError::bad_request(e.body_text()))?;
    let (limit, offset) = paging.resolve();
    let (items, total) = app
        .db
        .read(move |c| imports::list(c, limit, offset))
        .await?;
    Ok(Json(Page { items, total }))
}
