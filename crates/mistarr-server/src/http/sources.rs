//! The Sources routes of `docs/API.md`.

use std::sync::Arc;

use axum::extract::multipart::Field;
use axum::extract::{DefaultBodyLimit, FromRequest, Multipart, Request, State};
use axum::http::{header, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use mistarr_clients::ClientError;
use mistarr_core::magnet;
use mistarr_core::PlatformId;
use mistarr_sources::torrent;
use serde::{Deserialize, Deserializer, Serialize};

use super::extract::with_file;
use super::{ApiError, ApiJson, ApiPath, ApiQuery, Paging};
use crate::app::AppState;
use crate::db::ids::{JobId, SourceId};
use crate::db::platforms;
use crate::db::sources::{self as rows, SourceReason, SourceRow, SourceState};
use crate::db::sql::Paged;
use crate::db::views::source_detail::{
    self as detail, FileFilter, FileQuery, FileRow, Preview, SourceDetail,
};
use crate::incoming::place::{file_name, place_source, SourceFile};
use crate::incoming::IncomingFile;
use crate::jobs::bind_source::{BindSource, Choice};
use crate::jobs::source_import::publish_changed;
use crate::jobs::Scheduler;

/// Largest accepted upload; set torrents with many files run to a few MiB.
const UPLOAD_LIMIT: usize = 16 * 1024 * 1024;

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/sources", get(list))
        .route("/sources/incoming", get(incoming))
        .route(
            "/sources/upload",
            post(upload).layer(DefaultBodyLimit::max(UPLOAD_LIMIT)),
        )
        .route("/sources/{id}", get(show).put(update).delete(remove))
        .route("/sources/{id}/files", get(files))
        .route("/sources/{id}/preview", get(preview))
}

/// A source as the API returns it: the row with its reason worded.
#[derive(Debug, Serialize)]
struct SourceItem {
    #[serde(flatten)]
    row: SourceRow,
    reason: Option<String>,
}

impl From<SourceRow> for SourceItem {
    fn from(row: SourceRow) -> Self {
        let reason = row.reason.as_ref().map(reason_text);
        Self { row, reason }
    }
}

/// `GET /sources/{id}`: the item with how its files classify.
#[derive(Debug, Serialize)]
struct DetailItem {
    #[serde(flatten)]
    detail: SourceDetail,
    reason: Option<String>,
}

/// The sentence the API shows for `reason`.
fn reason_text(reason: &SourceReason) -> String {
    let name = |p: &PlatformId| {
        mistarr_mister::platforms::by_id(&p.0).map_or_else(|| p.0.clone(), |t| t.name.to_owned())
    };
    match reason {
        SourceReason::NoClient => {
            "No download client found. The file list is read once one is detected.".to_owned()
        }
        SourceReason::WaitingMetadata => {
            "Waiting for the download client to read the file list.".to_owned()
        }
        SourceReason::BadInfohash => {
            "The source's infohash is not valid. Remove the source and add it again.".to_owned()
        }
        SourceReason::ClientRefused { error } => {
            format!("The download client did not accept the source: {error}.")
        }
        SourceReason::NoMatch { percent, suggested } => {
            let base =
                format!("No platform matched {percent}% of the files. Pick a platform to bind it.");
            match suggested {
                Some(p) => format!("{base} Its names suggest {}.", name(p)),
                None => base,
            }
        }
        SourceReason::AwaitingDat { platform } => format!(
            "Looks like {}. No DAT for it is loaded yet; it binds once one loads.",
            name(platform)
        ),
        SourceReason::Ignored => {
            "Marked as not a game set. It is not bound automatically.".to_owned()
        }
    }
}

async fn list(
    State(app): State<Arc<AppState>>,
    ApiQuery(paging): ApiQuery<Paging>,
) -> Result<Json<Paged<SourceItem>>, ApiError> {
    let page = paging.resolve();
    let rows = app.db.read(move |c| rows::list(c, page)).await?;
    Ok(Json(Paged {
        items: rows.items.into_iter().map(SourceItem::from).collect(),
        total: rows.total,
    }))
}

/// `GET /sources/incoming`: files in `sources/` not loaded yet, and rejected ones.
async fn incoming(
    State(app): State<Arc<AppState>>,
    ApiQuery(paging): ApiQuery<Paging>,
) -> Result<Json<Paged<IncomingFile>>, ApiError> {
    let dir = app.config().paths.sources();
    let all = crate::incoming::list(&app, &dir, crate::jobs::JobKind::SourceImport).await?;
    Ok(Json(paging.resolve().slice(all)))
}

async fn load(app: &AppState, id: SourceId) -> Result<SourceRow, ApiError> {
    app.db
        .read(move |c| rows::get(c, id))
        .await?
        .ok_or_else(|| ApiError::no_such("source"))
}

/// `GET /sources/{id}/files` query: paging, a filter and a search.
#[derive(Debug, Default, Deserialize)]
struct FilesQuery {
    filter: Option<String>,
    q: Option<String>,
    #[serde(flatten)]
    paging: Paging,
}

async fn files(
    State(app): State<Arc<AppState>>,
    ApiPath(id): ApiPath<SourceId>,
    ApiQuery(query): ApiQuery<FilesQuery>,
) -> Result<Json<Paged<FileRow>>, ApiError> {
    let filter = query
        .filter
        .as_deref()
        .filter(|f| !f.is_empty())
        .map(|f| {
            FileFilter::parse(f)
                .ok_or_else(|| ApiError::bad_request("The filter is matched, unmatched or wanted."))
        })
        .transpose()?;
    let page = query.paging.resolve();
    let query = FileQuery { filter, q: query.q };
    load(&app, id).await?;
    let files = app
        .db
        .read(move |c| detail::files(c, id, &query, page))
        .await?;
    Ok(Json(files))
}

/// `GET /sources/{id}`: the source with how its files classify.
async fn show(
    State(app): State<Arc<AppState>>,
    ApiPath(id): ApiPath<SourceId>,
) -> Result<Json<DetailItem>, ApiError> {
    let detail = app
        .db
        .read(move |c| detail::detail(c, id))
        .await?
        .ok_or_else(|| ApiError::no_such("source"))?;
    let reason = detail.source.reason.as_ref().map(reason_text);
    Ok(Json(DetailItem { detail, reason }))
}

/// `GET /sources/{id}/preview`: how many files each platform with a DAT would match.
async fn preview(
    State(app): State<Arc<AppState>>,
    ApiPath(id): ApiPath<SourceId>,
) -> Result<Json<Preview>, ApiError> {
    let total = load(&app, id).await?.file_count;
    let step = detail::sample_step(total, detail::PREVIEW_SAMPLE);
    let mut tally = detail::PreviewTally::default();
    let mut after = None;
    // The database has one reader; each chunk is its own read so other pages get it in between.
    loop {
        let chunk = app
            .db
            .read(move |c| detail::preview_chunk(c, id, after, step, detail::PREVIEW_CHUNK))
            .await?;
        tally.add(&chunk);
        match chunk.last {
            Some(last) if chunk.files == u64::from(detail::PREVIEW_CHUNK) => after = Some(last),
            _ => break,
        }
    }
    let with_dat = app.db.read(detail::platforms_with_dat).await?;
    Ok(Json(tally.finish(total, with_dat)))
}

/// `PUT /sources/{id}` body. `platform_id: null` marks the source as not a game
/// set; `binding: "automatic"` hands it back to automatic binding; `state` is
/// `disabled`, or `enabled` to return to the state the source would otherwise have.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(clippy::option_option)] // Absent keeps the binding; null unbinds.
struct Update {
    #[serde(default, deserialize_with = "present")]
    platform_id: Option<Option<PlatformId>>,
    binding: Option<String>,
    seed_policy: Option<String>,
    state: Option<String>,
}

/// Distinguishes a `null` field from an absent one.
#[allow(clippy::option_option)] // The shape `Update::platform_id` needs.
fn present<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<PlatformId>>, D::Error> {
    Option::<PlatformId>::deserialize(d).map(Some)
}

/// The `PUT /sources/{id}` answer: the item, and the binding job it queued.
#[derive(Debug, Serialize)]
struct Updated {
    #[serde(flatten)]
    source: SourceItem,
    job_id: Option<JobId>,
}

/// Returns a disabled source to `bound`, `unbound` or `resolving` as its files and platform say.
fn enable_source(conn: &rusqlite::Connection, id: SourceId, threshold: f32) -> crate::Result<()> {
    let now = rows::get(conn, id)?;
    if let Some(r) = now.filter(|r| r.state == SourceState::Disabled) {
        let (state, reason) = if r.platform_id.is_some() {
            (SourceState::Bound, None)
        } else if r.file_count == 0 {
            (SourceState::Resolving, None)
        } else {
            let why = SourceReason::no_match(threshold, None);
            (SourceState::Unbound, Some(why))
        };
        rows::set_state(conn, id, state, reason.as_ref())?;
    }
    Ok(())
}

/// The binding `req` asks for, if any.
fn choice(req: &Update) -> Result<Option<Choice>, ApiError> {
    match (&req.platform_id, req.binding.as_deref()) {
        (Some(_), Some(_)) => Err(ApiError::bad_request(
            "Send platform_id or binding, not both.",
        )),
        (None, Some("automatic")) => Ok(Some(Choice::Automatic)),
        (None, Some(_)) => Err(ApiError::bad_request("The binding is automatic.")),
        (Some(Some(p)), None) => Ok(Some(Choice::Platform(p.clone()))),
        (Some(None), None) => Ok(Some(Choice::Ignore)),
        (None, None) => Ok(None),
    }
}

/// `PUT /sources/{id}`: 202 with the binding job when one is queued, else 200.
async fn update(
    State(app): State<Arc<AppState>>,
    ApiPath(id): ApiPath<SourceId>,
    ApiJson(req): ApiJson<Update>,
) -> Result<(StatusCode, Json<Updated>), ApiError> {
    let row = load(&app, id).await?;
    let seed = req
        .seed_policy
        .as_deref()
        .map(|s| {
            rows::seed_from_text(s).ok_or_else(|| {
                ApiError::bad_request("The seed policy is none, client or ratio:N with N above 0.")
            })
        })
        .transpose()?;
    let enable = match req.state.as_deref() {
        None => None,
        Some("disabled") => Some(false),
        Some("enabled") => Some(true),
        Some(_) => return Err(ApiError::bad_request("The state is disabled or enabled.")),
    };
    let choice = choice(&req)?;
    if let Some(Choice::Platform(p)) = &choice {
        let p = p.clone();
        if app
            .db
            .read(move |c| platforms::find(c, &p))
            .await?
            .is_none()
        {
            return Err(ApiError::bad_request("No such platform."));
        }
    }
    if choice.is_some() && row.file_count == 0 {
        return Err(ApiError::bad_request(
            "This source has no file list yet, so it cannot be bound.",
        ));
    }
    let threshold = app.config().sources.bind_threshold;
    let stored_seed = seed.clone();
    let requested = choice.clone();
    let updated = app
        .db
        .write_tx(move |tx| {
            if let Some(choice) = &requested {
                rows::request_binding(tx, id, choice)?;
            }
            if let Some(s) = &stored_seed {
                rows::set_seed_policy(tx, id, s)?;
            }
            match enable {
                Some(false) => rows::set_state(tx, id, SourceState::Disabled, None)?,
                Some(true) => enable_source(tx, id, threshold)?,
                None => {}
            }
            let row = rows::get(tx, id)?;
            Ok(row)
        })
        .await?
        .ok_or_else(|| ApiError::no_such("source"))?;
    if let (Some(seed), Some(cid)) = (seed, updated.client_id) {
        let applied = match app.client.get() {
            Some(client) => match client.set_seed_policy(&cid, seed).await {
                Ok(()) => true,
                Err(e) => {
                    tracing::warn!(source = %id, error = %e, "cannot apply the seed policy in the client");
                    false
                }
            },
            None => false,
        };
        // A frozen, missing or refusing client gets every policy again later.
        if !applied {
            crate::jobs::core_limits::defer(&app, crate::db::deferred::Op::Seed).await;
        }
    }
    // The job applies whichever request is newest when it runs, so sharing a queued one is safe.
    let job_id = match choice {
        Some(_) => {
            let job = BindSource {
                source_id: id,
                source_name: updated.display_name.clone(),
            };
            Some(Scheduler::enqueue(&app, Arc::new(job)).await?)
        }
        None => None,
    };
    publish_changed(&app, &updated);
    let status = if job_id.is_some() {
        StatusCode::ACCEPTED
    } else {
        StatusCode::OK
    };
    let body = Updated {
        source: updated.into(),
        job_id,
    };
    Ok((status, Json(body)))
}

async fn remove(
    State(app): State<Arc<AppState>>,
    ApiPath(id): ApiPath<SourceId>,
) -> Result<StatusCode, ApiError> {
    let row = load(&app, id).await?;
    if app
        .db
        .read(move |c| rows::open_download_count(c, id))
        .await?
        > 0
    {
        return Err(ApiError::bad_request(
            "This source has downloads that are queued, transferring, checking or importing. \
             Cancel them or let them finish before removing it.",
        ));
    }
    if row.client_id.is_some() && app.client.frozen() {
        return Err(ApiError::conflict(
            "The download client is paused while a core runs. Remove the source at the menu.",
        ));
    }
    if let (Some(cid), Some(client)) = (&row.client_id, app.client.get()) {
        match client.remove(cid, false).await {
            Ok(()) | Err(ClientError::NotFound) => {}
            Err(e) => {
                return Err(ApiError::unavailable(format!(
                    "The download client did not remove the source: {e}."
                )))
            }
        }
    }
    app.db.write(move |c| rows::delete(c, id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MagnetBody {
    magnet: String,
}

/// `POST /sources/upload`: one `.torrent` file as multipart form data, or a magnet link
/// as `{ magnet }`.
async fn upload(
    State(app): State<Arc<AppState>>,
    req: Request,
) -> Result<(StatusCode, Json<IncomingFile>), ApiError> {
    let multipart = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|t| t.starts_with("multipart/form-data"));
    let file = if multipart {
        let form = Multipart::from_request(req, &app)
            .await
            .map_err(|e| ApiError::bad_request(e.body_text()))?;
        torrent_file(form).await?
    } else {
        let ApiJson(body) = ApiJson::<MagnetBody>::from_request(req, &app).await?;
        magnet_file(&body.magnet)?
    };
    let placed = place_source(&app, file).await?;
    Ok((StatusCode::ACCEPTED, Json(placed)))
}

/// The first file of `form`, which must be a `.torrent`, read whole, since it is parsed whole.
async fn torrent_file(mut form: Multipart) -> Result<SourceFile, ApiError> {
    let none = |_: &str| false;
    let (name, data) = with_file(&mut form, none, async |name: String, field: Field<'_>| {
        let data = field.bytes().await;
        data.map(|d| (name, d))
            .map_err(|e| ApiError::bad_request(e.body_text()))
    })
    .await?;
    // Parsing walks every file entry, so it stays off the async workers.
    crate::threads::run(crate::threads::label::SOURCE_FILE, move || {
        let meta = torrent::parse_torrent(&data).map_err(|e| {
            ApiError::bad_request(format!("This is not a valid .torrent file: {e}"))
        })?;
        Ok(SourceFile {
            name: file_name(&name, "upload", "torrent"),
            bytes: Vec::from(data),
            infohash: meta.infohash,
            is_torrent: true,
        })
    })
    .await?
}

/// A magnet link as the `.magnet` file an upload places.
///
/// # Errors
///
/// A 400 when `uri` is not a magnet link with a v1 infohash.
pub(super) fn magnet_file(uri: &str) -> Result<SourceFile, ApiError> {
    let uri = uri.trim();
    let parsed = magnet::parse_magnet(uri)
        .map_err(|e| ApiError::bad_request(format!("This is not a valid magnet link: {e}")))?;
    let hex = parsed.infohash.to_string();
    let stem = parsed.display_name.unwrap_or_else(|| hex.clone());
    Ok(SourceFile {
        name: file_name(&stem, &hex, "magnet"),
        bytes: format!("{uri}\n").into_bytes(),
        infohash: parsed.infohash,
        is_torrent: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasons_are_worded_with_their_parameters() {
        let words = |r: SourceReason| reason_text(&r);
        assert_eq!(
            words(SourceReason::no_match(0.6, None)),
            "No platform matched 60% of the files. Pick a platform to bind it."
        );
        assert!(
            words(SourceReason::no_match(0.6, Some(PlatformId("gb".into()))))
                .ends_with(" Its names suggest Game Boy.")
        );
        assert_eq!(
            words(SourceReason::AwaitingDat {
                platform: PlatformId("gb".into())
            }),
            "Looks like Game Boy. No DAT for it is loaded yet; it binds once one loads."
        );
        let refused = SourceReason::ClientRefused {
            error: "refused".into(),
        };
        assert!(words(refused).ends_with("source: refused."));
        for r in [
            SourceReason::NoClient,
            SourceReason::WaitingMetadata,
            SourceReason::BadInfohash,
            SourceReason::Ignored,
        ] {
            assert!(words(r).ends_with('.'));
        }
        let mut row = SourceItem::from(sample_row());
        assert_eq!(row.reason, None);
        row = SourceItem::from(SourceRow {
            reason: Some(SourceReason::Ignored),
            ..sample_row()
        });
        let json = serde_json::to_value(&row).expect("json");
        assert_eq!(
            json["reason"],
            "Marked as not a game set. It is not bound automatically."
        );
    }

    fn sample_row() -> SourceRow {
        SourceRow {
            id: SourceId(1),
            infohash: "0a".repeat(20),
            display_name: "Synthetic Set".into(),
            origin_file: "set.torrent".into(),
            platform_id: None,
            bind_score: None,
            state: SourceState::Unbound,
            reason: None,
            seed_policy: "none".into(),
            file_count: 0,
            matched_count: 0,
            total_size: 0,
            client_id: None,
            added_at: 0,
            suggested_platform_id: None,
            user_binding: false,
            pending_binding: None,
        }
    }
}
