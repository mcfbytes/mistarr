//! `POST /fetch` and `DELETE /fetch/{token}` of `docs/API.md` "Fetching a URL".

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, post};
use axum::{Json, Router};
use mistarr_clients::fetch::{FetchError, FetchUrl};
use serde::{Deserialize, Serialize};

use super::sources::{magnet_file, place_source};
use super::ApiError;
use crate::app::AppState;
use crate::incoming::{IncomingFile, QUEUE_WAIT};
use crate::jobs::url_fetch::UrlFetch;
use crate::jobs::Scheduler;

/// Longest link accepted.
const MAX_LINK: usize = 8 * 1024;

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/fetch", post(start))
        .route("/fetch/{token}", delete(cancel))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FetchBody {
    url: String,
}

/// The answer to `POST /fetch`.
#[derive(Debug, Serialize)]
struct Started {
    /// The fetch's token for `DELETE /fetch/{token}`; `null` for a magnet.
    token: Option<u64>,
    /// Its `url_fetch` job, `null` for a magnet or while the writer is busy.
    job_id: Option<crate::db::jobs::JobId>,
    /// `sources` for a magnet, placed at once; `null` for a fetch.
    target: Option<&'static str>,
    /// The placed `.magnet` file as `/sources/incoming` lists it; `null` for a fetch.
    file: Option<IncomingFile>,
}

/// `POST /fetch`: places a magnet link at once, or queues one fetch of an http(s) URL.
/// The URL is never stored, logged or echoed in an error.
async fn start(
    State(app): State<Arc<AppState>>,
    body: Bytes,
) -> Result<(StatusCode, Json<Started>), ApiError> {
    let req: FetchBody = serde_json::from_slice(&body)
        .map_err(|_| ApiError::bad_request("Send { \"url\": \"...\" }."))?;
    let link = req.url.trim();
    if link.is_empty() {
        return Err(ApiError::bad_request("Enter a link."));
    }
    if link.len() > MAX_LINK {
        return Err(ApiError::bad_request("The link is too long."));
    }
    if link
        .get(..7)
        .is_some_and(|s| s.eq_ignore_ascii_case("magnet:"))
    {
        let file = place_source(&app, magnet_file(link)?).await?;
        return Ok((
            StatusCode::ACCEPTED,
            Json(Started {
                token: None,
                job_id: None,
                target: Some("sources"),
                file: Some(file),
            }),
        ));
    }
    let url = FetchUrl::parse(link).map_err(|e| match e {
        FetchError::Url(why) => ApiError::bad_request(why),
        _ => ApiError::bad_request("This is not a valid link."),
    })?;
    let job = UrlFetch::new(&app, url);
    let token = job.token();
    let job_id = Scheduler::enqueue_within(&app, Arc::new(job), QUEUE_WAIT, ()).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(Started {
            token: Some(token),
            job_id,
            target: None,
            file: None,
        }),
    ))
}

/// `DELETE /fetch/{token}`: asks a queued or running fetch to stop; 404 when none is open.
async fn cancel(
    State(app): State<Arc<AppState>>,
    token: Result<Path<u64>, PathRejection>,
) -> Result<StatusCode, ApiError> {
    let Path(token) = token.map_err(|e| ApiError::bad_request(e.body_text()))?;
    if app.fetches.cancel(token) {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("No such fetch is open."))
    }
}
