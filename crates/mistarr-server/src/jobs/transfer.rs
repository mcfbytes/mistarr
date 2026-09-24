//! Moves queued downloads into the client; the flow is `docs/ARCHITECTURE.md` "Wanted and transfer".

use std::sync::Arc;

use async_trait::async_trait;
use mistarr_clients::{
    ClientError, ClientTorrentId, DownloadClient, InfoHash, SeedPolicy, TorrentSource,
};
use mistarr_sources::torrent;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::broadcast::error::RecvError;

use super::{Job, JobContext, Scheduler};
use crate::app::AppState;
use crate::db::downloads::{self as rows, DownloadId, DownloadRow, DownloadState};
use crate::db::sources::{self, SourceId, SourceRow};
use crate::error::Result;
use crate::events::EventKind;

/// The `jobs.kind` of [`Transfer`].
pub const KIND: &str = "transfer";
/// The `jobs.kind` of [`Deselect`].
pub const DESELECT_KIND: &str = "deselect";

/// Error stored on downloads whose `.torrent` is gone from `sources/loaded/`.
pub const MISSING_TORRENT: &str =
    "The .torrent file for this source is no longer in sources/loaded/. Add the source again.";
/// Error stored on downloads whose magnet the client no longer has.
pub const LOST_MAGNET: &str =
    "The download client no longer has this magnet. Remove the source and add it again.";

/// The `download.changed` event body.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct DownloadChanged {
    /// The download.
    pub download_id: DownloadId,
    /// Its state now.
    pub state: DownloadState,
    /// Its progress, 0 to 1.
    pub progress: f64,
}

/// Publishes `download.changed`.
pub fn publish(app: &AppState, download_id: DownloadId, state: DownloadState, progress: f64) {
    let body = DownloadChanged {
        download_id,
        state,
        progress,
    };
    app.events.publish(EventKind::DownloadChanged, &body);
}

/// Publishes `download.changed` for each of `ids` as stored now.
///
/// # Errors
///
/// [`crate::Error::Db`] when the rows cannot be read.
pub async fn publish_ids(app: &AppState, ids: Vec<DownloadId>) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let found: Vec<DownloadRow> = app
        .db
        .read(move |c| {
            let mut out = Vec::with_capacity(ids.len());
            for id in ids {
                out.extend(rows::get(c, id)?);
            }
            Ok(out)
        })
        .await?;
    for r in found {
        publish(app, r.id, r.state, r.progress);
    }
    Ok(())
}

/// Chooses files for `wanted` downloads, then, per source with `queued`
/// downloads, adds the torrent paused with the selected files and its seed
/// policy, or extends the selection when the client has it, and starts it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Transfer;

/// Re-applies a source's selection after downloads left it, stopping the
/// torrent when nothing of it is selected any more.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deselect {
    /// The source.
    pub source_id: SourceId,
}

/// Whether to carry on with the next source.
enum Flow {
    Next,
    Stop,
}

#[async_trait]
impl Job for Transfer {
    fn kind(&self) -> &'static str {
        KIND
    }

    async fn run(&self, ctx: &JobContext) -> Result<()> {
        let app = &ctx.app;
        let promoted = app
            .db
            .write(|c| rows::promote_wanted(c, crate::unix_now()))
            .await?;
        publish_ids(app, promoted).await?;
        let Some(client) = app.client() else {
            return Ok(());
        };
        let queued = app.db.read(rows::queued_sources).await?;
        let mut started = 0usize;
        for source in queued {
            ctx.checkpoint().await?;
            match start_source(app, client.as_ref(), source).await? {
                (Flow::Next, n) => started += n,
                (Flow::Stop, n) => {
                    started += n;
                    break;
                }
            }
        }
        if started > 0 {
            ctx.progress(json!({ "started": started })).await?;
            app.poll_wake.notify_one();
        }
        Ok(())
    }
}

/// Selects and starts one source's queued downloads; returns how many started.
async fn start_source(
    app: &AppState,
    client: &dyn DownloadClient,
    source: SourceId,
) -> Result<(Flow, usize)> {
    let (row, queued, wanted) = app
        .db
        .read(move |c| {
            Ok((
                sources::get(c, source)?,
                rows::of_source(c, source, &[DownloadState::Queued])?,
                rows::selected_indices(c, source)?,
            ))
        })
        .await?;
    let Some(row) = row else {
        return Ok((Flow::Next, 0));
    };
    let ids: Vec<DownloadId> = queued.iter().map(|d| d.id).collect();
    if ids.is_empty() {
        return Ok((Flow::Next, 0));
    }
    let existing = row.client_id.as_deref().map(ClientTorrentId::new);
    let torrent = match existing {
        Some(id) => match client.set_wanted(&id, &wanted).await {
            Ok(()) => id,
            Err(ClientError::MetadataPending) => return Ok((Flow::Next, 0)),
            Err(ClientError::NotFound) => {
                app.db
                    .write(move |c| sources::set_client_id(c, source, None))
                    .await?;
                match add(app, client, &row, &wanted, &ids).await? {
                    Some(id) => id,
                    None => return Ok((Flow::Next, 0)),
                }
            }
            Err(e) => return unanswered(app, &ids, &e).await,
        },
        None => match add(app, client, &row, &wanted, &ids).await? {
            Some(id) => id,
            None => return Ok((Flow::Next, 0)),
        },
    };
    if let Err(e) = client.start(&torrent).await {
        tracing::warn!(source = %source, error = %e, "cannot start the torrent");
        return Ok((Flow::Stop, 0));
    }
    let moved = app
        .db
        .write(move |c| {
            rows::move_all(
                c,
                &ids,
                DownloadState::Transferring,
                None,
                crate::unix_now(),
            )
        })
        .await?;
    let n = moved.len();
    publish_ids(app, moved).await?;
    Ok((Flow::Next, n))
}

/// Fails the downloads for an error the client will repeat, else keeps them
/// queued and stops this pass.
async fn unanswered(app: &AppState, ids: &[DownloadId], e: &ClientError) -> Result<(Flow, usize)> {
    if let ClientError::FileIndex { .. } = e {
        let text = format!("The download client does not list this file: {e}.");
        fail(app, ids, &text).await?;
        return Ok((Flow::Next, 0));
    }
    tracing::warn!(error = %e, "the download client did not take the selection");
    Ok((Flow::Stop, 0))
}

async fn fail(app: &AppState, ids: &[DownloadId], error: &str) -> Result<()> {
    let (ids, error) = (ids.to_vec(), error.to_owned());
    let moved = app
        .db
        .write(move |c| {
            rows::move_all(
                c,
                &ids,
                DownloadState::Failed,
                Some(&error),
                crate::unix_now(),
            )
        })
        .await?;
    publish_ids(app, moved).await
}

/// Adds the source's torrent paused into `staging/<infohash>/`, mapped for the
/// client, and records its id. `None` when the downloads were failed or must wait.
async fn add(
    app: &AppState,
    client: &dyn DownloadClient,
    row: &SourceRow,
    wanted: &[u32],
    ids: &[DownloadId],
) -> Result<Option<ClientTorrentId>> {
    let Some(bytes) = metainfo(app, row).await else {
        let why = if row.origin_file.ends_with(".magnet") {
            LOST_MAGNET
        } else {
            MISSING_TORRENT
        };
        fail(app, ids, why).await?;
        return Ok(None);
    };
    let config = app.config();
    let local = config.paths.staging().join(&row.infohash);
    let dir = crate::client::to_remote(&config.client.remote_path_map, &local);
    let seed = sources::seed_from_text(&row.seed_policy).unwrap_or(SeedPolicy::None);
    match client
        .add(TorrentSource::Metainfo(bytes), &dir, wanted, seed)
        .await
    {
        Ok(id) => {
            let (source, stored) = (row.id, id.as_str().to_owned());
            app.db
                .write(move |c| sources::set_client_id(c, source, Some(&stored)))
                .await?;
            Ok(Some(id))
        }
        Err(e) => {
            unanswered(app, ids, &e).await?;
            Ok(None)
        }
    }
}

/// The source's `.torrent` from `sources/loaded/`, when it is still there
/// and still holds this source.
async fn metainfo(app: &AppState, row: &SourceRow) -> Option<Vec<u8>> {
    if !row.origin_file.ends_with(".torrent") {
        return None;
    }
    let path = app
        .config()
        .paths
        .sources()
        .join(mistarr_sources::watch::LOADED_DIR)
        .join(&row.origin_file);
    let bytes = tokio::fs::read(&path).await.ok()?;
    let meta = torrent::parse_torrent(&bytes).ok()?;
    (InfoHash::from_bytes(meta.infohash).to_string() == row.infohash).then_some(bytes)
}

#[async_trait]
impl Job for Deselect {
    fn kind(&self) -> &'static str {
        DESELECT_KIND
    }

    fn payload(&self) -> Value {
        json!({ "source_id": self.source_id })
    }

    async fn run(&self, ctx: &JobContext) -> Result<()> {
        let source = self.source_id;
        let (row, wanted) = ctx
            .app
            .db
            .read(move |c| Ok((sources::get(c, source)?, rows::selected_indices(c, source)?)))
            .await?;
        let (Some(cid), Some(client)) = (row.and_then(|r| r.client_id), ctx.app.client()) else {
            return Ok(());
        };
        let id = ClientTorrentId::new(cid);
        if wanted.is_empty() {
            match client.stop(&id).await {
                Ok(()) | Err(ClientError::NotFound) => {}
                Err(e) => return Err(crate::Error::Job(e.to_string())),
            }
        }
        match client.set_wanted(&id, &wanted).await {
            Ok(()) | Err(ClientError::NotFound | ClientError::MetadataPending) => Ok(()),
            Err(e) => Err(crate::Error::Job(e.to_string())),
        }
    }
}

/// Queues a [`Transfer`], logging when the scheduler has stopped.
pub async fn kick(app: &Arc<AppState>) {
    if let Err(e) = Scheduler::enqueue(app, Arc::new(Transfer)).await {
        tracing::warn!(error = %e, "cannot queue a transfer");
    }
}

/// Queues a [`Transfer`] whenever a source changes, so `wanted` downloads
/// find a file as soon as a source binds.
pub async fn watch(app: Arc<AppState>) {
    let mut live = app.events.subscribe(None).live;
    loop {
        match live.recv().await {
            Ok(ev) if ev.kind == EventKind::SourceChanged => kick(&app).await,
            Ok(_) => {}
            Err(RecvError::Lagged(_)) => kick(&app).await,
            Err(RecvError::Closed) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::state;

    #[tokio::test]
    async fn without_a_client_queued_downloads_wait() {
        let (_dir, app) = state();
        let mut events = app.events.subscribe(None).live;
        Scheduler::run_inline(&app, Arc::new(Transfer))
            .await
            .expect("run");
        let (_, total) = app
            .db
            .read(|c| rows::list(c, &[], 10, 0))
            .await
            .expect("list");
        assert_eq!(total, 0);
        let ev = events.recv().await.expect("job event");
        assert_eq!(ev.kind, EventKind::JobProgress);
        assert!(ev.data.contains(r#""kind":"transfer""#));
    }

    #[tokio::test]
    async fn deselect_without_a_torrent_is_a_no_op() {
        let (_dir, app) = state();
        let job = Deselect {
            source_id: SourceId(9),
        };
        assert_eq!(job.payload(), json!({ "source_id": 9 }));
        Scheduler::run_inline(&app, Arc::new(job))
            .await
            .expect("run");
    }

    #[test]
    fn change_events_carry_state_and_progress() {
        let (_dir, app) = state();
        let mut live = app.events.subscribe(None).live;
        publish(&app, DownloadId(3), DownloadState::Checking, 1.0);
        let ev = live.try_recv().expect("event");
        assert_eq!(ev.kind, EventKind::DownloadChanged);
        assert_eq!(
            ev.data,
            r#"{"download_id":3,"state":"checking","progress":1.0}"#
        );
    }
}
