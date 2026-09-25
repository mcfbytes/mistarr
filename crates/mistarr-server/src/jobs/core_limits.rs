//! How the download client is held while a core runs: rate limits, held
//! uploads or a stopped process; see `docs/DOWNLOAD-CLIENTS.md` "Core gate".

use std::fmt::Display;
use std::sync::Arc;
use std::time::Duration;

use mistarr_clients::{
    ClientKind, ClientTorrentId, Direction, DownloadClient, RateLimit, SeedPolicy,
};
use serde::{Deserialize, Serialize};

use crate::app::AppState;
use crate::client::{self, ClientEndpoint};
use crate::config::LimitsConfig;
use crate::db::settings::{self, keys};
use crate::db::sources;
use crate::events::EventKind;
use crate::freeze::{self, FreezeError, Frozen, Kill, Signal};
use crate::threads::{self, label};

/// The longest wait between retries while the client does not take its hold.
pub const RETRY_MAX: Duration = Duration::from_secs(60);

const DIRECTIONS: [Direction; 2] = [Direction::Down, Direction::Up];

/// How the client is held for a running core, as `/system/status` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ClientHold {
    /// Its uploads are held through its own upload limit.
    Uploads,
    /// Its process is stopped.
    Frozen,
}

/// The client's own limits in each direction the gate changed, stored under
/// [`keys::CLIENT_SAVED_LIMITS`] until they are put back.
///
/// ```
/// use mistarr_clients::{ClientKind, RateLimit};
/// use mistarr_server::client::ClientEndpoint;
/// use mistarr_server::jobs::core_limits::SavedLimits;
/// let s = SavedLimits { client: ClientEndpoint { kind: ClientKind::Rtorrent, url: "127.0.0.1:5000".into() },
///     down: None, up: Some(RateLimit::kbps(40)) };
/// let json = serde_json::to_string(&s).unwrap();
/// assert_eq!(serde_json::from_str::<SavedLimits>(&json).unwrap(), s);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedLimits {
    /// The client the limits belong to.
    pub client: ClientEndpoint,
    /// Its own download limit, while the gate replaces it.
    pub down: Option<RateLimit>,
    /// Its own upload limit, while the gate replaces it.
    pub up: Option<RateLimit>,
}

impl SavedLimits {
    fn empty(client: ClientEndpoint) -> Self {
        Self {
            client,
            down: None,
            up: None,
        }
    }

    fn get(&self, dir: Direction) -> Option<RateLimit> {
        match dir {
            Direction::Down => self.down,
            Direction::Up => self.up,
        }
    }

    fn set(&mut self, dir: Direction, limit: Option<RateLimit>) {
        match dir {
            Direction::Down => self.down = limit,
            Direction::Up => self.up = limit,
        }
    }
}

/// What the gate sets in each direction; `None` leaves the client's own limit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Target {
    /// The download limit.
    pub down: Option<RateLimit>,
    /// The upload limit.
    pub up: Option<RateLimit>,
}

impl Target {
    fn get(self, dir: Direction) -> Option<RateLimit> {
        match dir {
            Direction::Down => self.down,
            Direction::Up => self.up,
        }
    }

    fn set(&mut self, dir: Direction, limit: Option<RateLimit>) {
        match dir {
            Direction::Down => self.down = limit,
            Direction::Up => self.up = limit,
        }
    }
}

/// The limits for the menu or a running core: each non-zero `[limits]` value,
/// and uploads held when `hold_uploads`. A zero leaves the client's own limit.
///
/// ```
/// use mistarr_clients::RateLimit;
/// use mistarr_server::config::LimitsConfig;
/// use mistarr_server::jobs::core_limits::target;
/// let l = LimitsConfig::default();
/// assert_eq!(target(&l, true, false).down, Some(RateLimit::kbps(512)));
/// assert_eq!(target(&l, true, true).up, Some(RateLimit::HELD));
/// assert_eq!(target(&l, false, false).up, None);
/// ```
#[must_use]
pub fn target(limits: &LimitsConfig, core: bool, hold_uploads: bool) -> Target {
    let set = |kbps: u32| (kbps > 0).then(|| RateLimit::kbps(kbps));
    if core {
        Target {
            down: set(limits.down_kbps_core),
            up: if hold_uploads {
                Some(RateLimit::HELD)
            } else {
                set(limits.up_kbps_core)
            },
        }
    } else {
        Target {
            down: set(limits.down_kbps_menu),
            up: set(limits.up_kbps_menu),
        }
    }
}

/// A step that did not complete, logged by the caller once per streak.
#[derive(Debug)]
struct Failure {
    what: &'static str,
    error: String,
}

fn failed<E: Display>(what: &'static str) -> impl FnOnce(E) -> Failure {
    move |e| Failure {
        what,
        error: e.to_string(),
    }
}

/// What this process has done to the client.
#[derive(Debug)]
struct Applied {
    /// The client `have` describes.
    client: Option<ClientEndpoint>,
    /// The limits set in it.
    have: Target,
    /// Its own limits the gate replaced.
    saved: Option<SavedLimits>,
    /// Its stopped process.
    frozen: Option<Frozen>,
    /// A client that cannot be frozen, held by its uploads until the menu.
    refused: Option<ClientEndpoint>,
}

impl Applied {
    fn holding(&self) -> bool {
        self.frozen.is_some() || self.have.up == Some(RateLimit::HELD)
    }
}

/// Follows the gate: at the menu the menu limits and the client's own limits
/// elsewhere; while a core runs the core limits, and with
/// `transfer.pause_client_while_playing` a client on the board stopped or one
/// elsewhere with its uploads held. What the client does not take is retried
/// with a backoff from [`crate::app::Options::corename_poll`] to [`RETRY_MAX`],
/// and a held client is checked every [`crate::app::Options::hold_recheck`].
pub async fn follow_gate(app: Arc<AppState>) {
    let mut rx = app.gate.subscribe();
    let mut applied: Option<Applied> = None;
    let mut failures = 0u32;
    loop {
        rx.borrow_and_update();
        let outcome = match applied.as_mut() {
            Some(a) => step(&app, a).await,
            None => match load(&app).await {
                Ok(a) => step(&app, applied.insert(a)).await,
                Err(f) => Err(f),
            },
        };
        if let Some(a) = &applied {
            publish(&app, current_hold(&app, a)).await;
        }
        let delay = match &outcome {
            Ok(()) => {
                if failures > 0 {
                    tracing::info!("the download client took its core-gate hold");
                }
                failures = 0;
                applied
                    .as_ref()
                    .filter(|a| a.holding())
                    .map(|_| app.options.hold_recheck)
            }
            Err(f) => {
                failures += 1;
                if failures == 1 {
                    tracing::warn!(error = %f.error, "{}", f.what);
                } else {
                    tracing::debug!(error = %f.error, failures, "{}", f.what);
                }
                Some(backoff(app.options.corename_poll, failures))
            }
        };
        let recheck = outcome.is_ok() && delay.is_some();
        let timer = tokio::select! {
            r = rx.changed() => {
                if r.is_err() {
                    return;
                }
                false
            }
            () = app.limits_wake.notified() => false,
            () = tokio::time::sleep(delay.unwrap_or(RETRY_MAX)), if delay.is_some() => true,
        };
        if timer && recheck {
            if let Some(a) = applied.as_mut() {
                recheck_hold(&app, a).await;
            }
        }
    }
}

/// `base` doubled for each failure after the first, up to [`RETRY_MAX`].
fn backoff(base: Duration, failures: u32) -> Duration {
    base.saturating_mul(1 << failures.saturating_sub(1).min(16))
        .min(RETRY_MAX)
}

/// Reads what a previous run left: the saved limits and a frozen client.
async fn load(app: &AppState) -> Result<Applied, Failure> {
    let saved = app
        .db
        .read(|c| settings::get_json::<SavedLimits>(c, keys::CLIENT_SAVED_LIMITS))
        .await
        .map_err(failed("cannot read the client limits a previous run saved"))?;
    let file = app.options.frozen_file.clone();
    let frozen = blocking(move || freeze::read_file(&file))
        .await?
        .map_err(failed("cannot read the frozen client record"))?;
    Ok(Applied {
        client: None,
        have: Target::default(),
        saved,
        frozen,
        refused: None,
    })
}

async fn blocking<T, F>(f: F) -> Result<T, Failure>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    threads::blocking(label::CLIENT_FREEZE, f)
        .await
        .map_err(failed("a client task failed"))
}

/// Moves the client towards what the gate wants now.
async fn step(app: &Arc<AppState>, a: &mut Applied) -> Result<(), Failure> {
    let core = app.gate.state().core_running();
    let config = app.config();
    let pause = core && config.transfer.pause_client_while_playing;
    if !core {
        a.refused = None;
    }
    if let Some(frozen) = a.frozen {
        if pause {
            return Ok(());
        }
        thaw_client(app, a, frozen).await?;
    }
    let Some((id, client)) = app.client_entry() else {
        return match a.saved {
            Some(_) => Err(Failure {
                what: "cannot restore the client's own limits",
                error: "no download client".into(),
            }),
            None => Ok(()),
        };
    };
    if a.client.as_ref() != Some(&id) {
        a.client = Some(id.clone());
        a.have = Target::default();
    }
    if pause && client::is_local(&id.url) && a.refused.as_ref() != Some(&id) {
        let menu = target(&config.limits, false, false);
        apply_limits(app, a, &id, client.as_ref(), menu).await?;
        match freeze_client(app, &id, client.as_ref()).await? {
            Ok(frozen) => {
                a.frozen = Some(frozen);
                tracing::info!(pid = frozen.pid, "download client paused while a core runs");
                return Ok(());
            }
            Err(e) => {
                tracing::warn!(error = %e, "cannot pause the download client; holding its uploads instead");
                a.refused = Some(id.clone());
            }
        }
    }
    let want = target(&config.limits, core, pause);
    apply_limits(app, a, &id, client.as_ref(), want).await
}

/// Stops the client's process. The inner error is a client that cannot be
/// frozen; the outer one a step to retry.
async fn freeze_client(
    app: &AppState,
    id: &ClientEndpoint,
    client: &dyn DownloadClient,
) -> Result<Result<Frozen, FreezeError>, Failure> {
    let proc = app.options.proc_dir.clone();
    let pid = match id.kind {
        ClientKind::Rtorrent => {
            let reported = client
                .process_id()
                .await
                .map_err(failed("cannot read the client's process id"))?;
            let Some(pid) = reported else {
                return Ok(Err(FreezeError::NotFound("rtorrent".into())));
            };
            let proc = proc.clone();
            match blocking(move || freeze::check_name(&proc, pid, "rtorrent")).await? {
                Ok(()) => pid,
                Err(e) => return Ok(Err(e)),
            }
        }
        ClientKind::Transmission => {
            let (proc, port) = (proc.clone(), client::port(&id.url));
            match blocking(move || freeze::find(&proc, "transmission-daemon", port)).await? {
                Ok(pid) => pid,
                Err(e) => return Ok(Err(e)),
            }
        }
        _ => return Ok(Err(FreezeError::NotFound(id.kind.to_string()))),
    };
    let before = app.client_hold();
    // Held first, so nothing new is sent to a process about to stop.
    app.set_client_hold(Some(ClientHold::Frozen));
    let (kill, file) = (
        Kill::new(&app.options.kill),
        app.options.frozen_file.clone(),
    );
    let done = blocking(move || freeze::freeze(&proc, &kill, &file, pid)).await;
    match done {
        Ok(Ok(frozen)) => {
            announce(app).await;
            Ok(Ok(frozen))
        }
        Ok(Err(e @ (FreezeError::Kill(_) | FreezeError::Io(_)))) => {
            app.set_client_hold(before);
            Err(failed("cannot pause the download client")(e))
        }
        Ok(Err(e)) => {
            app.set_client_hold(before);
            Ok(Err(e))
        }
        Err(f) => {
            app.set_client_hold(before);
            Err(f)
        }
    }
}

/// Resumes a frozen client, refusing a pid that now names another process,
/// then lets polling and transfers carry on.
async fn thaw_client(app: &Arc<AppState>, a: &mut Applied, frozen: Frozen) -> Result<(), Failure> {
    let (proc, kill) = (app.options.proc_dir.clone(), Kill::new(&app.options.kill));
    match blocking(move || freeze::thaw(&proc, &kill, frozen)).await? {
        Ok(()) => tracing::info!(pid = frozen.pid, "download client resumed"),
        Err(e @ (FreezeError::Gone(_) | FreezeError::Reused(_))) => {
            tracing::warn!(error = %e, "the paused download client is gone; nothing to resume");
        }
        Err(e) => return Err(failed("cannot resume the download client")(e)),
    }
    let file = app.options.frozen_file.clone();
    blocking(move || freeze::remove_file(&file))
        .await?
        .map_err(failed("cannot remove the frozen client record"))?;
    a.frozen = None;
    publish(app, None).await;
    if let Some((_, client)) = app.client_entry() {
        apply_deferred_seed(app, client.as_ref()).await;
    }
    app.poll_wake.notify_one();
    crate::jobs::transfer::kick(app).await;
    Ok(())
}

/// Applies the seed policies changed while the client was frozen.
async fn apply_deferred_seed(app: &AppState, client: &dyn DownloadClient) {
    for source in app.take_deferred_seed() {
        let row = match app.db.read(move |c| sources::get(c, source)).await {
            Ok(Some(row)) => row,
            Ok(None) => continue,
            Err(e) => {
                tracing::warn!(error = %e, "cannot read a source to apply its seed policy");
                continue;
            }
        };
        let Some(cid) = row.client_id else { continue };
        let seed = sources::seed_from_text(&row.seed_policy).unwrap_or(SeedPolicy::None);
        if let Err(e) = client
            .set_seed_policy(&ClientTorrentId::new(cid), seed)
            .await
        {
            tracing::warn!(source = %source, error = %e, "cannot apply the seed policy in the client");
        }
    }
}

/// Sets `want` in the client. Before the gate first changes a direction it
/// saves the client's own limit there, and it puts that limit back once the
/// gate no longer sets the direction; a saved limit is never replaced.
async fn apply_limits(
    app: &AppState,
    a: &mut Applied,
    id: &ClientEndpoint,
    client: &dyn DownloadClient,
    want: Target,
) -> Result<(), Failure> {
    if a.saved.as_ref().is_some_and(|s| s.client != *id) {
        tracing::warn!("dropping the limits saved for a download client no longer in use");
        persist(app, None).await?;
        a.saved = None;
    }
    let mut saved = a
        .saved
        .clone()
        .unwrap_or_else(|| SavedLimits::empty(id.clone()));
    let mut read = false;
    for dir in DIRECTIONS {
        if want.get(dir).is_some() && saved.get(dir).is_none() {
            let own = client
                .rate_limit(dir)
                .await
                .map_err(failed("cannot read the client's rate limit"))?;
            saved.set(dir, Some(own));
            read = true;
        }
    }
    if read {
        persist(app, Some(&saved)).await?;
        a.saved = Some(saved.clone());
    }
    for dir in DIRECTIONS {
        match (want.get(dir), saved.get(dir)) {
            (Some(limit), _) if a.have.get(dir) != Some(limit) => {
                client
                    .set_rate_limit(dir, limit)
                    .await
                    .map_err(failed("cannot set the client's rate limit"))?;
                a.have.set(dir, Some(limit));
            }
            (None, Some(own)) => {
                client
                    .set_rate_limit(dir, own)
                    .await
                    .map_err(failed("cannot restore the client's rate limit"))?;
                a.have.set(dir, None);
                saved.set(dir, None);
                let keep = (saved.down.is_some() || saved.up.is_some()).then_some(&saved);
                persist(app, keep).await?;
                a.saved = keep.cloned();
            }
            _ => {}
        }
    }
    Ok(())
}

async fn persist(app: &AppState, saved: Option<&SavedLimits>) -> Result<(), Failure> {
    let saved = saved.cloned();
    app.db
        .write(move |c| match &saved {
            Some(s) => settings::set_json(c, keys::CLIENT_SAVED_LIMITS, s),
            None => settings::remove(c, keys::CLIENT_SAVED_LIMITS),
        })
        .await
        .map_err(failed("cannot save the client's own limits"))
}

/// Checks a held client is still held: a process resumed elsewhere is stopped
/// again, one that exited is let go so the next step finds its successor, and
/// uploads that left their hold are held again.
async fn recheck_hold(app: &AppState, a: &mut Applied) {
    if let Some(frozen) = a.frozen {
        let proc = app.options.proc_dir.clone();
        match blocking(move || freeze::is_stopped(&proc, frozen)).await {
            Ok(Ok(true)) => {}
            Ok(Ok(false)) => {
                tracing::info!(
                    pid = frozen.pid,
                    "the download client was resumed elsewhere; pausing it again"
                );
                let kill = Kill::new(&app.options.kill);
                let sent = blocking(move || kill.send(frozen.pid, Signal::Stop)).await;
                if let Ok(Err(e)) = sent {
                    tracing::warn!(error = %e, "cannot pause the download client again");
                }
            }
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "the paused download client exited");
                let file = app.options.frozen_file.clone();
                if let Ok(Err(e)) = blocking(move || freeze::remove_file(&file)).await {
                    tracing::warn!(error = %e, "cannot remove the frozen client record");
                }
                a.frozen = None;
                a.have = Target::default();
                publish(app, None).await;
            }
            Err(f) => tracing::debug!(error = %f.error, "{}", f.what),
        }
        return;
    }
    if a.have.up != Some(RateLimit::HELD) {
        return;
    }
    let Some((id, client)) = app.client_entry() else {
        return;
    };
    if a.client.as_ref() != Some(&id) {
        return;
    }
    match client.rate_limit(Direction::Up).await {
        Ok(up) if !up.is_held() => {
            tracing::info!("the download client's uploads left their hold; holding them again");
            a.have = Target::default();
        }
        Ok(_) => {}
        Err(e) => tracing::debug!(error = %e, "cannot check the client's upload hold"),
    }
}

/// How the client is held now.
fn current_hold(app: &AppState, a: &Applied) -> Option<ClientHold> {
    let pause = app.gate.state().core_running() && app.config().transfer.pause_client_while_playing;
    if a.frozen.is_some() {
        Some(ClientHold::Frozen)
    } else if pause
        && a.have.up == Some(RateLimit::HELD)
        && a.client == app.client_entry().map(|(id, _)| id)
    {
        Some(ClientHold::Uploads)
    } else {
        None
    }
}

/// Records how the client is held and publishes the status when that changed.
async fn publish(app: &AppState, hold: Option<ClientHold>) {
    if app.set_client_hold(hold) {
        announce(app).await;
    }
}

async fn announce(app: &AppState) {
    let status = crate::status::snapshot(app).await;
    app.events.publish(EventKind::Status, &status);
}

/// At startup, resumes a client a previous run left frozen, unless a core
/// still runs and the setting holds it; then it stays frozen.
pub async fn recover_frozen(app: &Arc<AppState>) {
    let file = app.options.frozen_file.clone();
    let frozen = match blocking(move || freeze::read_file(&file)).await {
        Ok(Ok(Some(frozen))) => frozen,
        Ok(Ok(None)) => return,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "cannot read the frozen client record");
            return;
        }
        Err(f) => {
            tracing::warn!(error = %f.error, "{}", f.what);
            return;
        }
    };
    app.set_client_hold(Some(ClientHold::Frozen));
    if app.gate.state().core_running() && app.config().transfer.pause_client_while_playing {
        return;
    }
    if resume(app, frozen).await {
        app.set_client_hold(None);
    }
}

/// On shutdown, resumes a frozen client, so a stopped mistarr never leaves it frozen.
pub async fn thaw_for_shutdown(app: &AppState) {
    let file = app.options.frozen_file.clone();
    if let Ok(Ok(Some(frozen))) = blocking(move || freeze::read_file(&file)).await {
        if resume(app, frozen).await {
            app.set_client_hold(None);
        }
    }
}

/// Sends SIGCONT and removes the record; true unless the signal failed.
async fn resume(app: &AppState, frozen: Frozen) -> bool {
    let (proc, kill) = (app.options.proc_dir.clone(), Kill::new(&app.options.kill));
    match blocking(move || freeze::thaw(&proc, &kill, frozen)).await {
        Ok(Ok(())) => tracing::info!(pid = frozen.pid, "download client resumed"),
        Ok(Err(e @ (FreezeError::Gone(_) | FreezeError::Reused(_)))) => {
            tracing::warn!(error = %e, "the paused download client is gone; nothing to resume");
        }
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "cannot resume the download client");
            return false;
        }
        Err(f) => {
            tracing::warn!(error = %f.error, "{}", f.what);
            return false;
        }
    }
    let file = app.options.frozen_file.clone();
    if let Ok(Err(e)) = blocking(move || freeze::remove_file(&file)).await {
        tracing::warn!(error = %e, "cannot remove the frozen client record");
    }
    true
}

#[cfg(test)]
mod tests;
