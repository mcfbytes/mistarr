//! The launch routes of `docs/API.md` "Launching": a title or a bare core, through
//! MiSTer Main's command FIFO. Paths come from the database and the SD card only.

use std::path::{Component, Path as FsPath, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use mistarr_mister::launch::{self as mister, CommandSink};
use mistarr_mister::platforms::{self as table, Kind};
use serde::Serialize;
use tokio::sync::MutexGuard;

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
/// [`ApiError`] 404 for an unknown title; 409 when launching is off, another
/// launch was just sent (`busy`) or the title cannot start (not in the collection,
/// a disc with a track that is not `verified`, a BIOS entry, no core or MRA on the
/// card); 503 when MiSTer Main cannot take the command; 500 when the MGL cannot be written.
pub async fn launch_title(app: &AppState, id: TitleId) -> Result<Launched, ApiError> {
    let sink = sink(app)?;
    let mut last = exclusive(app).await?;
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
    let launched = blocking(move || {
        let launched = plan_title(&title, &paths.root, &paths.games, &dir)?;
        sink.send(&launched.1).map_err(command_error)?;
        Ok(launched.0)
    })
    .await?;
    *last = Some(Instant::now());
    Ok(launched)
}

/// Holds the launch lock for the whole plan and send, refusing with 409 `busy`
/// while the last launch is younger than `options.launch_gap`.
async fn exclusive(app: &AppState) -> Result<MutexGuard<'_, Option<Instant>>, ApiError> {
    let last = app.launch_lock.lock().await;
    if last.is_some_and(|t| t.elapsed() < app.options.launch_gap) {
        return Err(ApiError::busy(
            "a launch was just sent; wait a few seconds before the next",
        ));
    }
    Ok(last)
}

/// Starts the newest installed core of platform `id` with no game.
///
/// # Errors
///
/// [`ApiError`] 404 for an unknown platform, 409 when launching is off, another
/// launch was just sent (`busy`), no core for it is installed or it is the arcade
/// platform, 503 when MiSTer Main cannot take the command.
pub async fn launch_core(app: &AppState, id: &str) -> Result<Launched, ApiError> {
    let sink = sink(app)?;
    let mut last = exclusive(app).await?;
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
    let launched = blocking(move || {
        let core = mister::find_core(&root, row).ok_or_else(no_core)?;
        let line = mister::load_core(&core.path).map_err(command_error)?;
        sink.send(&line).map_err(command_error)?;
        tracing::info!(core = %core.path.display(), "core launched");
        Ok(Launched {
            core: relative(&core.path, &root),
            file: None,
        })
    })
    .await?;
    *last = Some(Instant::now());
    Ok(launched)
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
        let rel_path = FsPath::new(rel);
        if !rel_path
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
        {
            return Err(ApiError::conflict("the entry's MRA path leaves _Arcade"));
        }
        let mra = root.join("_Arcade").join(rel_path);
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
    if row.launch.is_empty() {
        return Err(ApiError::conflict("this entry has no MRA to start it from"));
    }
    if row.kind == Kind::Disc && !title.all_verified {
        return Err(ApiError::conflict(
            "every track of a disc must be verified before it is launched",
        ));
    }
    let core = mister::find_core(root, row).ok_or_else(no_core)?;
    let files: Vec<&str> = title.files.iter().map(String::as_str).collect();
    let file = mister::game_path(row.kind, games, &files)
        .ok_or_else(|| ApiError::conflict("the entry has no file the core can load"))?;
    let doc = mister::mgl(&core.mgl_rbf, core.slot, &games.join(&file)).map_err(command_error)?;
    let mgl = mister::write_mgl(dir, &doc).map_err(|e| {
        tracing::warn!(dir = %dir.display(), error = %e, "cannot write the launch MGL");
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "the launch file could not be written",
        )
    })?;
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
