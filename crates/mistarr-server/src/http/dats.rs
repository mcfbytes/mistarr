//! The DATs routes of `docs/API.md`.

use std::io::Write;
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

use axum::extract::multipart::Field;
use axum::extract::{DefaultBodyLimit, Multipart, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use mistarr_sources::intake::{REASON_SUFFIX, REJECTED_DIR};
use serde::Serialize;

use super::extract::with_file;
use super::{ApiError, ApiPath, ApiQuery, Paging};
use crate::app::AppState;
use crate::db::dats::{self, DatReason, DatRef, DatVersionRow};
use crate::db::ids::DatVersionId;
use crate::db::sql::Paged;
use crate::incoming::place::{part_path, place_moved, place_part, PlaceError};
use crate::incoming::IncomingFile;
use crate::jobs::dat_import::DatImport;
use crate::jobs::JobKind;

/// Largest accepted upload, [`crate::jobs::dat_import::MAX_DAT_BYTES`].
#[expect(
    clippy::cast_possible_truncation,
    reason = "512 MiB fits every usize the target has."
)]
const MAX_UPLOAD: usize = crate::jobs::dat_import::MAX_DAT_BYTES as usize;

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
    ApiQuery(paging): ApiQuery<Paging>,
) -> Result<Json<Paged<DatItem>>, ApiError> {
    let page = paging.resolve();
    let rows = app.db.read(move |c| dats::list(c, page)).await?;
    Ok(Json(Paged {
        items: rows.items.into_iter().map(DatItem::from).collect(),
        total: rows.total,
    }))
}

/// A DAT version as the API returns it: the row with its reason worded.
#[derive(Debug, Serialize)]
struct DatItem {
    #[serde(flatten)]
    row: DatVersionRow,
    reason: Option<String>,
}

impl From<DatVersionRow> for DatItem {
    fn from(row: DatVersionRow) -> Self {
        let reason = row.reason.as_ref().map(reason_text);
        Self { row, reason }
    }
}

/// The sentence the API shows for `reason`.
fn reason_text(reason: &DatReason) -> String {
    let other = |r: &DatRef| match &r.dat_name {
        Some(name) => format!("{name} version {}", r.version),
        None => format!("version {}", r.version),
    };
    match reason {
        DatReason::Removed => "Removed; its games are no longer listed".to_owned(),
        DatReason::Replaced(by) => format!("Replaced by {}", other(by)),
        DatReason::Older(than) => format!("Older than {}, which stays current", other(than)),
    }
}

/// `GET /dats/incoming`: files in `dats/` not loaded yet, and rejected ones.
async fn incoming(
    State(app): State<Arc<AppState>>,
    ApiQuery(paging): ApiQuery<Paging>,
) -> Result<Json<Paged<IncomingFile>>, ApiError> {
    let dir = app.config().paths.dats();
    let all = crate::incoming::list(&app, &dir, JobKind::DatImport).await?;
    Ok(Json(paging.resolve().slice(all)))
}

fn accepted(name: &str) -> bool {
    FsPath::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .is_some_and(|e| matches!(e.as_str(), "dat" | "xml" | "zip"))
}

/// `POST /dats/upload`: streams the first file of the form into `dats/` and queues its import.
async fn upload(
    State(app): State<Arc<AppState>>,
    mut form: Multipart,
) -> Result<(StatusCode, Json<IncomingFile>), ApiError> {
    let dir = app.config().paths.dats();
    let hidden = |n: &str| n.starts_with('.');
    let (name, part) = with_file(&mut form, hidden, async |name: String, field: Field<'_>| {
        if !accepted(&name) {
            return Err(ApiError::bad_request("Upload a .dat, .xml or .zip file."));
        }
        let part = part_path(&dir);
        if let Err(e) = write_field(field, &part).await {
            let _ = std::fs::remove_file(&part);
            return Err(e);
        }
        Ok((name, part))
    })
    .await?;
    let placed = place_part(&app, &part, &name).await?;
    Ok((StatusCode::ACCEPTED, Json(placed)))
}

/// Streams one multipart field to `path`, one chunk in memory at a time.
async fn write_field(mut field: Field<'_>, path: &FsPath) -> Result<(), ApiError> {
    let mut file = Some(std::fs::File::create(path).map_err(crate::Error::io_at(path))?);
    while let Some(chunk) = field
        .chunk()
        .await
        .map_err(|e| ApiError::bad_request(e.body_text()))?
    {
        let Some(mut f) = file.take() else {
            break;
        };
        let written = crate::threads::run(crate::threads::label::DAT_SAVE, move || {
            f.write_all(&chunk)?;
            Ok::<_, std::io::Error>(f)
        })
        .await?;
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
        return Err(ApiError::bad_request(
            "That is not a file name in dats/rejected/.",
        ));
    }
    let path = dats.join(REJECTED_DIR).join(name);
    match std::fs::symlink_metadata(&path) {
        Ok(meta) if meta.is_file() => Ok(path),
        _ => Err(ApiError::no_such("rejected file")),
    }
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
    ApiPath(name): ApiPath<String>,
) -> Result<(StatusCode, Json<IncomingFile>), ApiError> {
    let dir = app.config().paths.dats();
    let path = rejected_file(&dir, &name)?;
    let target = match place_moved(&path, &dir, &name) {
        Err(PlaceError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ApiError::no_such("rejected file"))
        }
        moved => moved.map_err(crate::Error::from)?,
    };
    remove_if_present(&reason_of(&path))?;
    let job = Arc::new(DatImport::new(&target));
    let placed = crate::incoming::queue_placed(&app, &target, JobKind::DatImport, job).await?;
    Ok((StatusCode::ACCEPTED, Json(placed)))
}

/// `DELETE /dats/rejected/{file}`: deletes a rejected file and its reason.
async fn delete_rejected(
    State(app): State<Arc<AppState>>,
    ApiPath(name): ApiPath<String>,
) -> Result<StatusCode, ApiError> {
    let path = rejected_file(&app.config().paths.dats(), &name)?;
    std::fs::remove_file(&path).map_err(crate::Error::io_at(&path))?;
    remove_if_present(&reason_of(&path))?;
    Ok(StatusCode::NO_CONTENT)
}

fn remove_if_present(path: &FsPath) -> Result<(), ApiError> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(crate::Error::from(e).into()),
        _ => Ok(()),
    }
}

/// `DELETE /dats/{id}`: retires a loaded version with its titles and roms and unwants them.
/// A detached task then queues the recompute and scan and binds the waiting sources.
async fn retire(
    State(app): State<Arc<AppState>>,
    ApiPath(id): ApiPath<DatVersionId>,
) -> Result<StatusCode, ApiError> {
    let row = app
        .db
        .write_tx(move |tx| {
            let row = dats::retire(tx, id, crate::unix_now())?;
            Ok(row)
        })
        .await?
        .ok_or_else(|| ApiError::no_such("DAT version"))?;
    if let Some(p) = row.platform_id {
        // The rebind re-scores every unbound source, so it stays off the request.
        tokio::spawn(
            async move { crate::jobs::follow_up::catalogue_changed(&app, &[p], false).await },
        );
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasons_name_the_other_version() {
        let same = DatRef {
            dat_name: None,
            version: "20260201".into(),
        };
        let other = DatRef {
            dat_name: Some("Example Vendor - Example System (DB Export)".into()),
            version: "20260301".into(),
        };
        assert_eq!(
            reason_text(&DatReason::Replaced(same)),
            "Replaced by version 20260201"
        );
        assert_eq!(
            reason_text(&DatReason::Older(other)),
            "Older than Example Vendor - Example System (DB Export) version 20260301, which stays current"
        );
        assert_eq!(
            reason_text(&DatReason::Removed),
            "Removed; its games are no longer listed"
        );
    }

    #[test]
    fn rejected_files_are_named_plainly_and_must_exist() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(REJECTED_DIR)).expect("mkdir");
        std::fs::write(dir.path().join("rejected/a.dat"), b"x").expect("write");
        assert!(rejected_file(dir.path(), "a.dat").is_ok());
        for bad in ["", ".a.dat", "../a.dat", "x/a.dat", "a.dat.reason.txt"] {
            let e = rejected_file(dir.path(), bad).expect_err(bad);
            assert_eq!(e.status(), StatusCode::BAD_REQUEST, "{bad}");
        }
        let e = rejected_file(dir.path(), "b.dat").expect_err("absent");
        assert_eq!(e.status(), StatusCode::NOT_FOUND);
        assert!(reason_of(FsPath::new("/r/a.dat")).ends_with("a.dat.reason.txt"));
        assert!(remove_if_present(&dir.path().join("none")).is_ok());
        std::os::unix::fs::symlink(
            dir.path().join("rejected/a.dat"),
            dir.path().join("rejected/l.dat"),
        )
        .expect("symlink");
        let e = rejected_file(dir.path(), "l.dat").expect_err("a link is not a rejected file");
        assert_eq!(e.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn retiring_a_version_queues_its_recompute_and_scan() {
        use crate::db::jobs::{find_in, JobState};
        use crate::jobs::{dat_import::Recompute, scan::ScanJob, Job as _};
        use mistarr_core::PlatformId;

        let (_dir, app) = crate::app::testutil::state();
        std::fs::create_dir_all(app.config().paths.games.join("NES")).expect("games");
        let id = app
            .db
            .write_tx(|tx| {
                let v = dats::NewVersion {
                    dat_name: "Example Console",
                    version: "1",
                    source_file: "a.dat",
                    platform: Some("nes"),
                    now: 1,
                };
                Ok(dats::upsert_version(tx, &v)?.id)
            })
            .await
            .expect("version");
        let status = retire(State(Arc::clone(&app)), ApiPath(id))
            .await
            .expect("retire");
        assert_eq!(status, StatusCode::NO_CONTENT);
        let recompute = Recompute::new(&mistarr_core::PlatformId::new("nes")).payload();
        let scan = ScanJob {
            platform_id: Some(PlatformId::new("nes")),
        }
        .payload();
        crate::testing::eventually("a recompute and a scan", || async {
            let (r, s) = (recompute.clone(), scan.clone());
            let found = app
                .db
                .read(move |c| {
                    let any = [&JobState::ACTIVE[..], &JobState::FINISHED].concat();
                    Ok(find_in(c, JobKind::Recompute, &r, &any)?.is_some()
                        && find_in(c, JobKind::Scan, &s, &any)?.is_some())
                })
                .await
                .expect("jobs");
            found
        })
        .await;
    }

    #[test]
    fn only_dat_files_are_accepted() {
        assert!(accepted("a.DAT") && accepted("b.xml") && accepted("c.zip"));
        assert!(!accepted("d.txt") && !accepted("dat"));
    }
}
