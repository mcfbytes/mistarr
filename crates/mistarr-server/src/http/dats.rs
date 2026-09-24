//! The DATs routes of `docs/API.md`.

use std::io::Write;
use std::path::{Path as FsPath, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{DefaultBodyLimit, Multipart, Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Serialize;

use super::{ApiError, Page, Paging};
use crate::app::AppState;
use crate::db::dats::{self, DatVersionId, DatVersionRow};
use crate::db::jobs::JobId;
use crate::incoming::IncomingFile;
use crate::jobs::dat_import::{unique_path, DatImport, Recompute};
use crate::jobs::Scheduler;

/// Largest accepted upload; daily packs of every system fit well inside.
const MAX_UPLOAD: usize = 512 * 1024 * 1024;

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/dats", get(list))
        .route("/dats/incoming", get(incoming))
        .route(
            "/dats/upload",
            post(upload).layer(DefaultBodyLimit::max(MAX_UPLOAD)),
        )
        .route("/dats/{id}", delete(retire))
}

async fn list(
    State(app): State<Arc<AppState>>,
    paging: Result<Query<Paging>, QueryRejection>,
) -> Result<Json<Page<DatVersionRow>>, ApiError> {
    let Query(paging) = paging.map_err(|e| ApiError::bad_request(e.body_text()))?;
    let (limit, offset) = paging.resolve();
    let (items, total) = app.db.read(move |c| dats::list(c, limit, offset)).await?;
    Ok(Json(Page { items, total }))
}

/// `GET /dats/incoming`: files in `dats/` not loaded yet, and rejected ones.
async fn incoming(
    State(app): State<Arc<AppState>>,
    paging: Result<Query<Paging>, QueryRejection>,
) -> Result<Json<Page<IncomingFile>>, ApiError> {
    let Query(paging) = paging.map_err(|e| ApiError::bad_request(e.body_text()))?;
    let dir = app.config().paths.dats();
    let all = crate::incoming::list(&app, &dir, crate::jobs::dat_import::KIND).await?;
    Ok(Json(Page::slice(all, &paging)))
}

/// The answer to an upload: where the file landed and the job importing it.
#[derive(Debug, Serialize)]
struct Uploaded {
    file: String,
    job_id: JobId,
}

fn accepted(name: &str) -> bool {
    FsPath::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .is_some_and(|e| matches!(e.as_str(), "dat" | "xml" | "zip"))
}

/// A temporary name in `dir` the watcher ignores.
fn part_path(dir: &FsPath) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    dir.join(format!(".upload-{}-{n}.part", std::process::id()))
}

async fn upload(
    State(app): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<Uploaded>), ApiError> {
    let dir = app.config().paths.dats();
    loop {
        let field = multipart
            .next_field()
            .await
            .map_err(|e| ApiError::bad_request(e.body_text()))?
            .ok_or_else(|| ApiError::bad_request("no file in the upload"))?;
        let Some(name) = field
            .file_name()
            .and_then(|n| FsPath::new(n).file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .filter(|n| !n.starts_with('.'))
        else {
            continue;
        };
        if !accepted(&name) {
            return Err(ApiError::bad_request("expected a .dat, .xml or .zip file"));
        }
        let part = part_path(&dir);
        if let Err(e) = write_field(field, &part).await {
            let _ = std::fs::remove_file(&part);
            return Err(e);
        }
        let target = unique_path(&dir, &name);
        std::fs::rename(&part, &target).map_err(crate::Error::from)?;
        let job_id = Scheduler::enqueue(&app, Arc::new(DatImport::new(&target))).await?;
        let file = target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or(name);
        return Ok((StatusCode::ACCEPTED, Json(Uploaded { file, job_id })));
    }
}

/// Streams one multipart field to `path`, one chunk in memory at a time.
async fn write_field(
    mut field: axum::extract::multipart::Field<'_>,
    path: &FsPath,
) -> Result<(), ApiError> {
    let mut file = Some(std::fs::File::create(path).map_err(crate::Error::from)?);
    while let Some(chunk) = field
        .chunk()
        .await
        .map_err(|e| ApiError::bad_request(e.body_text()))?
    {
        let Some(mut f) = file.take() else {
            break;
        };
        let written = tokio::task::spawn_blocking(move || {
            f.write_all(&chunk)?;
            Ok::<_, std::io::Error>(f)
        })
        .await
        .map_err(|e| crate::Error::Task(e.to_string()))?;
        file = Some(written.map_err(crate::Error::from)?);
    }
    Ok(())
}

async fn retire(
    State(app): State<Arc<AppState>>,
    id: Result<Path<i64>, PathRejection>,
) -> Result<StatusCode, ApiError> {
    let Path(id) = id.map_err(|e| ApiError::bad_request(e.body_text()))?;
    let row = app
        .db
        .write(move |c| {
            let tx = c.transaction()?;
            let row = dats::retire(&tx, DatVersionId(id))?;
            tx.commit()?;
            Ok(row)
        })
        .await?
        .ok_or_else(|| ApiError::not_found("no such DAT version"))?;
    if let Some(p) = row.platform_id {
        Scheduler::enqueue(&app, Arc::new(Recompute::new(&p.0))).await?;
        crate::jobs::remap::enqueue(&app, Some(vec![p])).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_dat_files_are_accepted_and_parts_are_hidden() {
        assert!(accepted("a.DAT") && accepted("b.xml") && accepted("c.zip"));
        assert!(!accepted("d.txt") && !accepted("dat"));
        let p = part_path(FsPath::new("/x"));
        assert!(p
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with('.')));
        assert_ne!(p, part_path(FsPath::new("/x")));
    }
}
