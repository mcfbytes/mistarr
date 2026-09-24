//! The launch routes of `docs/API.md` "Launching": a title or a bare core, through
//! MiSTer Main's command FIFO. Paths come from the database and the SD card only.

use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::routing::post;
use axum::{Json, Router};
use mistarr_mister::launch::{self as mister, CommandSink};
use mistarr_mister::platforms::{self as table, Kind};
use serde::Serialize;

use super::ApiError;
use crate::app::AppState;
use crate::db::launch::{self, LaunchTitle};
use crate::db::platforms;
use crate::db::titles::TitleId;
use crate::status::{launch_state, LaunchState};

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/titles/{id}/launch", post(title))
        .route("/platforms/{id}/launch-core", post(core))
}

/// What was handed to MiSTer Main.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Launched {
    /// The `.rbf` or `.mra` loaded, relative to the SD root.
    pub core: String,
    /// The game file handed to the core, relative to `games/`; `None` for a bare core or an MRA.
    pub file: Option<String>,
}

async fn title(
    State(app): State<Arc<AppState>>,
    id: Result<Path<i64>, PathRejection>,
) -> Result<Json<Launched>, ApiError> {
    let Path(id) = id.map_err(|e| ApiError::bad_request(e.body_text()))?;
    Ok(Json(launch_title(&app, TitleId(id)).await?))
}

async fn core(
    State(app): State<Arc<AppState>>,
    id: Result<Path<String>, PathRejection>,
) -> Result<Json<Launched>, ApiError> {
    let Path(id) = id.map_err(|e| ApiError::bad_request(e.body_text()))?;
    Ok(Json(launch_core(&app, &id).await?))
}

/// The sink, when launching is enabled and MiSTer Main's FIFO exists.
fn sink(app: &AppState) -> Result<Arc<dyn CommandSink>, ApiError> {
    match launch_state(app) {
        LaunchState::Ready => Ok(app.command_sink()),
        LaunchState::Disabled => Err(ApiError::conflict("launching is turned off in settings")),
        LaunchState::Unavailable => Err(unavailable(&mistarr_mister::Error::CommandAbsent)),
    }
}

fn unavailable(e: &mistarr_mister::Error) -> ApiError {
    ApiError::unavailable(e.to_string())
}

/// Maps a failure to hand a command to Main onto the documented statuses.
fn command_error(e: mistarr_mister::Error) -> ApiError {
    use mistarr_mister::Error as E;
    match e {
        E::CommandAbsent | E::NotListening | E::CommandBusy => unavailable(&e),
        E::UnsafePath(_) => ApiError::conflict(e.to_string()),
        e => crate::Error::Job(e.to_string()).into(),
    }
}

/// Starts title `id` in its platform's newest core, or its MRA for an arcade title.
///
/// # Errors
///
/// [`ApiError`] 404 for an unknown title, 409 when launching is off or the title
/// cannot start (not in the collection, a BIOS entry, no core or MRA on the card),
/// 503 when MiSTer Main cannot take the command.
pub async fn launch_title(app: &AppState, id: TitleId) -> Result<Launched, ApiError> {
    let sink = sink(app)?;
    let title = app
        .db
        .read(move |c| launch::title(c, id))
        .await?
        .ok_or_else(|| ApiError::not_found("no such title"))?;
    if title.bios {
        return Err(ApiError::conflict("BIOS entries are not launched"));
    }
    if !title.complete {
        return Err(ApiError::conflict(
            "not every file of this entry is in the collection",
        ));
    }
    let paths = app.config().paths;
    let dir = app.options.launch_dir.clone();
    blocking(move || {
        let launched = plan_title(&title, &paths.root, &paths.games, &dir)?;
        sink.send(&launched.1).map_err(command_error)?;
        Ok(launched.0)
    })
    .await
}

/// Starts the newest installed core of platform `id` with no game.
///
/// # Errors
///
/// [`ApiError`] 404 for an unknown platform, 409 when launching is off, no core
/// for it is installed or it is the arcade platform, 503 when MiSTer Main cannot
/// take the command.
pub async fn launch_core(app: &AppState, id: &str) -> Result<Launched, ApiError> {
    let sink = sink(app)?;
    let lookup = id.to_owned();
    let known = app
        .db
        .read(move |c| platforms::get(c, &lookup))
        .await?
        .is_some();
    let row = table::by_id(id)
        .filter(|_| known)
        .ok_or_else(|| ApiError::not_found("no such platform"))?;
    if row.kind == Kind::Arcade {
        return Err(ApiError::conflict(
            "arcade cores start from an MRA; launch an arcade title instead",
        ));
    }
    let root = app.config().paths.root;
    blocking(move || {
        let core = mister::find_core(&root, row).ok_or_else(no_core)?;
        let line = mister::load_core(&core.path).map_err(command_error)?;
        sink.send(&line).map_err(command_error)?;
        tracing::info!(core = %core.path.display(), "core launched");
        Ok(Launched {
            core: relative(&core.path, &root),
            file: None,
        })
    })
    .await
}

fn no_core() -> ApiError {
    ApiError::conflict("no core for this platform is installed")
}

/// What to launch for `title` and the command line that does it, writing the MGL
/// for a DAT entry into `dir`.
fn plan_title(
    title: &LaunchTitle,
    root: &FsPath,
    games: &FsPath,
    dir: &FsPath,
) -> Result<(Launched, String), ApiError> {
    if title.source == "mra" {
        let rel = title
            .mra_path
            .as_deref()
            .ok_or_else(|| ApiError::conflict("the entry names no MRA file"))?;
        let mra = root.join("_Arcade").join(rel);
        if !mra.is_file() {
            return Err(ApiError::conflict(format!(
                "_Arcade/{rel} is no longer on the SD card"
            )));
        }
        let line = mister::load_core(&mra).map_err(command_error)?;
        tracing::info!(mra = %rel, "arcade title launched");
        let core = relative(&mra, root);
        return Ok((Launched { core, file: None }, line));
    }
    let row =
        table::by_id(&title.platform_id).ok_or_else(|| ApiError::not_found("no such platform"))?;
    let slot = row
        .launch
        .ok_or_else(|| ApiError::conflict("this entry has no MRA to start it from"))?;
    let core = mister::find_core(root, row).ok_or_else(no_core)?;
    let files: Vec<&str> = title.files.iter().map(String::as_str).collect();
    let file = mister::game_path(row.kind, &files)
        .ok_or_else(|| ApiError::conflict("the entry has no file the core can load"))?;
    let doc = mister::mgl(&core.mgl_rbf, slot, &games.join(&file)).map_err(command_error)?;
    let mgl = mister::write_mgl(dir, &doc).map_err(|e| crate::Error::Job(e.to_string()))?;
    let line = mister::load_core(&mgl).map_err(command_error)?;
    tracing::info!(core = %core.mgl_rbf, file = %file, "game launched");
    let launched = Launched {
        core: relative(&core.path, root),
        file: Some(file),
    };
    Ok((launched, line))
}

/// `path` relative to `root` with `/` separators, or whole when outside it.
fn relative(path: &FsPath, root: &FsPath) -> String {
    path.strip_prefix(root)
        .map_or_else(|_| path.to_path_buf(), PathBuf::from)
        .to_string_lossy()
        .into_owned()
}

/// Runs file system work off the async workers.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, ApiError> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| crate::Error::Task(e.to_string()))?
}

#[cfg(test)]
mod tests;
