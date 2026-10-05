//! `GET /imports` of `docs/API.md` "Downloads".

use std::sync::Arc;

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};

use super::{ApiError, ApiQuery, Paging};
use crate::app::AppState;
use crate::db::imports::{self, LogRow};
use crate::db::sql::Paged;

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/imports", get(list))
}

async fn list(
    State(app): State<Arc<AppState>>,
    ApiQuery(paging): ApiQuery<Paging>,
) -> Result<Json<Paged<LogRow>>, ApiError> {
    let page = paging.resolve();
    Ok(Json(app.db.read(move |c| imports::list(c, page)).await?))
}
