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
use crate::jobs::dat_import::{unique_path, DatImport, Recompute, REASON_SUFFIX, REJECTED_DIR};
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
        .route("/dats/rejected/{file}/retry", post(retry_rejected))
        .route("/dats/rejected/{file}", delete(delete_rejected))
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
        let written = crate::threads::blocking(crate::threads::label::DAT_SAVE, move || {
            f.write_all(&chunk)?;
            Ok::<_, std::io::Error>(f)
        })
        .await
        .map_err(|e| crate::Error::Task(e.to_string()))?;
        file = Some(written.map_err(crate::Error::from)?);
    }
    Ok(())
}

/// The rejected file `name` in `dats/rejected/`, when it is a plain file name that exists.
fn rejected_file(dats: &FsPath, name: &str) -> Result<PathBuf, ApiError> {
    let plain = !name.is_empty()
        && !name.starts_with('.')
        && !name.contains(['/', '\\'])
        && !name.ends_with(REASON_SUFFIX);
    if !plain {
        return Err(ApiError::bad_request("not a file name in dats/rejected/"));
    }
    let path = dats.join(REJECTED_DIR).join(name);
    match std::fs::symlink_metadata(&path) {
        Ok(meta) if meta.is_file() => Ok(path),
        _ => Err(ApiError::not_found("no such rejected file")),
    }
}

/// Moves `from` into `dir` under `name` or the first free variant, never replacing a
/// file: a hard link where the file system has them, else a copy made with `create_new`.
fn move_new(from: &FsPath, dir: &FsPath, name: &str) -> std::io::Result<PathBuf> {
    use std::io::ErrorKind;
    loop {
        let target = unique_path(dir, name);
        let placed = match std::fs::hard_link(from, &target) {
            Err(e) if e.kind() == ErrorKind::NotFound || e.kind() == ErrorKind::AlreadyExists => {
                Err(e)
            }
            Err(_) => copy_new(from, &target),
            ok => ok,
        };
        match placed {
            Ok(()) => {
                std::fs::remove_file(from)?;
                return Ok(target);
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
}

fn copy_new(from: &FsPath, to: &FsPath) -> std::io::Result<()> {
    let mut src = std::fs::File::open(from)?;
    let mut dst = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)?;
    let copied = std::io::copy(&mut src, &mut dst).and_then(|_| dst.sync_all());
    if copied.is_err() {
        let _ = std::fs::remove_file(to);
    }
    copied
}

fn reason_of(path: &FsPath) -> PathBuf {
    let mut reason = path.as_os_str().to_owned();
    reason.push(REASON_SUFFIX);
    PathBuf::from(reason)
}

/// `POST /dats/rejected/{file}/retry`: moves a rejected file back into `dats/`, under a
/// free name, drops its reason and queues its import.
async fn retry_rejected(
    State(app): State<Arc<AppState>>,
    file: Result<Path<String>, PathRejection>,
) -> Result<(StatusCode, Json<Uploaded>), ApiError> {
    let Path(name) = file.map_err(|e| ApiError::bad_request(e.body_text()))?;
    let dir = app.config().paths.dats();
    let path = rejected_file(&dir, &name)?;
    let target = match move_new(&path, &dir, &name) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ApiError::not_found("no such rejected file"))
        }
        moved => moved.map_err(crate::Error::from)?,
    };
    remove_if_present(&reason_of(&path))?;
    let job_id = Scheduler::enqueue(&app, Arc::new(DatImport::new(&target))).await?;
    let file = target
        .file_name()
        .map_or(name, |n| n.to_string_lossy().into_owned());
    Ok((StatusCode::ACCEPTED, Json(Uploaded { file, job_id })))
}

/// `DELETE /dats/rejected/{file}`: deletes a rejected file and its reason.
async fn delete_rejected(
    State(app): State<Arc<AppState>>,
    file: Result<Path<String>, PathRejection>,
) -> Result<StatusCode, ApiError> {
    let Path(name) = file.map_err(|e| ApiError::bad_request(e.body_text()))?;
    let path = rejected_file(&app.config().paths.dats(), &name)?;
    std::fs::remove_file(&path).map_err(crate::Error::from)?;
    remove_if_present(&reason_of(&path))?;
    Ok(StatusCode::NO_CONTENT)
}

fn remove_if_present(path: &FsPath) -> Result<(), ApiError> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(crate::Error::from(e).into()),
        _ => Ok(()),
    }
}

/// `DELETE /dats/{id}`: retires a loaded version with its titles and roms, unwants them and
/// queues the recompute job, which matches their files again and recomputes 1G1R.
async fn retire(
    State(app): State<Arc<AppState>>,
    id: Result<Path<i64>, PathRejection>,
) -> Result<StatusCode, ApiError> {
    let Path(id) = id.map_err(|e| ApiError::bad_request(e.body_text()))?;
    let row = app
        .db
        .write(move |c| {
            let tx = c.transaction()?;
            let row = dats::retire(&tx, DatVersionId(id), crate::unix_now())?;
            crate::db::commit(tx)?;
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
    fn rejected_files_are_named_plainly_and_must_exist() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(REJECTED_DIR)).expect("mkdir");
        std::fs::write(dir.path().join("rejected/a.dat"), b"x").expect("write");
        assert!(rejected_file(dir.path(), "a.dat").is_ok());
        for bad in ["", ".a.dat", "../a.dat", "x/a.dat", "a.dat.reason.txt"] {
            let e = rejected_file(dir.path(), bad).expect_err(bad);
            assert_eq!(e.status, StatusCode::BAD_REQUEST, "{bad}");
        }
        let e = rejected_file(dir.path(), "b.dat").expect_err("absent");
        assert_eq!(e.status, StatusCode::NOT_FOUND);
        assert!(reason_of(FsPath::new("/r/a.dat")).ends_with("a.dat.reason.txt"));
        assert!(remove_if_present(&dir.path().join("none")).is_ok());
        std::os::unix::fs::symlink(
            dir.path().join("rejected/a.dat"),
            dir.path().join("rejected/l.dat"),
        )
        .expect("symlink");
        let e = rejected_file(dir.path(), "l.dat").expect_err("a link is not a rejected file");
        assert_eq!(e.status, StatusCode::NOT_FOUND);
    }

    #[test]
    fn a_retried_file_never_replaces_one_of_the_same_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let from = dir.path().join("rejected.dat");
        std::fs::write(&from, b"retried").expect("write");
        std::fs::write(dir.path().join("a.dat"), b"waiting").expect("write");
        let moved = move_new(&from, dir.path(), "a.dat").expect("move");
        assert!(moved.ends_with("a (1).dat"));
        assert_eq!(std::fs::read(&moved).expect("read"), b"retried");
        assert_eq!(
            std::fs::read(dir.path().join("a.dat")).expect("read"),
            b"waiting"
        );
        assert!(!from.exists());
        let gone = move_new(&from, dir.path(), "b.dat").expect_err("moved already");
        assert_eq!(gone.kind(), std::io::ErrorKind::NotFound);
        let to = dir.path().join("c.dat");
        copy_new(&moved, &to).expect("copy");
        assert_eq!(
            copy_new(&moved, &to).expect_err("exists").kind(),
            std::io::ErrorKind::AlreadyExists
        );
    }

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
