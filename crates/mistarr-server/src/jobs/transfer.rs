//! Moves queued downloads into the client; the flow is `docs/ARCHITECTURE.md` "Wanted and transfer".

use std::sync::Arc;

use async_trait::async_trait;
use mistarr_clients::{ClientError, ClientTorrentId, DownloadClient, SeedPolicy, TorrentSource};
use mistarr_sources::torrent;
use serde_json::{json, Value};
use tokio::sync::broadcast::error::RecvError;

use super::{Job, JobContext, JobKind, Scheduler};
use crate::app::AppState;
use crate::db::deferred::Op;
use crate::db::downloads::{self as rows, Cancelled, DownloadRow, DownloadState};
use crate::db::ids::DownloadId;
use crate::db::ids::SourceId;
use crate::db::sources::{self, SourceRow};
use crate::error::Result;
use crate::events::{DownloadChanged, Event, EventKind};

/// Error stored on downloads whose `.torrent` is gone from `sources/loaded/`.
pub const MISSING_TORRENT: &str =
    "The .torrent file for this source is no longer in sources/loaded/. Add the source again.";
/// Error stored on downloads whose magnet the client no longer has.
pub const LOST_MAGNET: &str =
    "The download client no longer has this magnet. Remove the source and add it again.";

/// Publishes `download.changed`.
pub fn publish(app: &AppState, download_id: DownloadId, state: DownloadState, progress: f64) {
    app.events.publish(&Event::DownloadChanged(DownloadChanged {
        download_id,
        state,
        progress,
    }));
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
/// torrent when nothing of it is selected any more. Its payload is the struct itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    fn kind(&self) -> JobKind {
        JobKind::Transfer
    }

    async fn run(&self, ctx: &JobContext) -> Result<()> {
        let app = &ctx.app;
        let promoted = app
            .db
            .write(|c| rows::promote_wanted(c, crate::unix_now()))
            .await?;
        publish_ids(app, promoted).await?;
        let Some(client) = app.client.get() else {
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
    app: &Arc<AppState>,
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
    let torrent = match row.client_id {
        Some(id) => match client.set_wanted(&id, &wanted).await {
            Ok(()) => id,
            Err(ClientError::MetadataPending) => return Ok((Flow::Next, 0)),
            Err(ClientError::NotFound) => {
                app.db
                    .write(move |c| sources::set_client_id(c, source, None))
                    .await?;
                match add(app, client, &row, &wanted, &ids).await? {
                    Ok(id) => id,
                    Err(flow) => return Ok((flow, 0)),
                }
            }
            Err(e) => return unanswered(app, &ids, &e).await,
        },
        None => match add(app, client, &row, &wanted, &ids).await? {
            Ok(id) => id,
            Err(flow) => return Ok((flow, 0)),
        },
    };
    if let Err(e) = client.start(&torrent).await {
        tracing::warn!(source = %source, error = %e, "cannot start the torrent");
        return Ok((Flow::Stop, 0));
    }
    let expected = ids.len();
    // Moving checks each row is still queued, so a row cancelled meanwhile stays cancelled.
    let n = move_downloads(app, &ids, DownloadState::Transferring, None)
        .await?
        .len();
    if n < expected {
        let job = Arc::new(Deselect { source_id: source });
        Scheduler::enqueue(app, job).await?;
    }
    Ok((Flow::Next, n))
}

/// Fails the downloads for an error the client will repeat, else keeps them
/// queued and stops this pass.
async fn unanswered(app: &AppState, ids: &[DownloadId], e: &ClientError) -> Result<(Flow, usize)> {
    if let ClientError::FileIndex { .. } = e {
        let text = format!("The download client does not list this file: {e}.");
        move_downloads(app, ids, DownloadState::Failed, Some(&text)).await?;
        return Ok((Flow::Next, 0));
    }
    tracing::warn!(error = %e, "the download client did not take the selection");
    Ok((Flow::Stop, 0))
}

/// Moves those of `ids` that may go to `to`, storing `reason` as their error when
/// given, publishes `download.changed` for each that moved and returns them.
///
/// # Errors
///
/// [`crate::Error::Db`] when the rows cannot be written or read back.
pub async fn move_downloads(
    app: &AppState,
    ids: &[DownloadId],
    to: DownloadState,
    reason: Option<&str>,
) -> Result<Vec<DownloadId>> {
    let (ids, reason) = (ids.to_vec(), reason.map(str::to_owned));
    let moved = app
        .db
        .write(move |c| rows::move_all(c, &ids, to, reason.as_deref(), crate::unix_now()))
        .await?;
    publish_ids(app, moved.clone()).await?;
    Ok(moved)
}

/// Adds the source's torrent paused into `staging/<infohash>/`, which the
/// client maps to its own path, and records its id. Otherwise the downloads
/// were failed or must wait, and the flow says whether the pass carries on.
async fn add(
    app: &AppState,
    client: &dyn DownloadClient,
    row: &SourceRow,
    wanted: &[u32],
    ids: &[DownloadId],
) -> Result<std::result::Result<ClientTorrentId, Flow>> {
    let Some(src) = metainfo(app, row).await else {
        let why = if row.origin_file.ends_with(".magnet") {
            LOST_MAGNET
        } else {
            MISSING_TORRENT
        };
        move_downloads(app, ids, DownloadState::Failed, Some(why)).await?;
        return Ok(Err(Flow::Next));
    };
    let local = app.config().paths.staging().join(&row.infohash);
    crate::client::prepare_download_dir(&local).await;
    let seed = sources::seed_from_text(&row.seed_policy).unwrap_or(SeedPolicy::None);
    match client.add(src, &local, wanted, seed).await {
        Ok(id) => {
            let source = row.id;
            app.db
                .write(move |c| sources::set_client_id(c, source, Some(id)))
                .await?;
            Ok(Ok(id))
        }
        Err(e) => Ok(Err(unanswered(app, ids, &e).await?.0)),
    }
}

/// The source's `.torrent` from `sources/loaded/`, when it is still there
/// and still holds this source, with the file count its import recorded.
async fn metainfo(app: &AppState, row: &SourceRow) -> Option<TorrentSource> {
    if !row.origin_file.ends_with(".torrent") {
        return None;
    }
    let path = app
        .config()
        .paths
        .sources()
        .join(mistarr_sources::intake::LOADED_DIR)
        .join(&row.origin_file);
    let bytes = tokio::fs::read(&path).await.ok()?;
    let infohash = torrent::infohash(&bytes).ok()?;
    let file_count = usize::try_from(row.file_count).ok()?;
    (infohash.to_string() == row.infohash).then_some(TorrentSource::Metainfo {
        bytes,
        infohash,
        file_count,
    })
}

#[async_trait]
impl Job for Deselect {
    fn kind(&self) -> JobKind {
        JobKind::Deselect
    }

    fn payload(&self) -> Value {
        super::to_payload(self)
    }

    async fn run(&self, ctx: &JobContext) -> Result<()> {
        deselect(&ctx.app, self.source_id).await.map(drop)
    }
}

/// Applies a source's selection in the client, stopping its torrent when
/// nothing is selected. True once applied, or when the source has no torrent;
/// false when the client is frozen or missing and the core gate keeps the work
/// for later, as it does when the client refuses it.
///
/// # Errors
///
/// [`crate::Error::Client`] when the client refuses; [`crate::Error::Db`] on database failure.
pub async fn deselect(app: &AppState, source: SourceId) -> Result<bool> {
    let (row, wanted) = app
        .db
        .read(move |c| Ok((sources::get(c, source)?, rows::selected_indices(c, source)?)))
        .await?;
    let Some(id) = row.and_then(|r| r.client_id) else {
        return Ok(true);
    };
    let Some(client) = app.client.get() else {
        crate::jobs::watch::core_limits::defer(app, Op::Deselect(source)).await;
        return Ok(false);
    };
    if wanted.is_empty() {
        match client.stop(&id).await {
            Ok(()) | Err(ClientError::NotFound) => {}
            Err(e) => {
                crate::jobs::watch::core_limits::defer(app, Op::Deselect(source)).await;
                return Err(e.into());
            }
        }
    }
    match client.set_wanted(&id, &wanted).await {
        Ok(()) | Err(ClientError::NotFound | ClientError::MetadataPending) => Ok(true),
        Err(e) => {
            crate::jobs::watch::core_limits::defer(app, Op::Deselect(source)).await;
            Err(e.into())
        }
    }
}

/// Announces cancelled downloads and, for those the client had started,
/// queues a [`Deselect`] per source so the client stops fetching them.
pub async fn after_cancel(app: &Arc<AppState>, cancelled: &[Cancelled]) {
    let ids = cancelled.iter().map(|c| c.id).collect();
    if let Err(e) = publish_ids(app, ids).await {
        tracing::warn!(error = %e, "cannot announce cancelled downloads");
    }
    let started: std::collections::BTreeSet<SourceId> = cancelled
        .iter()
        .filter(|c| c.started)
        .filter_map(|c| c.source_id)
        .collect();
    for source_id in started {
        Scheduler::submit(app, Arc::new(Deselect { source_id })).await;
    }
}

/// Queues a [`Transfer`] whenever a source changes, so `wanted` downloads
/// find a file as soon as a source binds.
pub async fn watch(app: Arc<AppState>) {
    let mut live = app.events.subscribe(None).live;
    loop {
        match live.recv().await {
            Ok(ev) if ev.kind == EventKind::SourceChanged => {
                Scheduler::submit(&app, Arc::new(Transfer)).await;
            }
            Ok(_) => {}
            Err(RecvError::Lagged(_)) => {
                Scheduler::submit(&app, Arc::new(Transfer)).await;
            }
            Err(RecvError::Closed) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::state;
    use crate::db::downloads::{CancelOutcome, Candidate, NewDownload};
    use crate::db::fixtures::{pid, seed_rom};
    use crate::db::ids::TitleId;
    use crate::db::sources::{NewSource, SourceState};
    use mistarr_clients::{ClientFile, ClientInfo, TorrentStatus};
    use mistarr_sources::binding::Confidence;
    use std::path::Path;
    use std::sync::Mutex;
    use std::time::Duration;
    use tokio::sync::Notify;

    /// An in-process client that records calls; `add` can wait for a release or fail.
    #[derive(Default)]
    struct Mock {
        calls: Mutex<Vec<String>>,
        add_entered: Notify,
        release: Option<Notify>,
        unreachable: bool,
    }

    impl Mock {
        fn log(&self, call: String) {
            self.calls.lock().expect("lock").push(call);
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().expect("lock").clone()
        }
    }

    fn nope<T>() -> mistarr_clients::Result<T> {
        Err(ClientError::Protocol("not scripted".into()))
    }

    #[async_trait]
    impl DownloadClient for Mock {
        async fn probe(&self) -> mistarr_clients::Result<ClientInfo> {
            nope()
        }
        async fn add(
            &self,
            _src: TorrentSource,
            _dir: &Path,
            wanted: &[u32],
            _seed: SeedPolicy,
        ) -> mistarr_clients::Result<ClientTorrentId> {
            self.log(format!("add:{wanted:?}"));
            self.add_entered.notify_one();
            if let Some(release) = &self.release {
                release.notified().await;
            }
            if self.unreachable {
                return Err(ClientError::Unreachable("down".into()));
            }
            Ok(ClientTorrentId::new(mistarr_core::InfoHash::from_bytes(
                [7; 20],
            )))
        }
        async fn set_wanted(
            &self,
            _id: &ClientTorrentId,
            wanted: &[u32],
        ) -> mistarr_clients::Result<()> {
            self.log(format!("set_wanted:{wanted:?}"));
            Ok(())
        }
        async fn set_seed_policy(
            &self,
            _id: &ClientTorrentId,
            _seed: SeedPolicy,
        ) -> mistarr_clients::Result<()> {
            Ok(())
        }
        async fn start(&self, _id: &ClientTorrentId) -> mistarr_clients::Result<()> {
            self.log("start".into());
            Ok(())
        }
        async fn stop(&self, _id: &ClientTorrentId) -> mistarr_clients::Result<()> {
            self.log("stop".into());
            Ok(())
        }
        async fn status(&self, _id: &ClientTorrentId) -> mistarr_clients::Result<TorrentStatus> {
            nope()
        }
        async fn files(&self, _id: &ClientTorrentId) -> mistarr_clients::Result<Vec<ClientFile>> {
            nope()
        }
        async fn remove(&self, _id: &ClientTorrentId, _data: bool) -> mistarr_clients::Result<()> {
            nope()
        }
        async fn rate_limit(
            &self,
            _dir: mistarr_clients::Direction,
        ) -> mistarr_clients::Result<mistarr_clients::RateLimit> {
            nope()
        }
        async fn set_rate_limit(
            &self,
            _dir: mistarr_clients::Direction,
            _limit: mistarr_clients::RateLimit,
        ) -> mistarr_clients::Result<()> {
            nope()
        }
        async fn process_id(&self) -> mistarr_clients::Result<Option<u32>> {
            nope()
        }
    }

    /// A bound single-file source in `sources/loaded/` with one queued download.
    fn seed(app: &AppState, byte: u8) -> (SourceId, DownloadId) {
        let name = format!("Example Quest {byte} (USA).nes");
        let bytes = format!(
            "d4:infod6:lengthi16e4:name{}:{name}12:piece lengthi16384e6:pieces0:ee",
            name.len()
        )
        .into_bytes();
        let meta = torrent::parse_torrent(&bytes).expect("torrent");
        let hash = meta.infohash.to_string();
        let origin = format!("s{byte}.torrent");
        let loaded = app
            .config()
            .paths
            .sources()
            .join(mistarr_sources::intake::LOADED_DIR);
        std::fs::create_dir_all(&loaded).expect("mkdir");
        std::fs::write(loaded.join(&origin), &bytes).expect("write");
        app.db
            .write_blocking(|c| {
                let rom = seed_rom(c, &pid("nes"), &name, 16, &[])?;
                let title: i64 =
                    c.query_row("SELECT title_id FROM roms WHERE id = ?1", [rom], |r| {
                        r.get(0)
                    })?;
                let source = sources::insert(
                    c,
                    &NewSource {
                        infohash: &hash,
                        display_name: &name,
                        origin_file: &origin,
                        state: SourceState::Bound,
                        reason: None,
                        added_at: 0,
                    },
                )?;
                sources::replace_files(c, source, &meta.files)?;
                sources::set_matches(c, source, &[(0, Some(rom), Confidence::Name)])?;
                let id = rows::create(
                    c,
                    &NewDownload {
                        title_id: TitleId::new(title),
                        rom_id: rom,
                        file: Some(Candidate {
                            source_id: source,
                            file_index: 0,
                        }),
                        now: 0,
                    },
                )?;
                Ok((source, id))
            })
            .expect("seed")
    }

    async fn state_of(app: &AppState, id: DownloadId) -> DownloadState {
        app.db
            .read(move |c| rows::get(c, id))
            .await
            .expect("get")
            .expect("row")
            .state
    }

    #[tokio::test]
    async fn a_download_cancelled_while_adding_is_deselected() {
        let (_dir, app) = state();
        Scheduler::start(&app);
        let mock = Arc::new(Mock {
            release: Some(Notify::new()),
            ..Mock::default()
        });
        app.set_client(Arc::clone(&mock) as Arc<dyn DownloadClient>);
        let (_, id) = seed(&app, 1);
        let run = tokio::spawn({
            let app = Arc::clone(&app);
            async move { Scheduler::run_inline(&app, Arc::new(Transfer)).await }
        });
        mock.add_entered.notified().await;
        let cancelled = app
            .db
            .write(move |c| rows::cancel(c, id, 1))
            .await
            .expect("cancel");
        assert!(matches!(
            cancelled,
            CancelOutcome::Cancelled(c) if !c.started
        ));
        if let Some(release) = &mock.release {
            release.notify_one();
        }
        run.await.expect("join").expect("run");
        assert_eq!(state_of(&app, id).await, DownloadState::Cancelled);
        for _ in 0..200 {
            if mock.calls().len() >= 4 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(mock.calls(), ["add:[0]", "start", "stop", "set_wanted:[]"]);
    }

    #[tokio::test]
    async fn move_downloads_moves_and_announces_only_those_that_may_move() {
        let (_dir, app) = state();
        let (_, open) = seed(&app, 1);
        let (_, done) = seed(&app, 2);
        app.db
            .write(move |c| rows::cancel(c, done, 1))
            .await
            .expect("cancel");
        let mut sub = app.events.subscribe(None);
        let moved = move_downloads(&app, &[open, done], DownloadState::Failed, Some("Gone."))
            .await
            .expect("move");
        assert_eq!(moved, [open]);
        let row = app.db.read(move |c| rows::get(c, open)).await.expect("get");
        assert_eq!(row.and_then(|r| r.error).as_deref(), Some("Gone."));
        assert_eq!(state_of(&app, done).await, DownloadState::Cancelled);
        let mut changed = Vec::new();
        while let Ok(e) = sub.live.try_recv() {
            if e.kind == EventKind::DownloadChanged {
                changed.push(serde_json::from_str::<Value>(&e.data).expect("json"));
            }
        }
        assert_eq!(changed.len(), 1, "{changed:?}");
        assert_eq!(changed[0]["download_id"], open.get());
        assert_eq!(changed[0]["state"], "failed");
    }

    #[tokio::test]
    async fn an_unreachable_client_during_add_stops_the_pass() {
        let (_dir, app) = state();
        let mock = Arc::new(Mock {
            unreachable: true,
            ..Mock::default()
        });
        app.set_client(Arc::clone(&mock) as Arc<dyn DownloadClient>);
        let (_, first) = seed(&app, 1);
        let (_, second) = seed(&app, 2);
        Scheduler::run_inline(&app, Arc::new(Transfer))
            .await
            .expect("run");
        assert_eq!(mock.calls(), ["add:[0]"]);
        assert_eq!(state_of(&app, first).await, DownloadState::Queued);
        assert_eq!(state_of(&app, second).await, DownloadState::Queued);
    }

    #[tokio::test]
    async fn without_a_client_queued_downloads_wait() {
        let (_dir, app) = state();
        let mut events = app.events.subscribe(None).live;
        Scheduler::run_inline(&app, Arc::new(Transfer))
            .await
            .expect("run");
        let page = crate::db::sql::Page {
            limit: 10,
            offset: 0,
        };
        let listed = app
            .db
            .read(move |c| rows::list(c, &[], page))
            .await
            .expect("list");
        assert_eq!(listed.total, 0);
        let ev = events.recv().await.expect("job event");
        assert_eq!(ev.kind, EventKind::JobProgress);
        assert!(ev.data.contains(r#""kind":"transfer""#));
    }

    #[tokio::test]
    async fn deselect_without_a_torrent_is_a_no_op() {
        let (_dir, app) = state();
        let job = Deselect {
            source_id: SourceId::new(9),
        };
        assert_eq!(job.payload(), json!({ "source_id": 9 }));
        Scheduler::run_inline(&app, Arc::new(job))
            .await
            .expect("run");
    }

    #[tokio::test]
    async fn a_cancel_deselects_each_started_source_once() {
        let (_dir, app) = state();
        let cancelled = |id, source: Option<i64>, started| Cancelled {
            id: DownloadId::new(id),
            source_id: source.map(SourceId::new),
            started,
        };
        let all = [
            cancelled(1, Some(4), true),
            cancelled(2, Some(4), true),
            cancelled(3, Some(5), false),
            cancelled(4, None, true),
        ];
        after_cancel(&app, &all).await;
        let deselects = app
            .db
            .read(|c| crate::db::jobs::count_kind(c, JobKind::Deselect))
            .await
            .expect("count");
        assert_eq!(deselects, 1);
    }

    #[test]
    fn change_events_carry_state_and_progress() {
        let (_dir, app) = state();
        let mut live = app.events.subscribe(None).live;
        publish(&app, DownloadId::new(3), DownloadState::Checking, 1.0);
        let ev = live.try_recv().expect("event");
        assert_eq!(ev.kind, EventKind::DownloadChanged);
        assert_eq!(
            ev.data,
            r#"{"download_id":3,"state":"checking","progress":1.0}"#
        );
    }
}
