//! Polling of a drop directory shared by the DAT and source watchers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use mistarr_sources::intake::StableFiles;

use super::{Job, Scheduler};
use crate::app::AppState;
use crate::db::ids::JobId;
use crate::db::jobs::JobState;
use crate::threads::Label;

/// Polls `dir` every `poll` on a blocking thread labelled `label` and enqueues
/// `make(path)` per stable file. A file whose job failed, or that could not be
/// queued, is reported and enqueued again on a later poll.
pub async fn run(
    app: Arc<AppState>,
    dir: PathBuf,
    mut files: StableFiles,
    poll: Duration,
    label: Label,
    make: fn(&Path) -> Arc<dyn Job>,
) {
    let mut pending: HashMap<PathBuf, JobId> = HashMap::new();
    let mut tick = tokio::time::interval(poll);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let mut finished = Vec::new();
        for (path, &id) in &pending {
            match app.db.read(move |c| crate::db::jobs::get(c, id)).await {
                Ok(Some(row)) if row.state == JobState::Failed => {
                    files.forget(path);
                    finished.push(path.clone());
                }
                Ok(Some(row)) if !row.state.is_finished() => {}
                Ok(_) => finished.push(path.clone()),
                Err(e) => tracing::warn!(error = %e, "cannot read import job"),
            }
        }
        for path in finished {
            pending.remove(&path);
        }
        let polled = dir.clone();
        let found;
        (files, found) = match crate::threads::run(label, move || {
            let found = files.poll(&polled);
            (files, found)
        })
        .await
        {
            Ok(polled) => polled,
            Err(e) => {
                tracing::error!(error = %e, "a drop directory watcher stopped");
                return;
            }
        };
        for path in found {
            match Scheduler::enqueue(&app, make(&path)).await {
                Ok(id) => {
                    pending.insert(path, id);
                }
                Err(e) => {
                    files.forget(&path);
                    tracing::warn!(error = %e, "cannot queue an import");
                }
            }
        }
    }
}
