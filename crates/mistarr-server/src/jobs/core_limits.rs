//! How the download client is held while a core runs: rate limits, held
//! uploads or a stopped process; see `docs/DOWNLOAD-CLIENTS.md` "Core gate".

use std::fmt::Display;
use std::sync::{Arc, PoisonError};
use std::time::Duration;

use tokio::time::Instant;

use mistarr_clients::{
    ClientError, ClientKind, ClientTorrentId, Direction, DownloadClient, RateLimit,
};
use serde::{Deserialize, Serialize};

use crate::app::AppState;
use crate::client::{self, ClientEndpoint, ClientKey};
use crate::config::LimitsConfig;
use crate::db::deferred::{self, Op};
use crate::db::settings::{self, keys};
use crate::db::sources;
use crate::events::EventKind;
use crate::freeze::{self, FreezeError, Frozen, Kill};
use crate::threads::{self, label};

/// The longest wait between retries while the client does not take its hold.
pub const RETRY_MAX: Duration = Duration::from_secs(60);

/// How long one try at putting back a previous client's limits may take.
const RESTORE_WAIT: Duration = Duration::from_secs(5);

/// The first wait before a previous client's limits are tried again; it
/// doubles after each failed try up to [`PREVIOUS_RETRY_MAX`].
const PREVIOUS_RETRY_FIRST: Duration = Duration::from_mins(1);

/// The longest wait between tries at a previous client's limits.
const PREVIOUS_RETRY_MAX: Duration = Duration::from_hours(1);

/// How long a previous client's limits are kept for another try.
pub const PREVIOUS_KEEP: Duration = Duration::from_hours(24);

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

/// The client's own limits the gate changed, stored under
/// [`keys::CLIENT_SAVED_LIMITS`] until they are put back.
///
/// ```
/// use mistarr_clients::{ClientKind, RateLimit};
/// use mistarr_server::client::ClientEndpoint;
/// use mistarr_server::jobs::core_limits::SavedLimits;
/// let s = SavedLimits { client: ClientEndpoint { kind: ClientKind::Rtorrent, url: "127.0.0.1:5000".into() },
///     down: None, up: Some(RateLimit::kbps(40)), alt_up: None };
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
    /// Its own alternate upload rate in kbps, while held uploads replace it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alt_up: Option<u32>,
}

impl SavedLimits {
    fn empty(client: ClientEndpoint) -> Self {
        Self {
            client,
            down: None,
            up: None,
            alt_up: None,
        }
    }

    fn is_empty(&self) -> bool {
        self.down.is_none() && self.up.is_none() && self.alt_up.is_none()
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

/// Limits saved for a client no longer in use, or with no client detected,
/// stored in a list under [`keys::CLIENT_PREVIOUS_LIMITS`]. They are put back
/// through a handle built for that client, tried again with a growing wait,
/// and dropped with a warning once a try fails [`PREVIOUS_KEEP`] after they
/// were set aside.
///
/// ```
/// use mistarr_clients::{ClientKind, RateLimit};
/// use mistarr_server::client::ClientEndpoint;
/// use mistarr_server::jobs::core_limits::{PreviousLimits, SavedLimits};
/// let saved = SavedLimits { client: ClientEndpoint { kind: ClientKind::Rtorrent, url: "127.0.0.1:5000".into() },
///     down: None, up: Some(RateLimit::kbps(40)), alt_up: None };
/// let p = PreviousLimits { saved, since: 10, tries: 1, next_at: 70 };
/// let json = serde_json::to_string(&p).unwrap();
/// assert_eq!(serde_json::from_str::<PreviousLimits>(&json).unwrap(), p);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviousLimits {
    /// The limits and the client they belong to.
    pub saved: SavedLimits,
    /// When they were set aside, in seconds since the epoch.
    pub since: i64,
    /// Failed tries so far.
    pub tries: u32,
    /// When to try next, in seconds since the epoch.
    pub next_at: i64,
}

/// The wait after `tries` failed tries at a previous client's limits.
///
/// ```
/// use std::time::Duration;
/// use mistarr_server::jobs::core_limits::previous_retry;
/// assert_eq!(previous_retry(1), Duration::from_secs(60));
/// assert_eq!(previous_retry(3), Duration::from_secs(240));
/// assert_eq!(previous_retry(30), Duration::from_secs(3600));
/// ```
#[must_use]
pub fn previous_retry(tries: u32) -> Duration {
    PREVIOUS_RETRY_FIRST
        .saturating_mul(1 << tries.saturating_sub(1).min(16))
        .min(PREVIOUS_RETRY_MAX)
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

/// The limit set for `want` against the client's `own`: a rate is never above
/// an own limit in force, so `[limits]` only ever lowers the client.
///
/// ```
/// use mistarr_clients::RateLimit;
/// use mistarr_server::jobs::core_limits::lowered;
/// assert_eq!(lowered(RateLimit::kbps(64), RateLimit::kbps(40)), RateLimit::kbps(40));
/// assert_eq!(lowered(RateLimit::kbps(8), RateLimit::kbps(40)), RateLimit::kbps(8));
/// assert_eq!(lowered(RateLimit::kbps(64), RateLimit::default()), RateLimit::kbps(64));
/// assert_eq!(lowered(RateLimit::HELD, RateLimit::kbps(40)), RateLimit::HELD);
/// ```
#[must_use]
pub fn lowered(want: RateLimit, own: RateLimit) -> RateLimit {
    if want.is_held() || !own.enabled || own.kbps == 0 {
        want
    } else {
        RateLimit::kbps(want.kbps.min(own.kbps))
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
    /// Whether its alternate upload rate was asked for during this hold.
    alt_read: bool,
    /// Whether its alternate upload rate is held.
    alt_held: bool,
    /// Its own limits the gate replaced.
    saved: Option<SavedLimits>,
    /// Limits of clients no longer in use, still to be put back.
    previous: Vec<PreviousLimits>,
    /// Its stopped process.
    frozen: Option<Frozen>,
    /// A client that cannot be frozen, held by its uploads until the menu.
    refused: Option<ClientEndpoint>,
}

impl Applied {
    fn holding(&self) -> bool {
        self.frozen.is_some() || self.have.up == Some(RateLimit::HELD)
    }

    /// Forgets what was set, so the next step sets it again.
    fn forget(&mut self) {
        self.have = Target::default();
        self.alt_read = false;
        self.alt_held = false;
    }

    /// When a previous client's limits are next due for a try; never while
    /// the client is `frozen`, since no try runs then.
    fn previous_at(&self, frozen: bool) -> Option<Instant> {
        let now = crate::unix_now();
        let next = self.previous.iter().map(|p| p.next_at).min()?;
        let wait = u64::try_from(next.saturating_sub(now)).unwrap_or(0);
        (!frozen).then(|| Instant::now() + Duration::from_secs(wait))
    }
}

/// Failures in a row of one kind of work: the first is a warning, the rest
/// debug, and success after them info.
#[derive(Debug, Default)]
struct Streak {
    failures: u32,
}

impl Streak {
    /// Records the outcome; the time to try again after a failure.
    fn record(
        &mut self,
        outcome: &Result<(), Failure>,
        base: Duration,
        recovered: &str,
    ) -> Option<Instant> {
        match outcome {
            Ok(()) => {
                if self.failures > 0 {
                    tracing::info!("{recovered}");
                }
                self.failures = 0;
                None
            }
            Err(f) => {
                self.failures += 1;
                if self.failures == 1 {
                    tracing::warn!(error = %f.error, "{}", f.what);
                } else {
                    tracing::debug!(error = %f.error, failures = self.failures, "{}", f.what);
                }
                Some(Instant::now() + backoff(base, self.failures))
            }
        }
    }
}

/// Follows the gate: at the menu the menu limits and the client's own limits
/// elsewhere; while a core runs the core limits, and with
/// `transfer.pause_client_while_playing` a client on the board stopped or one
/// elsewhere with its uploads held. What the client does not take is retried
/// with a backoff from [`crate::app::Options::corename_poll`] to [`RETRY_MAX`],
/// and a held client is checked every [`crate::app::Options::hold_recheck`].
/// Client work kept for later runs when the client is not frozen, with a
/// backoff of its own.
pub async fn follow_gate(app: Arc<AppState>) {
    let mut rx = app.gate.subscribe();
    let base = app.options.corename_poll;
    let mut applied: Option<Applied> = None;
    let (mut hold_streak, mut replay_streak) = (Streak::default(), Streak::default());
    let (mut replay_retry, mut recheck_at) = (None, None::<Instant>);
    loop {
        rx.borrow_and_update();
        #[cfg(test)]
        app.gate_passes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if recheck_at.is_some_and(|at| at <= Instant::now()) {
            recheck_at = None;
            if let Some(a) = applied.as_mut() {
                recheck_hold(&app, a).await;
            }
        }
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
        let hold_retry = hold_streak.record(
            &outcome,
            base,
            "the download client took its core-gate hold",
        );
        recheck_at = if applied.as_ref().is_some_and(Applied::holding) {
            recheck_at.or_else(|| Some(Instant::now() + app.options.hold_recheck))
        } else {
            None
        };
        let frozen = app.client_frozen();
        if !frozen && replay_retry.is_none_or(|at: Instant| at <= Instant::now()) {
            let replayed = replay_deferred(&app).await;
            replay_retry = replay_streak.record(
                &replayed,
                base,
                "the download client took the work kept for it",
            );
        }
        let previous = applied.as_ref().and_then(|a| a.previous_at(frozen));
        // Only deadlines whose work runs now: a skipped one would leave a past deadline and spin.
        let replay_at = replay_retry.filter(|_| !frozen);
        let wake = [hold_retry, recheck_at, replay_at, previous]
            .into_iter()
            .flatten()
            .min();
        tokio::select! {
            r = rx.changed() => {
                if r.is_err() {
                    return;
                }
            }
            () = app.limits_wake.notified() => {}
            () = tokio::time::sleep_until(wake.unwrap_or_else(Instant::now)), if wake.is_some() => {}
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
    let previous = app
        .db
        .read(|c| settings::get_json::<Vec<PreviousLimits>>(c, keys::CLIENT_PREVIOUS_LIMITS))
        .await
        .map_err(failed("cannot read the limits saved for a previous client"))?
        .unwrap_or_default();
    let file = app.options.frozen_file.clone();
    let frozen = match blocking(move || freeze::read_file(&file, freeze::euid())).await? {
        Ok(frozen) => frozen,
        Err(e @ FreezeError::Untrusted(_)) => {
            tracing::warn!(error = %e, "ignoring the frozen client record");
            None
        }
        Err(e) => return Err(failed("cannot read the frozen client record")(e)),
    };
    Ok(Applied {
        client: None,
        have: Target::default(),
        alt_read: false,
        alt_held: false,
        saved,
        previous,
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

/// Moves the client towards what the gate wants now, then, unless the client
/// is frozen, puts back a previous client's limits that are due.
async fn step(app: &Arc<AppState>, a: &mut Applied) -> Result<(), Failure> {
    let held = hold(app, a).await;
    if !app.client_frozen() {
        restore_previous(app, a, false).await;
    }
    held
}

/// Moves the client towards what the gate wants now.
async fn hold(app: &Arc<AppState>, a: &mut Applied) -> Result<(), Failure> {
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
        if let Some(saved) = a.saved.clone() {
            set_aside(app, a, saved).await?;
        }
        return Ok(());
    };
    if a.client.as_ref() != Some(&id) {
        a.client = Some(id.clone());
        a.forget();
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
            pid
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
    let (lock, stop) = (Arc::clone(&app.freeze_lock), app.shutdown_signal());
    let done = blocking(move || {
        let _one = lock.lock().unwrap_or_else(PoisonError::into_inner);
        if *stop.borrow() {
            return Err(FreezeError::ShuttingDown);
        }
        freeze::freeze(&proc, &kill, &file, pid)
    })
    .await;
    match done {
        Ok(Ok(frozen)) => {
            announce(app).await;
            Ok(Ok(frozen))
        }
        Ok(Err(e @ (FreezeError::Kill(_) | FreezeError::ShuttingDown))) => {
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
        Err(e @ (FreezeError::Gone(_) | FreezeError::Reused(_) | FreezeError::NotClient(_))) => {
            tracing::warn!(error = %e, "the paused download client is gone; nothing to resume");
            defer(app, Op::Detect).await;
        }
        Err(e) => return Err(failed("cannot resume the download client")(e)),
    }
    let file = app.options.frozen_file.clone();
    blocking(move || freeze::remove_file(&file))
        .await?
        .map_err(failed("cannot remove the frozen client record"))?;
    a.frozen = None;
    publish(app, None).await;
    app.poll_wake.notify_one();
    crate::jobs::transfer::kick(app).await;
    Ok(())
}

/// Keeps client work that did not reach the client, frozen, missing or
/// refusing it; the core gate runs it once the client answers, or at the next
/// start when mistarr stops first.
pub async fn defer(app: &AppState, op: Op) {
    let now = crate::unix_now();
    match app.db.write(move |c| deferred::add(c, op, now)).await {
        Ok(true) => {
            tracing::debug!(?op, "client work waits for the client to resume");
            // The client may have resumed in between; the gate then runs it now.
            app.limits_wake.notify_one();
        }
        Ok(false) => {}
        Err(e) => tracing::warn!(error = %e, "cannot keep client work for when the client resumes"),
    }
}

/// Runs the client work [`defer`] kept: detection first, so the rest goes to
/// the client that answers now, then the seed policy of every source in the
/// client, selections and releases. Work that succeeded is removed and work
/// the client refused counts a try, a failure the gate retries with a
/// backoff; with no client all of it waits.
async fn replay_deferred(app: &Arc<AppState>) -> Result<(), Failure> {
    let waiting = app
        .db
        .read(deferred::get)
        .await
        .map_err(failed("cannot read the client work kept for the client"))?;
    if waiting.is_empty() {
        return Ok(());
    }
    let (mut done, mut refused) = (Vec::new(), Vec::new());
    let mut outcome = |op: Op, ok: bool| if ok { done.push(op) } else { refused.push(op) };
    let ops: Vec<Op> = waiting.iter().map(|e| e.op).collect();
    if ops.contains(&Op::Detect) {
        let found = crate::jobs::detect_client::detect_and_store(app, false).await;
        if let Err(e) = &found {
            tracing::warn!(error = %e, "cannot detect the download client");
        }
        outcome(Op::Detect, found.is_ok());
    }
    if let Some(client) = app.client() {
        for op in ops {
            let ok = match op {
                Op::Seed => apply_seed_policies(app, client.as_ref()).await,
                Op::Deselect(source) => crate::jobs::transfer::deselect(app, source)
                    .await
                    .map_err(|e| tracing::debug!(source = %source, error = %e, "cannot apply a kept selection"))
                    .unwrap_or(false),
                Op::Release(source) => crate::jobs::import::release_source(app, source).await,
                _ => continue,
            };
            outcome(op, ok);
        }
    }
    let left = !refused.is_empty();
    let now = crate::unix_now();
    let dropped = app
        .db
        .write(move |c| deferred::finish(c, &done, &refused, now))
        .await
        .map_err(failed("cannot record the client work kept for the client"))?;
    for e in dropped {
        tracing::warn!(op = ?e.op, tries = e.tries, "dropping client work the download client keeps refusing");
    }
    if left {
        return Err(Failure {
            what: "the download client did not take the work kept for it",
            error: "kept for another try".into(),
        });
    }
    Ok(())
}

/// Applies every source's seed policy from the database to the client; true
/// when the client took each one.
async fn apply_seed_policies(app: &AppState, client: &dyn DownloadClient) -> bool {
    let rows = match app.db.read(sources::list_in_client).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "cannot read the sources to apply their seed policies");
            return false;
        }
    };
    let mut all = true;
    for (source, cid, seed) in rows {
        match client
            .set_seed_policy(&ClientTorrentId::new(cid), seed)
            .await
        {
            Ok(()) | Err(ClientError::NotFound) => {}
            Err(e) => {
                tracing::warn!(source = %source, error = %e, "cannot apply the seed policy in the client");
                all = false;
            }
        }
    }
    all
}

/// Sets `want` in the client, never above its own limits. Before the gate
/// first changes a direction it saves the client's own limit there, and it
/// puts that limit back once the gate no longer sets the direction; a saved
/// limit is never replaced. Held uploads also hold the alternate upload rate.
async fn apply_limits(
    app: &AppState,
    a: &mut Applied,
    id: &ClientEndpoint,
    client: &dyn DownloadClient,
    want: Target,
) -> Result<(), Failure> {
    if let Some(old) = a.saved.clone().filter(|s| s.client != *id) {
        set_aside(app, a, old).await?;
    }
    if a.saved.is_none() {
        take_back(app, a, id).await?;
    }
    let mut saved = a
        .saved
        .clone()
        .unwrap_or_else(|| SavedLimits::empty(id.clone()));
    let mut read = false;
    if DIRECTIONS
        .iter()
        .any(|d| want.get(*d).is_some() && saved.get(*d).is_none())
    {
        // The same daemon may be set aside under another address; its own limits come back first.
        restore_previous(app, a, true).await;
    }
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
    let hold_alt = want.up == Some(RateLimit::HELD);
    if hold_alt && saved.alt_up.is_none() && !a.alt_read {
        // A client that does not report the rate is held through its upload limit alone.
        let alt = client.alt_up_limit().await.unwrap_or_else(|e| {
            tracing::debug!(error = %e, "cannot read the client's alternate upload rate");
            None
        });
        a.alt_read = true;
        if let Some(alt) = alt {
            saved.alt_up = Some(alt.kbps);
            read = true;
        }
    }
    if read {
        persist(app, Some(&saved)).await?;
        a.saved = Some(saved.clone());
    }
    for dir in DIRECTIONS {
        let own = saved.get(dir);
        let want = want.get(dir).map(|w| own.map_or(w, |own| lowered(w, own)));
        match (want, own) {
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
                keep(app, a, &saved).await?;
            }
            _ => {}
        }
    }
    match (hold_alt, saved.alt_up) {
        (true, Some(_)) if !a.alt_held => {
            client
                .set_alt_up_rate(0)
                .await
                .map_err(failed("cannot hold the client's alternate upload rate"))?;
            a.alt_held = true;
        }
        (false, Some(own)) => {
            client
                .set_alt_up_rate(own)
                .await
                .map_err(failed("cannot restore the client's alternate upload rate"))?;
            a.alt_held = false;
            a.alt_read = false;
            saved.alt_up = None;
            keep(app, a, &saved).await?;
        }
        _ => {}
    }
    Ok(())
}

/// Stores what is still saved, or drops the record once nothing is.
async fn keep(app: &AppState, a: &mut Applied, saved: &SavedLimits) -> Result<(), Failure> {
    let keep = (!saved.is_empty()).then_some(saved);
    persist(app, keep).await?;
    a.saved = keep.cloned();
    Ok(())
}

/// Puts `saved` back in the client it names, through a handle built for it.
async fn restore_saved(app: &AppState, saved: &SavedLimits) -> Result<(), Failure> {
    let key = ClientKey {
        kind: saved.client.kind,
        url: saved.client.url.clone(),
        path_map: app.config().client.remote_path_map.clone(),
    };
    let client = key.build().ok_or_else(|| Failure {
        what: "cannot restore the client's own limits",
        error: format!("cannot reach the {} client", saved.client.kind),
    })?;
    restore_into(client.as_ref(), saved).await
}

/// Sets each saved limit in `client`.
async fn restore_into(client: &dyn DownloadClient, saved: &SavedLimits) -> Result<(), Failure> {
    for dir in DIRECTIONS {
        if let Some(own) = saved.get(dir) {
            client
                .set_rate_limit(dir, own)
                .await
                .map_err(failed("cannot restore the client's own limits"))?;
        }
    }
    if let Some(own) = saved.alt_up {
        client
            .set_alt_up_rate(own)
            .await
            .map_err(failed("cannot restore the client's own limits"))?;
    }
    Ok(())
}

/// Moves `saved` from the current record to the previous clients' list, to be
/// put back by [`restore_previous`] without holding up the current client.
async fn set_aside(app: &AppState, a: &mut Applied, saved: SavedLimits) -> Result<(), Failure> {
    let now = crate::unix_now();
    let mut previous = a.previous.clone();
    previous.retain(|p| p.saved.client != saved.client);
    previous.push(PreviousLimits {
        saved,
        since: now,
        tries: 0,
        next_at: now,
    });
    persist_previous(app, &previous).await?;
    a.previous = previous;
    persist(app, None).await?;
    a.saved = None;
    Ok(())
}

/// Takes back limits set aside for `id` when it is the client again, so its
/// own limits are never read while they are still held.
async fn take_back(app: &AppState, a: &mut Applied, id: &ClientEndpoint) -> Result<(), Failure> {
    let Some(pos) = a.previous.iter().position(|p| p.saved.client == *id) else {
        return Ok(());
    };
    let mut previous = a.previous.clone();
    let back = previous.remove(pos).saved;
    persist(app, Some(&back)).await?;
    a.saved = Some(back);
    persist_previous(app, &previous).await?;
    a.previous = previous;
    Ok(())
}

/// Tries each previous client's limits that are due, each for at most
/// [`RESTORE_WAIT`]. A failed try waits [`previous_retry`] for the next, and
/// one failing [`PREVIOUS_KEEP`] after they were set aside drops them.
async fn restore_previous(app: &AppState, a: &mut Applied, all: bool) {
    let now = crate::unix_now();
    if !a.previous.iter().any(|p| all || p.next_at <= now) {
        return;
    }
    let mut gate = app.gate.subscribe();
    let mut moved = false;
    let keep_secs = i64::try_from(PREVIOUS_KEEP.as_secs()).unwrap_or(i64::MAX);
    let mut left = Vec::new();
    for mut p in a.previous.clone() {
        if moved || (!all && p.next_at > now) {
            left.push(p);
            continue;
        }
        // A gate change ends the try, so a core starting meanwhile is acted on at once.
        let tried = tokio::select! {
            r = tokio::time::timeout(RESTORE_WAIT, restore_saved(app, &p.saved)) => Some(r),
            _ = gate.changed() => None,
        };
        let Some(tried) = tried else {
            moved = true;
            left.push(p);
            continue;
        };
        let error = match tried {
            Ok(Ok(())) => {
                tracing::info!("restored the limits of a download client no longer in use");
                continue;
            }
            Ok(Err(f)) => f.error,
            Err(_) => "no answer".to_owned(),
        };
        p.tries += 1;
        if now.saturating_sub(p.since) >= keep_secs {
            tracing::warn!(error = %error, tries = p.tries, "dropping the limits saved for a download client no longer in use");
            continue;
        }
        tracing::debug!(error = %error, tries = p.tries, "cannot restore the limits of a download client no longer in use");
        let wait = i64::try_from(previous_retry(p.tries).as_secs()).unwrap_or(i64::MAX);
        p.next_at = now.saturating_add(wait);
        left.push(p);
    }
    if let Err(f) = persist_previous(app, &left).await {
        tracing::warn!(error = %f.error, "{}", f.what);
    }
    a.previous = left;
}

async fn persist_previous(app: &AppState, previous: &[PreviousLimits]) -> Result<(), Failure> {
    let previous = previous.to_vec();
    app.db
        .write(move |c| {
            if previous.is_empty() {
                settings::remove(c, keys::CLIENT_PREVIOUS_LIMITS)
            } else {
                settings::set_json(c, keys::CLIENT_PREVIOUS_LIMITS, &previous)
            }
        })
        .await
        .map_err(failed("cannot save the limits of a previous client"))
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
/// again, one that exited or was replaced is let go so the next step finds its
/// successor, and uploads that left their hold are held again.
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
                let (proc, kill) = (app.options.proc_dir.clone(), Kill::new(&app.options.kill));
                let (lock, stop) = (Arc::clone(&app.freeze_lock), app.shutdown_signal());
                let sent = blocking(move || {
                    let _one = lock.lock().unwrap_or_else(PoisonError::into_inner);
                    if *stop.borrow() {
                        return Err(FreezeError::ShuttingDown);
                    }
                    freeze::stop_again(&proc, &kill, frozen)
                })
                .await;
                match sent {
                    Ok(Err(e @ FreezeError::NotClient(_))) => let_go(app, a, &e).await,
                    Ok(Err(e)) => {
                        tracing::warn!(error = %e, "cannot pause the download client again");
                    }
                    _ => {}
                }
            }
            Ok(Err(e)) => let_go(app, a, &e).await,
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
            a.forget();
        }
        Ok(_) => {}
        Err(e) => tracing::debug!(error = %e, "cannot check the client's upload hold"),
    }
}

/// Drops a frozen process that is no longer the client, without signalling it.
async fn let_go(app: &AppState, a: &mut Applied, why: &FreezeError) {
    tracing::warn!(error = %why, "the paused download client exited");
    defer(app, Op::Detect).await;
    let file = app.options.frozen_file.clone();
    if let Ok(Err(e)) = blocking(move || freeze::remove_file(&file)).await {
        tracing::warn!(error = %e, "cannot remove the frozen client record");
    }
    a.frozen = None;
    a.forget();
    publish(app, None).await;
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
    let frozen = match blocking(move || freeze::read_file(&file, freeze::euid())).await {
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

/// On shutdown, resumes a frozen client, so a stopped mistarr never leaves it
/// frozen. It waits for a stop in flight, and none starts once shutdown began.
pub async fn thaw_for_shutdown(app: &AppState) {
    let (file, proc, kill) = (
        app.options.frozen_file.clone(),
        app.options.proc_dir.clone(),
        Kill::new(&app.options.kill),
    );
    let lock = Arc::clone(&app.freeze_lock);
    let done = blocking(move || {
        let _one = lock.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(frozen) = freeze::read_file(&file, freeze::euid())? else {
            return Ok(false);
        };
        resume_now(&proc, &kill, &file, frozen)
    })
    .await;
    match done {
        Ok(Ok(true)) => {
            app.set_client_hold(None);
        }
        Ok(Ok(false)) => {}
        Ok(Err(e)) => tracing::warn!(error = %e, "cannot resume the download client"),
        Err(f) => tracing::warn!(error = %f.error, "{}", f.what),
    }
}

/// Sends SIGCONT and removes the record; true unless the signal failed.
async fn resume(app: &AppState, frozen: Frozen) -> bool {
    let (proc, kill, file) = (
        app.options.proc_dir.clone(),
        Kill::new(&app.options.kill),
        app.options.frozen_file.clone(),
    );
    let lock = Arc::clone(&app.freeze_lock);
    let done = blocking(move || {
        let _one = lock.lock().unwrap_or_else(PoisonError::into_inner);
        resume_now(&proc, &kill, &file, frozen)
    })
    .await;
    match done {
        Ok(Ok(resumed)) => resumed,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "cannot resume the download client");
            false
        }
        Err(f) => {
            tracing::warn!(error = %f.error, "{}", f.what);
            false
        }
    }
}

/// Resumes `frozen` and removes its record; a process that is gone or no
/// longer the client only loses its record.
fn resume_now(
    proc: &std::path::Path,
    kill: &Kill,
    file: &std::path::Path,
    frozen: Frozen,
) -> Result<bool, FreezeError> {
    match freeze::thaw(proc, kill, frozen) {
        Ok(()) => tracing::info!(pid = frozen.pid, "download client resumed"),
        Err(e @ (FreezeError::Gone(_) | FreezeError::Reused(_) | FreezeError::NotClient(_))) => {
            tracing::warn!(error = %e, "the paused download client is gone; nothing to resume");
        }
        Err(e) => return Err(e),
    }
    if let Err(e) = freeze::remove_file(file) {
        tracing::warn!(error = %e, "cannot remove the frozen client record");
    }
    Ok(true)
}

#[cfg(test)]
mod tests;
