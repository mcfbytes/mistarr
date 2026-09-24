//! The download poller and the core-gate rate limits; see `docs/DOWNLOAD-CLIENTS.md`
//! "Polling" and "Core gate".

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use mistarr_clients::{
    ClientError, ClientTorrentId, DownloadClient, SeedPolicy, TorrentState, TorrentStatus,
};

use super::transfer;
use crate::app::{AppState, Options};
use crate::config::LimitsConfig;
use crate::db::downloads::{self as rows, DownloadId, DownloadState, Observed, PollRow};
use crate::db::settings::{self, keys};
use crate::db::sources::{self, SourceId};
use crate::error::Result;
use crate::events::EventKind;
use crate::jobs::detect_client::ClientStatus;

/// Consecutive failed polls after which the client is shown unreachable.
pub const FAILURES_BEFORE_UNREACHABLE: u32 = 3;

/// Error stored on downloads whose torrent the client no longer has.
pub const LOST_TORRENT: &str =
    "The torrent is no longer in the download client. Retry to add it again.";

/// How soon the next poll is due.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cadence {
    /// Something is transferring or checking.
    Active,
    /// Nothing is.
    Idle,
    /// The client failed repeatedly.
    Backoff,
}

impl Cadence {
    /// The wait before the next poll under `options`.
    ///
    /// ```
    /// use mistarr_server::app::Options;
    /// use mistarr_server::jobs::poll::Cadence;
    /// assert_eq!(Cadence::Active.interval(&Options::default()).as_secs(), 5);
    /// ```
    #[must_use]
    pub fn interval(self, options: &Options) -> Duration {
        match self {
            Self::Active => options.poll_active,
            Self::Idle => options.poll_idle,
            Self::Backoff => options.poll_backoff,
        }
    }
}

/// What one poll found for one download.
#[derive(Debug, Clone, PartialEq)]
pub struct Seen {
    /// The download.
    pub id: DownloadId,
    /// The state it maps to.
    pub state: DownloadState,
    /// 0 to 1.
    pub progress: f64,
    /// Local path of the finished file, once `importing`.
    pub staged_path: Option<String>,
    /// Why it failed.
    pub error: Option<String>,
}

/// Where the client puts a download's file, seen locally: `<staging>/<infohash>/`,
/// then the torrent's name unless it is a single file, then the path in the torrent.
///
/// ```
/// use std::path::Path;
/// use mistarr_server::db::downloads::{DownloadId, DownloadState, PollRow};
/// use mistarr_server::db::sources::SourceId;
/// let row = PollRow { id: DownloadId(1), state: DownloadState::Transferring, progress: 0.0,
///     staged_path: None, source_id: SourceId(1), file_index: 0, path: "NES/a.nes".into(),
///     infohash: "ab".into(), torrent_name: "Set".into(), single_file: false,
///     client_id: None, seed_policy: "none".into(), size: 4 };
/// let p = mistarr_server::jobs::poll::staged_path(Path::new("/s"), &row);
/// assert_eq!(p, Path::new("/s/ab/Set/NES/a.nes"));
/// ```
#[must_use]
pub fn staged_path(staging: &Path, row: &PollRow) -> PathBuf {
    let mut out = staging.join(&row.infohash);
    if !row.single_file {
        push_relative(&mut out, &row.torrent_name);
    }
    push_relative(&mut out, &row.path);
    out
}

/// Appends only the plain components of `rel`, so a torrent cannot name a path outside.
fn push_relative(out: &mut PathBuf, rel: &str) {
    for c in Path::new(rel).components() {
        if let Component::Normal(part) = c {
            out.push(part);
        }
    }
}

/// Maps the client's view of a torrent onto one download; `None` when the
/// client does not list the file yet.
///
/// ```
/// use std::path::Path;
/// use mistarr_clients::{FileProgress, InfoHash, TorrentState, TorrentStatus};
/// use mistarr_server::db::downloads::{DownloadId, DownloadState, PollRow};
/// use mistarr_server::db::sources::SourceId;
/// let row = PollRow { id: DownloadId(1), state: DownloadState::Transferring, progress: 0.0,
///     staged_path: None, source_id: SourceId(1), file_index: 0, path: "a.nes".into(),
///     infohash: "ab".into(), torrent_name: "a.nes".into(), single_file: true,
///     client_id: None, seed_policy: "none".into(), size: 4 };
/// let st = TorrentStatus { infohash: InfoHash::from_bytes([0; 20]), state: TorrentState::Downloading,
///     files: vec![FileProgress { index: 0, bytes_done: 1, size: None, wanted: true }],
///     ratio: 0.0, down_rate: 0, up_rate: 0, is_finished: false };
/// let seen = mistarr_server::jobs::poll::observe(&st, &row, Path::new("/s")).unwrap();
/// assert_eq!((seen.state, seen.progress), (DownloadState::Transferring, 0.25));
/// ```
#[must_use]
pub fn observe(status: &TorrentStatus, row: &PollRow, staging: &Path) -> Option<Seen> {
    if let TorrentState::Error(msg) = &status.state {
        return Some(Seen {
            id: row.id,
            state: DownloadState::Failed,
            progress: row.progress,
            staged_path: None,
            error: Some(format!("The download client stopped the torrent: {msg}")),
        });
    }
    let file = status.file(row.file_index)?;
    let size = file.size_or(row.size);
    // Byte counts stay far below 2^52, so the ratio is exact enough.
    #[allow(clippy::cast_precision_loss)]
    let progress = if size == 0 {
        1.0
    } else {
        file.bytes_done.min(size) as f64 / size as f64
    };
    let (state, staged) = if file.bytes_done != size {
        (DownloadState::Transferring, None)
    } else if status.file_done(row.file_index, row.size) {
        let path = staged_path(staging, row);
        (
            DownloadState::Importing,
            Some(path.to_string_lossy().into_owned()),
        )
    } else {
        (DownloadState::Checking, None)
    };
    Some(Seen {
        id: row.id,
        state,
        progress,
        staged_path: staged,
        error: None,
    })
}

/// The poll loop's memory between polls.
#[derive(Default)]
pub struct Poller {
    failures: u32,
    unreachable: bool,
    client: Option<Arc<dyn DownloadClient>>,
    seeded: HashSet<SourceId>,
}

impl Poller {
    /// A poller that has not polled yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Consecutive polls in which a client call failed.
    #[must_use]
    pub fn failures(&self) -> u32 {
        self.failures
    }

    /// The cadence the current failures and downloads call for.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Db`] when the downloads cannot be counted.
    pub async fn cadence(&self, app: &AppState) -> Result<Cadence> {
        if self.failures >= FAILURES_BEFORE_UNREACHABLE {
            return Ok(Cadence::Backoff);
        }
        let active = app
            .db
            .read(|c| rows::count_in(c, &[DownloadState::Transferring, DownloadState::Checking]))
            .await?;
        Ok(if active > 0 {
            Cadence::Active
        } else {
            Cadence::Idle
        })
    }

    /// Reads every torrent with a transferring or checking download from the
    /// client, stores what moved and publishes `download.changed` for it, and
    /// returns the cadence for the next poll. A seed policy is applied to each
    /// torrent once per client handle, since rtorrent forgets it on restart.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Db`] on database failure; client failures are counted instead.
    pub async fn tick(&mut self, app: &AppState) -> Result<Cadence> {
        let Some(client) = app.client() else {
            return self.cadence(app).await;
        };
        if !self
            .client
            .as_ref()
            .is_some_and(|c| Arc::ptr_eq(c, &client))
        {
            self.seeded.clear();
            self.client = Some(Arc::clone(&client));
        }
        let polled = app.db.read(rows::polled).await?;
        let mut failed = false;
        let mut answered = false;
        for group in polled.chunk_by(|a, b| a.source_id == b.source_id) {
            match self.poll_source(app, client.as_ref(), group).await? {
                Some(true) => answered = true,
                Some(false) => {}
                None => {
                    failed = true;
                    break;
                }
            }
        }
        self.record(app, failed, answered).await?;
        self.cadence(app).await
    }

    /// Polls one torrent; `None` when the client failed, else whether it answered.
    async fn poll_source(
        &mut self,
        app: &AppState,
        client: &dyn DownloadClient,
        group: &[PollRow],
    ) -> Result<Option<bool>> {
        let first = &group[0];
        let Some(cid) = first.client_id.as_deref() else {
            return Ok(Some(false));
        };
        let (source, id) = (first.source_id, ClientTorrentId::new(cid));
        let policy = sources::seed_from_text(&first.seed_policy).unwrap_or(SeedPolicy::None);
        if !self.seeded.contains(&source) {
            match client.set_seed_policy(&id, policy.clone()).await {
                Ok(()) => {
                    self.seeded.insert(source);
                }
                Err(ClientError::NotFound) => return lost(app, source, group).await,
                Err(e) => return Ok(client_failed(&e)),
            }
        }
        let status = match client.status(&id).await {
            Ok(s) => s,
            Err(ClientError::NotFound) => return lost(app, source, group).await,
            Err(e) => return Ok(client_failed(&e)),
        };
        let staging = app.config().paths.staging();
        let seen: Vec<Seen> = group
            .iter()
            .filter_map(|r| observe(&status, r, &staging))
            .collect();
        let (changed, open) = app
            .db
            .write(move |c| {
                let now = crate::unix_now();
                let mut changed = Vec::new();
                for s in seen {
                    let o = Observed {
                        state: s.state,
                        progress: s.progress,
                        staged_path: s.staged_path.as_deref(),
                        error: s.error.as_deref(),
                    };
                    if rows::observe(c, s.id, &o, now)? {
                        changed.push(s);
                    }
                }
                let open = rows::of_source(
                    c,
                    source,
                    &[
                        DownloadState::Queued,
                        DownloadState::Transferring,
                        DownloadState::Checking,
                    ],
                )?;
                Ok((changed, open.len()))
            })
            .await?;
        let handed = changed.iter().any(|s| s.state == DownloadState::Importing);
        for s in &changed {
            transfer::publish(app, s.id, s.state, s.progress);
        }
        if handed && should_stop(&status, open, &policy) {
            if let Err(e) = client.stop(&id).await {
                tracing::warn!(source = %source, error = %e, "cannot stop the finished torrent");
            }
        }
        Ok(Some(true))
    }

    /// Counts a failed poll, or clears the count after an answered one, and
    /// shows the client unreachable or reachable again when that changes.
    async fn record(&mut self, app: &AppState, failed: bool, answered: bool) -> Result<()> {
        if failed {
            self.failures += 1;
            if self.failures >= FAILURES_BEFORE_UNREACHABLE && !self.unreachable {
                self.unreachable = true;
                tracing::warn!(failures = self.failures, "download client unreachable");
                set_reachable(app, false).await?;
            }
        } else if answered {
            self.failures = 0;
            if self.unreachable {
                self.unreachable = false;
                set_reachable(app, true).await?;
            }
        }
        Ok(())
    }
}

fn client_failed(e: &ClientError) -> Option<bool> {
    tracing::debug!(error = %e, "poll failed");
    None
}

/// Fails the torrent's polled downloads and forgets its client id.
async fn lost(app: &AppState, source: SourceId, group: &[PollRow]) -> Result<Option<bool>> {
    let ids: Vec<DownloadId> = group.iter().map(|r| r.id).collect();
    let moved = app
        .db
        .write(move |c| {
            sources::set_client_id(c, source, None)?;
            rows::move_all(
                c,
                &ids,
                DownloadState::Failed,
                Some(LOST_TORRENT),
                crate::unix_now(),
            )
        })
        .await?;
    transfer::publish_ids(app, moved).await?;
    Ok(Some(true))
}

/// Stores `reachable` on the detected client status and publishes `status`.
async fn set_reachable(app: &AppState, reachable: bool) -> Result<()> {
    let changed = app
        .db
        .write(move |c| {
            let Some(mut st) = settings::get_json::<ClientStatus>(c, keys::CLIENT_DETECTED)? else {
                return Ok(false);
            };
            if st.reachable == reachable {
                return Ok(false);
            }
            st.reachable = reachable;
            settings::set_json(c, keys::CLIENT_DETECTED, &st)?;
            Ok(true)
        })
        .await?;
    if changed {
        let status = crate::status::snapshot(app).await;
        app.events.publish(EventKind::Status, &status);
    }
    Ok(())
}

/// Polls at the cadence [`Poller::tick`] returns, waking early to re-check
/// the cadence when a transfer starts, and queues a [`transfer::Transfer`]
/// while downloads wait for the client.
pub async fn run(app: Arc<AppState>) {
    let mut poller = Poller::new();
    let mut next = poller.cadence(&app).await.unwrap_or(Cadence::Idle);
    loop {
        tokio::select! {
            () = tokio::time::sleep(next.interval(&app.options)) => {}
            () = app.poll_wake.notified() => {
                next = poller.cadence(&app).await.unwrap_or(next);
                continue;
            }
        }
        let waiting = app
            .db
            .read(|c| rows::count_in(c, &[DownloadState::Wanted, DownloadState::Queued]))
            .await
            .unwrap_or(0);
        if waiting > 0 {
            transfer::kick(&app).await;
        }
        next = poller.tick(&app).await.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "poll failed");
            Cadence::Idle
        });
    }
}

/// The `(down, up)` limits in KiB/s for the menu or a running core; 0 is unlimited.
///
/// ```
/// use mistarr_server::config::LimitsConfig;
/// use mistarr_server::jobs::poll::limits_for;
/// assert_eq!(limits_for(&LimitsConfig::default(), true), (512, 64));
/// assert_eq!(limits_for(&LimitsConfig::default(), false), (0, 0));
/// ```
#[must_use]
pub fn limits_for(limits: &LimitsConfig, core: bool) -> (u32, u32) {
    if core {
        (limits.down_kbps_core, limits.up_kbps_core)
    } else {
        (limits.down_kbps_menu, limits.up_kbps_menu)
    }
}

/// Whether the poller stops a torrent: only under seed policy `none`, which
/// neither client acts on by itself, once no download of its source is open
/// and every file selected in the client is complete.
#[must_use]
pub fn should_stop(status: &TorrentStatus, open: usize, policy: &SeedPolicy) -> bool {
    *policy == SeedPolicy::None
        && open == 0
        && status.state != TorrentState::Stopped
        && status
            .files
            .iter()
            .filter(|f| f.wanted)
            .all(mistarr_clients::FileProgress::is_complete)
}

/// Applies the core limits each time CORENAME leaves `MENU` and the menu
/// limits each time it returns, once per transition. While no client takes
/// them, they are retried every [`Options::corename_poll`].
pub async fn follow_gate(app: Arc<AppState>) {
    let mut rx = app.gate.subscribe();
    let mut applied = false;
    loop {
        let want = rx.borrow_and_update().core_running();
        if want != applied && apply_limits(&app, want).await {
            applied = want;
        }
        let changed = if want == applied {
            rx.changed().await.is_ok()
        } else {
            tokio::select! {
                r = rx.changed() => r.is_ok(),
                () = tokio::time::sleep(app.options.corename_poll) => true,
            }
        };
        if !changed {
            return;
        }
    }
}

/// Sends the core or menu limits; true when the client took them.
async fn apply_limits(app: &AppState, core: bool) -> bool {
    let (down, up) = limits_for(&app.config().limits, core);
    let Some(client) = app.client() else {
        tracing::debug!("no download client for rate limits");
        return false;
    };
    match client.set_rate_limits(Some(down), Some(up)).await {
        Ok(()) => {
            tracing::info!(core, down, up, "rate limits applied");
            true
        }
        Err(e) => {
            tracing::warn!(error = %e, "cannot apply rate limits");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::state;
    use mistarr_clients::{FileProgress, InfoHash};

    fn row(file_index: u32, single: bool) -> PollRow {
        PollRow {
            id: DownloadId(7),
            state: DownloadState::Transferring,
            progress: 0.5,
            staged_path: None,
            source_id: SourceId(1),
            file_index,
            path: "Sub/../x.nes".into(),
            infohash: "cd".into(),
            torrent_name: "Set".into(),
            single_file: single,
            client_id: Some("cd".into()),
            seed_policy: "none".into(),
            size: if file_index == 1 { 0 } else { 8 },
        }
    }

    fn status(state: TorrentState, done: u64) -> TorrentStatus {
        TorrentStatus {
            infohash: InfoHash::from_bytes([0; 20]),
            state,
            files: vec![
                FileProgress {
                    index: 0,
                    bytes_done: done,
                    size: Some(8),
                    wanted: true,
                },
                FileProgress {
                    index: 1,
                    bytes_done: 0,
                    size: Some(0),
                    wanted: false,
                },
            ],
            ratio: 0.0,
            down_rate: 0,
            up_rate: 0,
            is_finished: false,
        }
    }

    #[test]
    fn states_follow_the_client() {
        let staging = Path::new("/st");
        let r = row(0, false);
        let s = observe(&status(TorrentState::Downloading, 2), &r, staging).expect("seen");
        assert_eq!((s.state, s.progress), (DownloadState::Transferring, 0.25));
        let s = observe(&status(TorrentState::Checking, 8), &r, staging).expect("seen");
        assert_eq!((s.state, s.progress), (DownloadState::Checking, 1.0));
        let s = observe(&status(TorrentState::Seeding, 8), &r, staging).expect("seen");
        assert_eq!(s.state, DownloadState::Importing);
        assert_eq!(s.staged_path.as_deref(), Some("/st/cd/Set/Sub/x.nes"));
        let s = observe(&status(TorrentState::Error("disk".into()), 2), &r, staging).expect("seen");
        assert_eq!(s.state, DownloadState::Failed);
        assert!(s.error.is_some_and(|e| e.ends_with("disk")));
        let empty = observe(&status(TorrentState::Stopped, 0), &row(1, true), staging);
        assert_eq!(empty.map(|s| s.progress), Some(1.0));
        assert!(observe(&status(TorrentState::Stopped, 0), &row(5, true), staging).is_none());
        let single = row(0, true);
        assert_eq!(staged_path(staging, &single), Path::new("/st/cd/Sub/x.nes"));
    }

    #[test]
    fn sizes_the_client_leaves_out_come_from_the_metainfo() {
        let staging = Path::new("/st");
        let sizeless = |state, done| {
            let mut st = status(state, done);
            st.files.iter_mut().for_each(|f| f.size = None);
            st
        };
        let r = row(0, false);
        let s = observe(&sizeless(TorrentState::Downloading, 2), &r, staging).expect("seen");
        assert_eq!((s.state, s.progress), (DownloadState::Transferring, 0.25));
        let s = observe(&sizeless(TorrentState::Downloading, 8), &r, staging).expect("seen");
        assert_eq!(s.state, DownloadState::Importing);
        let s = observe(&sizeless(TorrentState::Checking, 8), &r, staging).expect("seen");
        assert_eq!(s.state, DownloadState::Checking);
        assert!(!should_stop(
            &sizeless(TorrentState::Seeding, 8),
            0,
            &SeedPolicy::None
        ));
    }

    #[test]
    fn only_a_settled_none_policy_torrent_is_stopped() {
        let done = status(TorrentState::Seeding, 8);
        assert!(should_stop(&done, 0, &SeedPolicy::None));
        assert!(
            !should_stop(&done, 1, &SeedPolicy::None),
            "a download is open"
        );
        assert!(!should_stop(&done, 0, &SeedPolicy::Client));
        assert!(!should_stop(&done, 0, &SeedPolicy::Ratio { ratio: 1.0 }));
        let partial = status(TorrentState::Downloading, 4);
        assert!(
            !should_stop(&partial, 0, &SeedPolicy::None),
            "a selected file is incomplete"
        );
        let stopped = status(TorrentState::Stopped, 8);
        assert!(
            !should_stop(&stopped, 0, &SeedPolicy::None),
            "already stopped"
        );
    }

    #[test]
    fn cadences_map_to_options() {
        let o = Options::default();
        assert_eq!(Cadence::Idle.interval(&o), Duration::from_secs(60));
        assert_eq!(Cadence::Backoff.interval(&o), Duration::from_secs(300));
        let l = LimitsConfig {
            down_kbps_menu: 9,
            ..LimitsConfig::default()
        };
        assert_eq!(limits_for(&l, false), (9, 0));
    }

    #[tokio::test]
    async fn without_a_client_the_poller_idles() {
        let (_dir, app) = state();
        let mut p = Poller::new();
        assert_eq!(p.tick(&app).await.expect("tick"), Cadence::Idle);
        assert_eq!(p.failures(), 0);
    }

    #[tokio::test]
    async fn repeated_failures_back_off_and_flag_the_client() {
        let (_dir, app) = state();
        let detected = ClientStatus {
            kind: None,
            url: None,
            reachable: true,
            version: None,
            rtorrent_on_path: false,
            checked_at: 0,
        };
        app.db
            .write(move |c| settings::set_json(c, keys::CLIENT_DETECTED, &detected))
            .await
            .expect("store");
        let mut p = Poller::new();
        for _ in 0..FAILURES_BEFORE_UNREACHABLE {
            p.record(&app, true, false).await.expect("record");
        }
        assert_eq!(p.cadence(&app).await.expect("cadence"), Cadence::Backoff);
        let read = |app: Arc<AppState>| async move {
            app.db
                .read(|c| settings::get_json::<ClientStatus>(c, keys::CLIENT_DETECTED))
                .await
                .expect("read")
                .expect("stored")
                .reachable
        };
        assert!(!read(Arc::clone(&app)).await);
        p.record(&app, false, true).await.expect("record");
        assert_eq!(p.failures(), 0);
        assert!(read(Arc::clone(&app)).await);
    }
}
