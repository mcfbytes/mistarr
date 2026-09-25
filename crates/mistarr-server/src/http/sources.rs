//! The Sources routes of `docs/API.md`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{DefaultBodyLimit, Path as UrlPath, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use mistarr_clients::{ClientError, ClientTorrentId, InfoHash};
use mistarr_core::PlatformId;
use mistarr_sources::{magnet, torrent};
use serde::{Deserialize, Deserializer, Serialize};

use super::{ApiError, Page, Paging};
use crate::app::AppState;
use crate::db::jobs::JobId;
use crate::db::source_detail::{
    self as detail, FileFilter, FileQuery, FileRow, Preview, SourceDetail,
};
use crate::db::sources::{self as rows, SourceId, SourceRow, SourceState};
use crate::jobs::bind_source::{BindSource, Choice};
use crate::jobs::source_import::{self, publish_changed, SourceImport, DUPLICATE};
use crate::jobs::Scheduler;

/// Largest accepted upload; set torrents with many files run to a few MiB.
const UPLOAD_LIMIT: usize = 16 * 1024 * 1024;

/// Names tried per upload: `name`, then `name (1)` onwards.
const PLACE_ATTEMPTS: u32 = 100;

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

async fn list(
    State(app): State<Arc<AppState>>,
    paging: Result<Query<Paging>, QueryRejection>,
) -> Result<Json<Page<SourceRow>>, ApiError> {
    let Query(paging) = paging.map_err(|e| ApiError::bad_request(e.body_text()))?;
    let (limit, offset) = paging.resolve();
    let (items, total) = app.db.read(move |c| rows::list(c, limit, offset)).await?;
    Ok(Json(Page { items, total }))
}

/// `GET /sources/incoming`: files in `sources/` not loaded yet, and rejected ones.
async fn incoming(
    State(app): State<Arc<AppState>>,
    paging: Result<Query<Paging>, QueryRejection>,
) -> Result<Json<Page<crate::incoming::IncomingFile>>, ApiError> {
    let Query(paging) = paging.map_err(|e| ApiError::bad_request(e.body_text()))?;
    let dir = app.config().paths.sources();
    let all = crate::incoming::list(&app, &dir, crate::jobs::source_import::IMPORT_KIND).await?;
    Ok(Json(Page::slice(all, &paging)))
}

fn source_id(id: Result<UrlPath<i64>, PathRejection>) -> Result<SourceId, ApiError> {
    id.map(|UrlPath(id)| SourceId(id))
        .map_err(|e| ApiError::bad_request(e.body_text()))
}

async fn load(app: &AppState, id: SourceId) -> Result<SourceRow, ApiError> {
    app.db
        .read(move |c| rows::get(c, id))
        .await?
        .ok_or_else(|| ApiError::not_found("No such source."))
}

/// `GET /sources/{id}/files` query: paging, a filter and a search.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FilesQuery {
    limit: Option<u32>,
    offset: Option<u32>,
    filter: Option<String>,
    q: Option<String>,
}

async fn files(
    State(app): State<Arc<AppState>>,
    id: Result<UrlPath<i64>, PathRejection>,
    query: Result<Query<FilesQuery>, QueryRejection>,
) -> Result<Json<Page<FileRow>>, ApiError> {
    let id = source_id(id)?;
    let Query(query) = query.map_err(|e| ApiError::bad_request(e.body_text()))?;
    let filter = query
        .filter
        .as_deref()
        .filter(|f| !f.is_empty())
        .map(|f| {
            FileFilter::parse(f)
                .ok_or_else(|| ApiError::bad_request("filter is matched, unmatched or wanted."))
        })
        .transpose()?;
    let paging = Paging {
        limit: query.limit,
        offset: query.offset,
    };
    let (limit, offset) = paging.resolve();
    let query = FileQuery { filter, q: query.q };
    load(&app, id).await?;
    let (items, total) = app
        .db
        .read(move |c| detail::files(c, id, &query, limit, offset))
        .await?;
    Ok(Json(Page { items, total }))
}

/// `GET /sources/{id}`: the source with how its files classify.
async fn show(
    State(app): State<Arc<AppState>>,
    id: Result<UrlPath<i64>, PathRejection>,
) -> Result<Json<SourceDetail>, ApiError> {
    let id = source_id(id)?;
    app.db
        .read(move |c| detail::detail(c, id))
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("No such source."))
}

/// `GET /sources/{id}/preview`: how many files each platform with a DAT would match.
async fn preview(
    State(app): State<Arc<AppState>>,
    id: Result<UrlPath<i64>, PathRejection>,
) -> Result<Json<Preview>, ApiError> {
    let id = source_id(id)?;
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
    platform_id: Option<Option<String>>,
    binding: Option<String>,
    seed_policy: Option<String>,
    state: Option<String>,
}

/// Distinguishes a `null` field from an absent one.
#[allow(clippy::option_option)] // The shape `Update::platform_id` needs.
fn present<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(d).map(Some)
}

/// The `PUT /sources/{id}` answer: the item, and the binding job it queued.
#[derive(Debug, Serialize)]
struct Updated {
    #[serde(flatten)]
    source: SourceRow,
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
            let why = source_import::unbound_reason(threshold);
            (SourceState::Unbound, Some(why))
        };
        rows::set_state(conn, id, state, reason.as_deref())?;
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
        (None, Some(_)) => Err(ApiError::bad_request("binding is automatic.")),
        (Some(Some(p)), None) => Ok(Some(Choice::Platform(PlatformId(p.clone())))),
        (Some(None), None) => Ok(Some(Choice::Ignore)),
        (None, None) => Ok(None),
    }
}

async fn update(
    State(app): State<Arc<AppState>>,
    id: Result<UrlPath<i64>, PathRejection>,
    body: Bytes,
) -> Result<Response, ApiError> {
    let id = source_id(id)?;
    let req: Update =
        serde_json::from_slice(&body).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let row = load(&app, id).await?;
    let seed = req
        .seed_policy
        .as_deref()
        .map(|s| {
            rows::seed_from_text(s).ok_or_else(|| {
                ApiError::bad_request("seed_policy is none, client or ratio:N with N above 0.")
            })
        })
        .transpose()?;
    let enable = match req.state.as_deref() {
        None => None,
        Some("disabled") => Some(false),
        Some("enabled") => Some(true),
        Some(_) => return Err(ApiError::bad_request("state is disabled or enabled.")),
    };
    let choice = choice(&req)?;
    if let Some(Choice::Platform(p)) = &choice {
        let p = p.clone();
        if !app.db.read(move |c| rows::platform_exists(c, &p)).await? {
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
        .write(move |c| {
            let tx = c.transaction()?;
            if let Some(choice) = &requested {
                rows::request_binding(&tx, id, choice)?;
            }
            if let Some(s) = &stored_seed {
                rows::set_seed_policy(&tx, id, s)?;
            }
            match enable {
                Some(false) => rows::set_state(&tx, id, SourceState::Disabled, None)?,
                Some(true) => enable_source(&tx, id, threshold)?,
                None => {}
            }
            let row = rows::get(&tx, id)?;
            crate::db::commit(tx)?;
            Ok(row)
        })
        .await?
        .ok_or_else(|| ApiError::not_found("No such source."))?;
    if let (Some(seed), Some(cid)) = (seed, &updated.client_id) {
        let applied = match app.client() {
            Some(client) => client
                .set_seed_policy(&ClientTorrentId::new(cid.as_str()), seed)
                .await
                .map_err(|e| tracing::warn!(source = %id, error = %e, "cannot apply the seed policy in the client"))
                .is_ok(),
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
        source: updated,
        job_id,
    };
    Ok((status, Json(body)).into_response())
}

async fn remove(
    State(app): State<Arc<AppState>>,
    id: Result<UrlPath<i64>, PathRejection>,
) -> Result<StatusCode, ApiError> {
    let id = source_id(id)?;
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
    if row.client_id.is_some() && app.client_frozen() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "conflict",
            "The download client is paused while a core runs. Remove the source at the menu.",
        ));
    }
    if let (Some(cid), Some(client)) = (&row.client_id, app.client()) {
        match client
            .remove(&ClientTorrentId::new(cid.as_str()), false)
            .await
        {
            Ok(()) | Err(ClientError::NotFound) => {}
            Err(e) => {
                return Err(ApiError::new(
                    StatusCode::BAD_GATEWAY,
                    "internal",
                    format!("The download client did not remove the source: {e}."),
                ))
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

async fn upload(
    State(app): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let (name, bytes, infohash, is_torrent) = if content_type.starts_with("multipart/form-data") {
        let content_type = content_type.to_owned();
        // Parsing walks every file entry, so it stays off the async workers.
        let (name, bytes, infohash) =
            crate::threads::blocking(crate::threads::label::SOURCE_FILE, move || {
                let (filename, data) = multipart_file(&content_type, &body).ok_or_else(|| {
                    ApiError::bad_request("Send one .torrent file as multipart form data.")
                })?;
                let meta = torrent::parse_torrent(data).map_err(|e| {
                    ApiError::bad_request(format!("Not a valid .torrent file: {e}."))
                })?;
                let name = file_name(&filename, "upload", "torrent");
                Ok::<_, ApiError>((name, data.to_vec(), meta.infohash))
            })
            .await
            .map_err(|e| crate::Error::Task(e.to_string()))??;
        (name, bytes, infohash, true)
    } else {
        let req: MagnetBody = serde_json::from_slice(&body).map_err(|e| {
            ApiError::bad_request(format!(
                "Send {{ \"magnet\": \"...\" }} or a .torrent file: {e}"
            ))
        })?;
        let uri = req.magnet.trim();
        let parsed = magnet::parse_magnet(uri)
            .map_err(|e| ApiError::bad_request(format!("Not a valid magnet: {e}.")))?;
        let hex = InfoHash::from_bytes(parsed.infohash).to_string();
        let stem = parsed.display_name.unwrap_or_else(|| hex.clone());
        let name = file_name(&stem, &hex, "magnet");
        (
            name,
            format!("{uri}\n").into_bytes(),
            parsed.infohash,
            false,
        )
    };
    let hex = InfoHash::from_bytes(infohash).to_string();
    let existing = app
        .db
        .read(move |c| rows::find_by_infohash(c, &hex))
        .await?;
    if existing.is_some_and(|r| !is_torrent || r.state != SourceState::Resolving) {
        return Err(ApiError::bad_request(DUPLICATE));
    }
    let dir = app.config().paths.sources();
    let placed = crate::threads::blocking(crate::threads::label::SOURCE_FILE, move || {
        place(&dir, &name, &bytes)
    })
    .await
    .map_err(|e| crate::Error::Task(e.to_string()))?;
    let path = match placed {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "conflict",
                "A file with this name is already waiting in the sources directory.",
            ))
        }
        Err(e) => return Err(crate::Error::Io(e).into()),
    };
    let job = Arc::new(SourceImport { path: path.clone() });
    let placed =
        crate::incoming::queue_placed(&app, &path, source_import::IMPORT_KIND, job).await?;
    Ok((StatusCode::ACCEPTED, Json(placed)).into_response())
}

/// Writes `bytes` under `name`, or `name (N)` when taken, in `dir`. The name
/// is claimed with `create_new` and filled by renaming a uniquely named
/// temporary file over the claim, so concurrent uploads never share a path.
/// `AlreadyExists` when no free name is found.
fn place(dir: &Path, name: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    std::fs::create_dir_all(dir)?;
    let (stem, ext) = name.rsplit_once('.').unwrap_or((name, ""));
    let target = (0..PLACE_ATTEMPTS)
        .map(|n| match n {
            0 => dir.join(name),
            n => dir.join(format!("{stem} ({n}).{ext}")),
        })
        .find_map(|candidate| {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&candidate)
            {
                Ok(_) => Some(Ok(candidate)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => None,
                Err(e) => Some(Err(e)),
            }
        })
        .unwrap_or_else(|| {
            Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "no free file name",
            ))
        })?;
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".upload-{}-{n}.part", std::process::id()));
    let written = std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, &target));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(&target);
        return Err(e);
    }
    Ok(target)
}

/// A safe basename ending in `.ext` from a user-supplied name, else `fallback.ext`.
fn file_name(given: &str, fallback: &str, ext: &str) -> String {
    let base = given.rsplit(['/', '\\']).next().unwrap_or("");
    let suffix = format!(".{ext}");
    let stem = if base.len() > suffix.len() && base.to_ascii_lowercase().ends_with(&suffix) {
        &base[..base.len() - suffix.len()]
    } else {
        base
    };
    let clean: String = stem
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " -_.,()[]+'".contains(c) {
                c
            } else {
                '_'
            }
        })
        .take(120)
        .collect();
    let clean = clean.trim().trim_start_matches('.');
    if clean.is_empty() {
        format!("{fallback}{suffix}")
    } else {
        format!("{clean}{suffix}")
    }
}

/// The filename and bytes of the first part carrying a filename in a
/// `multipart/form-data` body.
fn multipart_file<'b>(content_type: &str, body: &'b [u8]) -> Option<(String, &'b [u8])> {
    let boundary = content_type
        .split(';')
        .map(str::trim)
        .find_map(|p| p.strip_prefix("boundary="))?
        .trim_matches('"');
    let delimiter = format!("--{boundary}").into_bytes();
    let mut rest = &body[find(body, &delimiter)? + delimiter.len()..];
    loop {
        if rest.starts_with(b"--") {
            return None;
        }
        let head_end = find(rest, b"\r\n\r\n")?;
        let head = String::from_utf8_lossy(&rest[..head_end]);
        let data_start = head_end + 4;
        let mut close = b"\r\n".to_vec();
        close.extend_from_slice(&delimiter);
        let data_end = data_start + find(&rest[data_start..], &close)?;
        let filename = head.lines().find_map(|l| {
            let lower = l.to_ascii_lowercase();
            if !lower.starts_with("content-disposition:") {
                return None;
            }
            let at = lower.find("filename=\"")? + "filename=\"".len();
            let name = &l[at..];
            Some(name[..name.find('"')?].to_owned())
        });
        if let Some(name) = filename {
            return Some((name, &rest[data_start..data_end]));
        }
        rest = &rest[data_end + close.len()..];
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multipart_finds_the_file_part() {
        let body = b"--XyZ\r\nContent-Disposition: form-data; name=\"note\"\r\n\r\nhi\r\n--XyZ\r\nContent-Disposition: form-data; name=\"file\"; filename=\"Set.torrent\"\r\nContent-Type: application/x-bittorrent\r\n\r\nd1:ae\r\n--XyZ--\r\n";
        let (name, data) = multipart_file("multipart/form-data; boundary=XyZ", body).expect("file");
        assert_eq!((name.as_str(), data), ("Set.torrent", &b"d1:ae"[..]));
        assert!(multipart_file("multipart/form-data", body).is_none());
        let no_file =
            b"--XyZ\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\nx\r\n--XyZ--\r\n";
        assert!(multipart_file("multipart/form-data; boundary=\"XyZ\"", no_file).is_none());
    }

    #[test]
    fn names_are_made_safe() {
        assert_eq!(
            file_name("../../x/Set (A).TORRENT", "u", "torrent"),
            "Set (A).torrent"
        );
        assert_eq!(file_name("a/b:c?.torrent", "u", "torrent"), "b_c_.torrent");
        assert_eq!(file_name("...", "u", "magnet"), "u.magnet");
        assert_eq!(file_name("", "abc", "magnet"), "abc.magnet");
    }

    #[test]
    fn placing_never_overwrites() {
        let dir = tempfile::tempdir().expect("tempdir");
        let a = place(dir.path(), "s.torrent", b"1").expect("place");
        let b = place(dir.path(), "s.torrent", b"2").expect("place");
        assert_eq!(a.file_name().and_then(|n| n.to_str()), Some("s.torrent"));
        assert_eq!(
            b.file_name().and_then(|n| n.to_str()),
            Some("s (1).torrent")
        );
        assert_eq!(std::fs::read(&b).expect("read"), b"2");
        assert_eq!(std::fs::read_dir(dir.path()).expect("dir").count(), 2);
    }

    #[test]
    fn concurrent_placements_keep_every_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let placed: Vec<PathBuf> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..8u8)
                .map(|i| s.spawn(move || place(root, "c.torrent", &[i]).expect("place")))
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("join"))
                .collect()
        });
        let mut contents: Vec<u8> = placed
            .iter()
            .map(|p| std::fs::read(p).expect("read")[0])
            .collect();
        contents.sort_unstable();
        assert_eq!(contents, (0..8).collect::<Vec<u8>>());
        assert_eq!(std::fs::read_dir(dir.path()).expect("dir").count(), 8);
    }

    #[test]
    fn placing_reports_when_no_name_is_free() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("f.magnet"), b"x").expect("write");
        for n in 1..PLACE_ATTEMPTS {
            std::fs::write(dir.path().join(format!("f ({n}).magnet")), b"x").expect("write");
        }
        let err = place(dir.path(), "f.magnet", b"y").expect_err("full");
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    }
}
