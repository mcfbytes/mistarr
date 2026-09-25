//! Files in a watched directory that are not loaded yet; `docs/API.md` "Incoming files".

use std::path::Path;
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

/// Lists the files in `dir` and `dir/rejected/`, pending ones first by name,
/// then rejected ones newest first, matching each pending file to the open
/// `kind` job whose payload `path` names it.
///
/// # Errors
///
/// [`crate::Error::Db`] when the open jobs cannot be read.
pub async fn list(app: &AppState, dir: &Path, kind: &'static str) -> Result<Vec<IncomingFile>> {
    let open = app.db.read(jobs::open_rows).await?;
    let gate = app.gate.state();
    let dir = dir.to_path_buf();
    let listed = crate::threads::blocking(crate::threads::label::INCOMING, move || {
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
            let path = dir.join(&name).to_string_lossy().into_owned();
            let job = open.iter().find(|r| {
                r.kind == kind && r.payload.get("path").and_then(Value::as_str) == Some(&path)
            });
            pending_file(name, size, modified, job, &open, &gate)
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

/// A pending file's state from its open job, if any.
fn pending_file(
    file: String,
    size: u64,
    modified: i64,
    job: Option<&JobRow>,
    open: &[JobRow],
    gate: &GateState,
) -> IncomingFile {
    let Some(job) = job else {
        return IncomingFile {
            file,
            size,
            state: IncomingState::Waiting,
            reason: Some(SETTLING.to_owned()),
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

/// Why a queued job has not started: the job running on its lane, if any.
///
/// ```
/// use mistarr_server::db::jobs::{JobId, JobRow, JobState};
/// let row = |id, kind: &str, state, path: &str| JobRow { id: JobId(id), kind: kind.into(),
///     lane: "background".into(), payload: serde_json::json!({ "path": path }), state,
///     progress: None, created_at: 0, updated_at: 0 };
/// let open = [row(1, "dat_import", JobState::Running, "/d/a.dat"), row(2, "dat_import", JobState::Queued, "/d/b.dat")];
/// let why = mistarr_server::incoming::queued_reason(&open[1], &open);
/// assert_eq!(why, "Queued behind a.dat.");
/// ```
#[must_use]
pub fn queued_reason(job: &JobRow, open: &[JobRow]) -> String {
    let ahead = open
        .iter()
        .find(|r| r.lane == job.lane && r.id != job.id && r.state != JobState::Queued);
    match ahead {
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
        let f = pending_file("x".into(), 1, 0, Some(&row), &[], &gate);
        assert_eq!(f.state, IncomingState::Waiting);
        assert_eq!(f.reason.as_deref(), Some("Paused while NES is running"));
        let open = GateState::default();
        let f = pending_file(
            "x".into(),
            1,
            0,
            Some(&row),
            std::slice::from_ref(&row),
            &open,
        );
        assert_eq!(
            f.reason.as_deref(),
            Some("Queued behind other library work.")
        );
    }
}
