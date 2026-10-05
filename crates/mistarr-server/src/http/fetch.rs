//! `POST /fetch` and `DELETE /fetch/{token}` of `docs/API.md` "Fetching a URL".

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{delete, post};
use axum::{Json, Router};
use mistarr_clients::fetch::{FetchError, FetchUrl};
use serde::{Deserialize, Serialize};

use super::sources::magnet_file;
use super::{ApiError, ApiPath};
use crate::app::AppState;
use crate::incoming::place::place_source;
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
    job_id: Option<crate::db::ids::JobId>,
    /// `sources` for a magnet, placed at once; `null` for a fetch.
    target: Option<&'static str>,
    /// The placed `.magnet` file as `/sources/incoming` lists it; `null` for a fetch.
    file: Option<IncomingFile>,
}

/// `POST /fetch`: places a magnet link at once, or queues one fetch of an http(s) URL.
/// The URL is never stored, logged or echoed in an error, so a body that does not
/// parse gets a fixed message rather than the parser's.
async fn start(
    State(app): State<Arc<AppState>>,
    body: axum::body::Bytes,
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
    let (token, flag) = (job.token(), job.cancel_flag());
    let job_id = Scheduler::enqueue_within(&app, Arc::new(job), QUEUE_WAIT, ()).await?;
    app.fetches.register(token, &flag);
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
    ApiPath(token): ApiPath<u64>,
) -> Result<StatusCode, ApiError> {
    if app.fetches.cancel(token) {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::no_such("fetch is open"))
    }
}
