//! Files in a watched directory that are not loaded yet; `docs/API.md` "Incoming files".

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::UNIX_EPOCH;

use serde::Serialize;
use serde_json::Value;

use crate::app::AppState;
use crate::db::jobs::{self, JobId, JobRow, JobState};
use crate::error::Result;
use crate::jobs::gate::GateState;
use crate::jobs::Lane;
use crate::status::{hold_reason, job_detail};

/// Where an incoming file stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum IncomingState {
    /// Not picked up yet, or queued behind other work.
    Waiting,
    /// Its import job is running.
    Importing,
    /// Moved to `rejected/` with a reason.
    Rejected,
}

/// One file in a watched directory or its `rejected/` subdirectory.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IncomingFile {
    /// The file name.
    pub file: String,
    /// Size in bytes.
    pub size: u64,
    /// Where it stands.
    pub state: IncomingState,
    /// Why it waits or was rejected.
    pub reason: Option<String>,
    /// Its import job, while one is open.
    pub job_id: Option<JobId>,
    /// That job's last progress.
    pub progress: Option<Value>,
    /// Unix seconds of the file's last change.
    pub modified: i64,
}

/// Reason shown for a file the watcher has not queued yet.
pub const SETTLING: &str = "Waiting for the file to stop changing.";

/// How long an upload waits for its import job to be recorded before answering without it.
pub const QUEUE_WAIT: std::time::Duration = std::time::Duration::from_millis(250);

/// Files the server placed in a watched directory whose import job is still being
/// recorded; such a file needs no settling, so it never shows [`SETTLING`].
#[derive(Debug, Default)]
pub struct Placed {
    paths: Mutex<HashSet<PathBuf>>,
}

impl Placed {
    /// Whether `path` is waiting for its job to be recorded.
    #[must_use]
    pub fn contains(&self, path: &Path) -> bool {
        self.lock().contains(path)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashSet<PathBuf>> {
        self.paths.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Removes its path from [`Placed`] when dropped.
struct PlacedGuard {
    app: Arc<AppState>,
    path: PathBuf,
}

impl Drop for PlacedGuard {
    fn drop(&mut self) {
        self.app.placed.lock().remove(&self.path);
    }
}

/// Queues `job` for a file the server just placed at `path` and describes the file:
/// waiting with the reason, even when the writer is too busy to record the job
/// within [`QUEUE_WAIT`], in which case `job_id` is `None` and it is recorded later.
///
/// # Errors
///
/// [`crate::Error::Db`] when the job or the open jobs cannot be read or written.
pub async fn queue_placed(
    app: &Arc<AppState>,
    path: &Path,
    kind: &'static str,
    job: Arc<dyn crate::jobs::Job>,
) -> Result<IncomingFile> {
    app.placed.lock().insert(path.to_path_buf());
    let guard = PlacedGuard {
        app: Arc::clone(app),
        path: path.to_path_buf(),
    };
    crate::jobs::Scheduler::enqueue_within(app, job, QUEUE_WAIT, guard).await?;
    one(app, path, kind).await
}

/// Describes the pending file at `path` as [`list`] would.
///
/// # Errors
///
/// [`crate::Error::Db`] when the open jobs cannot be read.
pub async fn one(app: &AppState, path: &Path, kind: &'static str) -> Result<IncomingFile> {
    let mut open = app.db.read(jobs::open_rows).await?;
    app.live.overlay(&mut open);
    let gate = app.gate.state();
    let meta = tokio::fs::metadata(path).await.ok();
    let modified = meta
        .as_ref()
        .and_then(|m| m.modified().ok())
        .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let text = path.to_string_lossy();
    let job = open
        .iter()
        .find(|r| r.kind == kind && r.payload.get("path").and_then(Value::as_str) == Some(&text));
    let pending = Pending {
        size: meta.map_or(0, |m| m.len()),
        modified,
        placed: app.placed.contains(path),
    };
    Ok(pending_file(name, &pending, job, &open, &gate))
}

/// Lists the files in `dir` and `dir/rejected/`, pending ones first by name,
/// then rejected ones newest first, matching each pending file to the open
/// `kind` job whose payload `path` names it.
///
/// # Errors
///
/// [`crate::Error::Db`] when the open jobs cannot be read.
pub async fn list(app: &AppState, dir: &Path, kind: &'static str) -> Result<Vec<IncomingFile>> {
    let mut open = app.db.read(jobs::open_rows).await?;
    app.live.overlay(&mut open);
    let gate = app.gate.state();
    let dir = dir.to_path_buf();
    let listed = tokio::task::spawn_blocking(move || {
        let pending = files_in(&dir);
        let rejected = rejected_in(&dir.join(REJECTED_DIR));
        (dir, pending, rejected)
    })
    .await
    .map_err(|e| crate::Error::Task(e.to_string()))?;
    let (dir, pending, rejected) = listed;
    let mut out: Vec<IncomingFile> = pending
        .into_iter()
        .map(|(name, size, modified)| {
            let full = dir.join(&name);
            let path = full.to_string_lossy().into_owned();
            let job = open.iter().find(|r| {
                r.kind == kind && r.payload.get("path").and_then(Value::as_str) == Some(&path)
            });
            let pending = Pending {
                size,
                modified,
                placed: app.placed.contains(&full),
            };
            pending_file(name, &pending, job, &open, &gate)
        })
        .collect();
    out.extend(rejected);
    Ok(out)
}

/// The rejected files in `dir`, newest first, each with its `.reason.txt`.
fn rejected_in(dir: &Path) -> Vec<IncomingFile> {
    let mut files = files_in(dir);
    files.retain(|(name, _, _)| !name.ends_with(REASON_SUFFIX));
    files.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
    files
        .into_iter()
        .map(|(name, size, modified)| {
            let sidecar = dir.join(format!("{name}{REASON_SUFFIX}"));
            let reason = std::fs::read_to_string(sidecar)
                .ok()
                .map(|t| t.trim().to_owned())
                .filter(|t| !t.is_empty());
            IncomingFile {
                file: name,
                size,
                state: IncomingState::Rejected,
                reason,
                job_id: None,
                progress: None,
                modified,
            }
        })
        .collect()
}

/// The directory beside the dropped files that holds rejected ones.
pub const REJECTED_DIR: &str = "rejected";
/// Suffix of the file beside a rejected one that says why.
pub const REASON_SUFFIX: &str = ".reason.txt";

/// What is known of a pending file besides its name and job.
struct Pending {
    size: u64,
    modified: i64,
    /// Placed by the server, its job not recorded yet.
    placed: bool,
}

/// A pending file's state from its open job, if any.
fn pending_file(
    file: String,
    pending: &Pending,
    job: Option<&JobRow>,
    open: &[JobRow],
    gate: &GateState,
) -> IncomingFile {
    let Pending {
        size,
        modified,
        placed,
    } = *pending;
    let Some(job) = job else {
        let reason = if placed {
            writer_reason(open)
        } else {
            SETTLING.to_owned()
        };
        return IncomingFile {
            file,
            size,
            state: IncomingState::Waiting,
            reason: Some(reason),
            job_id: None,
            progress: None,
            modified,
        };
    };
    let (state, reason) = match job.state {
        JobState::Running => (IncomingState::Importing, None),
        JobState::Paused => (
            IncomingState::Importing,
            hold_reason(gate, &job.lane, job.state),
        ),
        _ => (
            IncomingState::Waiting,
            hold_reason(gate, &job.lane, job.state).or_else(|| Some(queued_reason(job, open))),
        ),
    };
    IncomingFile {
        file,
        size,
        state,
        reason,
        job_id: Some(job.id),
        progress: job.progress.clone(),
        modified,
    }
}

/// Why a job cannot be recorded yet: the DAT import holding the writer, if one runs.
///
/// ```
/// use mistarr_server::db::jobs::{JobId, JobRow, JobState};
/// let row = JobRow { id: JobId(1), kind: "dat_import".into(), lane: "background".into(),
///     payload: serde_json::json!({ "path": "/d/a.dat" }), state: JobState::Running,
///     progress: None, created_at: 0, updated_at: 0 };
/// let why = mistarr_server::incoming::writer_reason(&[row]);
/// assert_eq!(why, "Waiting for the DAT import of a.dat to finish.");
/// assert_eq!(mistarr_server::incoming::writer_reason(&[]), "Waiting to be queued.");
/// ```
#[must_use]
pub fn writer_reason(open: &[JobRow]) -> String {
    open.iter()
        .find(|r| r.state == JobState::Running && r.kind == crate::jobs::dat_import::KIND)
        .map_or_else(|| "Waiting to be queued.".to_owned(), dat_wait)
}

fn dat_wait(row: &JobRow) -> String {
    match job_detail(&row.payload) {
        Some(file) => format!("Waiting for the DAT import of {file} to finish."),
        None => "Waiting for the DAT import to finish.".to_owned(),
    }
}

/// Why a queued job has not started: the job running on its lane, if any; a DAT
/// import ahead of other work is named as such.
///
/// ```
/// use mistarr_server::db::jobs::{JobId, JobRow, JobState};
/// let row = |id, kind: &str, state, path: &str| JobRow { id: JobId(id), kind: kind.into(),
///     lane: "background".into(), payload: serde_json::json!({ "path": path }), state,
///     progress: None, created_at: 0, updated_at: 0 };
/// let open = [row(1, "dat_import", JobState::Running, "/d/a.dat"), row(2, "dat_import", JobState::Queued, "/d/b.dat")];
/// let why = mistarr_server::incoming::queued_reason(&open[1], &open);
/// assert_eq!(why, "Queued behind a.dat.");
/// let torrent = row(3, "source_import", JobState::Queued, "/s/b.torrent");
/// let why = mistarr_server::incoming::queued_reason(&torrent, &open);
/// assert_eq!(why, "Waiting for the DAT import of a.dat to finish.");
/// ```
#[must_use]
pub fn queued_reason(job: &JobRow, open: &[JobRow]) -> String {
    let ahead = open
        .iter()
        .find(|r| r.lane == job.lane && r.id != job.id && r.state != JobState::Queued);
    match ahead {
        Some(r) if r.kind == crate::jobs::dat_import::KIND && job.kind != r.kind => dat_wait(r),
        Some(r) => format!(
            "Queued behind {}.",
            job_detail(&r.payload).unwrap_or_else(|| r.kind.replace('_', " "))
        ),
        None if job.lane == Lane::Heavy.as_str() => "Queued behind other library work.".to_owned(),
        None => "Queued.".to_owned(),
    }
}

/// `(name, size, mtime)` of the regular, non-hidden files directly in `dir`, by name.
fn files_in(dir: &Path) -> Vec<(String, u64, i64)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<_> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let meta = e.metadata().ok()?;
            if name.starts_with('.') || !meta.is_file() {
                return None;
            }
            let modified = meta
                .modified()
                .ok()
                .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
                .and_then(|d| i64::try_from(d.as_secs()).ok())
                .unwrap_or(0);
            Some((name, meta.len(), modified))
        })
        .collect();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::state;
    use serde_json::json;

    #[tokio::test]
    async fn pending_and_rejected_files_carry_their_state() {
        let (dir, app) = state();
        let dats = dir.path().join("dats");
        std::fs::create_dir_all(dats.join(REJECTED_DIR)).expect("mkdir");
        std::fs::write(dats.join("a.dat"), b"x").expect("write");
        std::fs::write(dats.join("b.dat"), b"yy").expect("write");
        std::fs::write(dats.join(".c.part"), b"").expect("write");
        std::fs::write(dats.join("rejected/bad.dat"), b"z").expect("write");
        std::fs::write(dats.join("rejected/bad.dat.reason.txt"), "not a DAT\n").expect("write");
        let a = dats.join("a.dat").to_string_lossy().into_owned();
        app.db
            .write(move |c| {
                let id = jobs::insert(c, "dat_import", &json!({ "path": a }), "background", 1)?;
                jobs::set_state(c, id, JobState::Running, 1)?;
                jobs::set_progress(c, id, &json!({ "games": 3 }), 1)
            })
            .await
            .expect("seed");
        let items = list(&app, &dats, "dat_import").await.expect("list");
        let names: Vec<_> = items.iter().map(|i| i.file.as_str()).collect();
        assert_eq!(names, ["a.dat", "b.dat", "bad.dat"]);
        assert_eq!(items[0].state, IncomingState::Importing);
        assert_eq!(items[0].progress, Some(json!({ "games": 3 })));
        assert_eq!(items[1].state, IncomingState::Waiting);
        assert_eq!(items[1].reason.as_deref(), Some(SETTLING));
        assert_eq!(items[1].size, 2);
        assert_eq!(items[2].state, IncomingState::Rejected);
        assert_eq!(items[2].reason.as_deref(), Some("not a DAT"));
        assert!(list(&app, &dir.path().join("none"), "dat_import")
            .await
            .expect("list")
            .is_empty());
    }

    #[test]
    fn a_held_heavy_job_says_so() {
        let row = JobRow {
            id: JobId(4),
            kind: "scan".into(),
            lane: "heavy".into(),
            payload: json!({ "path": "/d/x" }),
            state: JobState::Queued,
            progress: None,
            created_at: 0,
            updated_at: 0,
        };
        let gate = GateState {
            corename: Some("NES".into()),
            manual: None,
        };
        let pending = Pending {
            size: 1,
            modified: 0,
            placed: false,
        };
        let f = pending_file("x".into(), &pending, Some(&row), &[], &gate);
        assert_eq!(f.state, IncomingState::Waiting);
        assert_eq!(f.reason.as_deref(), Some("Paused while NES is running"));
        let open = GateState::default();
        let f = pending_file(
            "x".into(),
            &pending,
            Some(&row),
            std::slice::from_ref(&row),
            &open,
        );
        assert_eq!(
            f.reason.as_deref(),
            Some("Queued behind other library work.")
        );
    }

    struct Noop;

    #[async_trait::async_trait]
    impl crate::jobs::Job for Noop {
        fn kind(&self) -> &'static str {
            "source_import"
        }
        fn payload(&self) -> Value {
            json!({ "path": "/nowhere/x.torrent" })
        }
        async fn run(&self, _ctx: &crate::jobs::JobContext) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_placed_file_waits_for_the_writer_not_for_settling() {
        let (dir, app) = state();
        let path = dir.path().join("x.torrent");
        std::fs::write(&path, b"d1:ae").expect("write");
        let dat = json!({ "path": "/d/big.dat" });
        app.db
            .write(move |c| {
                let id = jobs::insert(c, "dat_import", &dat, "background", 1)?;
                jobs::set_state(c, id, JobState::Running, 1)
            })
            .await
            .expect("seed");
        let (held, is_held) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let db = app.db.clone();
        let holder = std::thread::spawn(move || {
            db.write_blocking(|c| {
                let tx = c.transaction()?;
                let _ = held.send(());
                let _ = released.recv();
                crate::db::commit(tx)
            })
        });
        is_held.recv().expect("held");
        let started = std::time::Instant::now();
        let f = queue_placed(&app, &path, "source_import", Arc::new(Noop))
            .await
            .expect("queued");
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert_eq!(f.job_id, None);
        assert_eq!(f.state, IncomingState::Waiting);
        assert_eq!(
            f.reason.as_deref(),
            Some("Waiting for the DAT import of big.dat to finish.")
        );
        assert!(app.placed.contains(&path));
        release.send(()).expect("release");
        holder.join().expect("join").expect("write");
        for _ in 0..200 {
            if !app.placed.contains(&path) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(!app.placed.contains(&path), "forgotten once recorded");
        assert_eq!(writer_reason(&[]), "Waiting to be queued.");
    }
}
