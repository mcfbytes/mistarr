//! Fetching one URL the user supplied into `dats/` or `sources/`; see `docs/ARCHITECTURE.md` "Fetching a URL".

pub mod content;
pub mod spool;

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use async_trait::async_trait;
use mistarr_clients::fetch::{FetchUrl, Fetcher, Limits, Roots};
use serde_json::Value;

use self::content::{Checked, Found, Refused, SNIFF_BYTES};
use self::spool::{Pace, Places, Spool};
use super::progress::Progress;
use super::{Job, JobContext, JobKind, Lane, Scheduler};
use crate::app::AppState;
use crate::db::ids::JobId;
use crate::error::{Error, Result};
use crate::incoming::place::{self, PlaceError, SourceFile};
use crate::threads::{label, run};

/// Why a fetched file was refused, whatever it turned out to be.
pub const NOT_ACCEPTED: &str = "This isn't a DAT, DAT pack or torrent file.";

/// The error of a gzip body the server sent unasked.
pub const COMPRESSED: &str = "The server sent a compressed file mistarr can't read.";

/// The error of a zip that holds more than DATs; `docs/UI.md` says why fetches refuse it.
pub const OTHER_FILES: &str = "This zip holds files other than DATs.";

/// The error of a fetch the card has no room for.
pub const CARD_FULL: &str = "The card has too little free space for the file.";

/// The subdirectory of SQLite's temporary directory that holds fetches in RAM, mistarr's
/// own, so the startup sweep never touches another program's files.
pub const RAM_SUBDIR: &str = "mistarr-fetch";

/// The shortest and longest rest after each MiB written to the card while a core runs.
const REST_MIN: Duration = Duration::from_millis(20);
const REST_MAX: Duration = Duration::from_secs(1);

/// Where a fetch's token stands: not yet tied to its job, with any cancel asked for
/// meanwhile, or tied to the job the scheduler cancels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    Waiting { cancelled: bool },
    Job(JobId),
}

/// The fetches queued or running, by the token the API hands out; nothing of the URL.
/// A token maps to its job, which [`Scheduler::cancel`] stops.
#[derive(Debug)]
pub struct Fetches {
    open: Mutex<HashMap<u64, Slot>>,
    next: AtomicU64,
}

/// Bits of a token below its per-process nonce: fetches one run may start.
const COUNTER_BITS: u32 = 20;

impl Default for Fetches {
    /// Tokens start at a random nonce, below 2^53 so JavaScript holds them exactly, and
    /// count up, so a page open across a restart never matches an old one.
    fn default() -> Self {
        use std::hash::BuildHasher;
        let random = std::collections::hash_map::RandomState::new().hash_one(std::process::id());
        let nonce = (random & 0xFFFF_FFFF) << COUNTER_BITS;
        Self {
            open: Mutex::new(HashMap::new()),
            next: AtomicU64::new(nonce),
        }
    }
}

impl Fetches {
    /// A new token, open for cancelling until [`Fetches::close`].
    ///
    /// ```
    /// let fetches = mistarr_server::jobs::url_fetch::Fetches::default();
    /// let scheduler = mistarr_server::jobs::Scheduler::new();
    /// let token = fetches.issue();
    /// assert!(fetches.cancel(token, &scheduler), "asked before its job is known");
    /// fetches.close(token);
    /// assert!(!fetches.cancel(token, &scheduler));
    /// ```
    pub fn issue(&self) -> u64 {
        let token = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        self.lock()
            .insert(token, Slot::Waiting { cancelled: false });
        token
    }

    /// Ties `token` to its job once recorded, passing on a cancel asked for before;
    /// a closed token is left closed.
    pub fn bind(&self, token: u64, id: JobId, scheduler: &Scheduler) {
        let mut open = self.lock();
        let Some(slot) = open.get_mut(&token) else {
            return;
        };
        if *slot == (Slot::Waiting { cancelled: true }) {
            scheduler.cancel(id);
        }
        *slot = Slot::Job(id);
    }

    /// Cancels fetch `token`; false when no such fetch is open.
    pub fn cancel(&self, token: u64, scheduler: &Scheduler) -> bool {
        match self.lock().get_mut(&token) {
            Some(Slot::Job(id)) => scheduler.cancel(*id),
            Some(slot) => {
                *slot = Slot::Waiting { cancelled: true };
                true
            }
            None => false,
        }
    }

    /// Forgets fetch `token` once it has ended, or was never queued.
    pub fn close(&self, token: u64) {
        self.lock().remove(&token);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Slot>> {
        self.open.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Fetches one URL once and places the file as an upload would. The URL lives only in
/// this value: the payload holds the token, and progress the file name once known.
/// Dropping it closes the token, whether or not the job ran.
pub struct UrlFetch {
    url: FetchUrl,
    token: u64,
    app: Weak<AppState>,
}

/// What a [`UrlFetch`] stores as its payload: the token, never the URL.
#[derive(serde::Serialize)]
struct Payload {
    fetch: u64,
}

impl UrlFetch {
    /// A fetch of `url` with a token from `app`'s fetches, to [`Fetches::bind`]
    /// once it is queued.
    #[must_use]
    pub fn new(app: &Arc<AppState>, url: FetchUrl) -> Self {
        let token = app.fetches.issue();
        Self {
            url,
            token,
            app: Arc::downgrade(app),
        }
    }

    /// The token `DELETE /fetch/{token}` cancels it by.
    #[must_use]
    pub fn token(&self) -> u64 {
        self.token
    }
}

/// What a fetch reports as it goes.
#[derive(Debug, Clone, Default)]
struct View {
    token: u64,
    received: u64,
    total: Option<u64>,
    file: Option<String>,
}

impl View {
    fn progress(&self, phase: &str) -> Progress {
        let mut p = Progress::phase(phase).with("token", self.token);
        if !matches!(phase, "checking" | "placing") {
            p = p.bytes(self.received, self.total);
        }
        if let Some(f) = &self.file {
            p = p.with("file", f);
        }
        p
    }
}

fn too_large(cap: u64, what: &str) -> Error {
    Error::FetchRefused(format!(
        "The file is larger than {} MiB, the most {what} may be.",
        cap >> 20
    ))
}

fn what(found: Option<Found>) -> &'static str {
    match found {
        Some(Found::Torrent) => "a torrent",
        _ => "a DAT or DAT pack",
    }
}

/// The type `head` announces, refused when it is none or the announced length exceeds its cap.
fn identify(head: &[u8], total: Option<u64>) -> Result<Found> {
    if content::is_gzip(head) {
        return Err(Error::FetchRefused(COMPRESSED.to_owned()));
    }
    let found = content::sniff(head).ok_or_else(|| Error::FetchRefused(NOT_ACCEPTED.to_owned()))?;
    if total.is_some_and(|t| t > found.cap()) {
        return Err(too_large(found.cap(), what(Some(found))));
    }
    Ok(found)
}

/// The name to place a file of type `found` under: the server's name for it, made safe,
/// with the extension its type needs; `download` when there is none.
///
/// ```
/// use mistarr_server::jobs::url_fetch::{content::Found, placed_name};
/// assert_eq!(placed_name(Some("../Set.XML"), Found::Xml), "Set.xml");
/// assert_eq!(placed_name(Some("get.php"), Found::Zip), "get.php.zip");
/// assert_eq!(placed_name(None, Found::Torrent), "download.torrent");
/// ```
#[must_use]
pub fn placed_name(hint: Option<&str>, found: Found) -> String {
    let given = hint.unwrap_or("");
    let is_xml = std::path::Path::new(given)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("xml"));
    let ext = match found {
        Found::Torrent => "torrent",
        Found::Zip => "zip",
        Found::Xml if is_xml => "xml",
        Found::Xml => "dat",
    };
    crate::incoming::place::file_name(given, "download", ext)
}

impl UrlFetch {
    /// Runs `work` until it ends, the fetch is cancelled or the server shuts down.
    async fn until_stopped<T>(ctx: &JobContext, work: impl Future<Output = T>) -> Result<T> {
        tokio::select! {
            out = work => Ok(out),
            e = ctx.stop.until_stopped() => Err(e),
        }
    }

    /// A fetcher trusting `Options::ca_file`, else [`Roots::system_or_bundled`]; roots are loaded
    /// for http links too, which may redirect to https.
    async fn fetcher(&self, app: &AppState) -> Result<Fetcher> {
        let ca = app.options.ca_file.clone();
        let loaded = run(label::FETCH, move || match ca {
            Some(path) => Roots::from_pem_file(&path).map_err(|e| (path, e)),
            None => Ok(Roots::system_or_bundled()),
        })
        .await?;
        let roots = match loaded {
            Ok(roots) => roots,
            Err((path, e)) => {
                warn_once(|| {
                    tracing::warn!(path = %path.display(), error = %e, "the CA file cannot be read");
                });
                return Err(Error::CaFile(e));
            }
        };
        if let Some((path, why)) = roots.skipped() {
            warn_once(|| {
                tracing::warn!(
                    env = mistarr_clients::fetch::CERT_FILE_ENV,
                    path = %path.display(),
                    error = why,
                    "the named CA bundle cannot be read; using the system or built-in one"
                );
            });
        }
        tracing::debug!(origin = ?roots.origin(), count = roots.len(), "TLS roots");
        Fetcher::new(roots, Limits::default()).map_err(Error::from)
    }

    async fn fetch(&self, ctx: &JobContext) -> Result<()> {
        let app = &ctx.app;
        let reporter = ctx.reporter();
        let mut view = View {
            token: self.token,
            ..View::default()
        };
        reporter.report("connecting", || view.progress("connecting"));
        ctx.stop.stopped()?;
        tracing::debug!(host = self.url.host(), "fetching a URL");
        let fetcher = self.fetcher(app).await?;
        let mut resp = Self::until_stopped(ctx, fetcher.get(&self.url)).await??;
        view.total = resp.content_length();
        if view
            .total
            .is_some_and(|t| t > crate::jobs::dat_import::MAX_DAT_BYTES)
        {
            return Err(too_large(
                crate::jobs::dat_import::MAX_DAT_BYTES,
                what(None),
            ));
        }
        let hint = resp.file_name().map(str::to_owned);
        let pace = pace(app);
        let mut spool =
            Spool::create(places(app), self.token, view.total, Arc::clone(&pace)).await?;
        let mut head = Vec::with_capacity(SNIFF_BYTES);
        let mut found = None;
        while let Some(chunk) = Self::until_stopped(ctx, resp.chunk()).await?? {
            view.received += chunk.len() as u64;
            if found.is_none() {
                let take = chunk.len().min(SNIFF_BYTES - head.len());
                head.extend_from_slice(&chunk[..take]);
                if head.len() == SNIFF_BYTES {
                    let f = identify(&head, view.total)?;
                    view.file = Some(placed_name(hint.as_deref(), f));
                    found = Some(f);
                }
            }
            let cap = found.map_or(crate::jobs::dat_import::MAX_DAT_BYTES, Found::cap);
            if view.received > cap {
                return Err(too_large(cap, what(found)));
            }
            spool.push(&chunk).await?;
            reporter.report("receiving", || view.progress("receiving"));
        }
        let found = match found {
            Some(f) => f,
            None => identify(&head, view.total)?,
        };
        let name = placed_name(hint.as_deref(), found);
        view.file = Some(name.clone());
        spool.finish().await?;
        reporter.report("checking", || view.progress("checking"));
        let checked = self.check(ctx, found, &mut spool).await?;
        ctx.stop.stopped()?;
        reporter.report("placing", || view.progress("placing"));
        let placed = match checked {
            Checked::Torrent { bytes, infohash } => {
                drop(spool);
                let file = SourceFile {
                    name,
                    bytes,
                    infohash,
                    is_torrent: true,
                };
                match place::place_source(app, file).await {
                    Err(Error::Place(e @ (PlaceError::Duplicate | PlaceError::NoFreeName))) => {
                        return Err(Error::FetchRefused(e.to_string()))
                    }
                    placed => placed?,
                }
            }
            Checked::Dat { .. } => {
                let dir = app.config().paths.dats();
                std::fs::create_dir_all(&dir).map_err(crate::Error::io_at(&dir))?;
                let part = place::part_path(&dir);
                spool.place(part.clone(), ctx.stop.clone()).await?;
                place::place_part(app, &part, &name).await?
            }
        };
        let done = view
            .progress("placed")
            .with("file", &placed.file)
            .with("target", found.target())
            .with("placed", &placed);
        ctx.progress(done.into_value()).await
    }

    /// Checks the spooled file on a blocking thread, stopping when cancelled; a DAT or
    /// pack is rewritten by mistarr and the spool then holds the rewrite alone.
    async fn check(&self, ctx: &JobContext, found: Found, spool: &mut Spool) -> Result<Checked> {
        let path = spool.path().to_path_buf();
        let target = spool.target(crate::jobs::dat_import::MAX_DAT_BYTES);
        let stop = ctx.stop.clone();
        let checked = run(label::FETCH, move || {
            content::check(found, &path, &target, &|| stop.is_stopped())
        })
        .await?;
        match checked {
            Ok(Checked::Dat { path, in_ram }) => {
                spool.adopt(path.clone(), in_ram);
                Ok(Checked::Dat { path, in_ram })
            }
            Ok(c) => Ok(c),
            Err(Refused::TooLarge) => Err(too_large(
                content::MAX_UNPACKED_BYTES,
                "a DAT or DAT pack unpacked",
            )),
            Err(Refused::NoRoom) => Err(Error::FetchRefused(CARD_FULL.to_owned())),
            Err(Refused::NotAccepted(why)) => {
                tracing::debug!(why, "a fetched file was refused");
                Err(Error::FetchRefused(NOT_ACCEPTED.to_owned()))
            }
            Err(Refused::OtherFiles) => Err(Error::FetchRefused(OTHER_FILES.to_owned())),
            Err(Refused::Stopped) => {
                ctx.stop.stopped()?;
                Err(Error::CancelledByUser)
            }
            Err(Refused::Io(e)) => Err(e.into()),
        }
    }
}

/// Logs `warn` the first time it is called in this process.
fn warn_once(warn: impl FnOnce()) {
    static WARNED: AtomicBool = AtomicBool::new(false);
    if !WARNED.swap(true, Ordering::Relaxed) {
        warn();
    }
}

/// The rest after each MiB written to the card: while a core runs, as long as the write
/// took, within [`REST_MIN`] and [`REST_MAX`]; otherwise none.
fn pace(app: &AppState) -> Pace {
    let gate = Arc::clone(&app.gate);
    Arc::new(move |took: Duration| {
        if gate.state().core_running() {
            took.clamp(REST_MIN, REST_MAX)
        } else {
            Duration::ZERO
        }
    })
}

/// A card that filled up during a fetch, as the fetch's error.
fn card_full(e: Error) -> Error {
    match e {
        e if e
            .io()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::StorageFull) =>
        {
            Error::FetchRefused(CARD_FULL.to_owned())
        }
        e => e,
    }
}

/// mistarr's own directory for fetches in RAM, under SQLite's temporary directory when
/// that is set and is not `card`.
#[must_use]
pub fn ram_dir(card: &std::path::Path) -> Option<PathBuf> {
    std::env::var_os(crate::db::tempdir::SQLITE_TMPDIR)
        .map(PathBuf::from)
        .filter(|d| d != card)
        .map(|d| d.join(RAM_SUBDIR))
}

/// Where a spool may go: mistarr's directory in RAM when there is one, else the card.
fn places(app: &AppState) -> Places {
    let config = app.config();
    let card = config.paths.tmp();
    let ram = ram_dir(&card);
    Places {
        ram,
        card,
        floor: config.memory.import_floor_mib.saturating_mul(1024 * 1024),
    }
}

#[async_trait]
impl Job for UrlFetch {
    fn kind(&self) -> JobKind {
        JobKind::UrlFetch
    }

    fn payload(&self) -> Value {
        super::to_payload(&Payload { fetch: self.token })
    }

    fn lane(&self) -> Lane {
        Lane::Fetch
    }

    async fn run(&self, ctx: &JobContext) -> Result<()> {
        ctx.app.fetches.bind(self.token, ctx.id, &ctx.app.scheduler);
        self.fetch(ctx).await.map_err(card_full)
    }
}

impl Drop for UrlFetch {
    fn drop(&mut self) {
        if let Some(app) = self.app.upgrade() {
            app.fetches.close(self.token);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn names_take_the_extension_of_their_type() {
        assert_eq!(placed_name(Some("a.dat"), Found::Xml), "a.dat");
        assert_eq!(placed_name(Some("a"), Found::Xml), "a.dat");
        assert_eq!(placed_name(Some(".hidden.zip"), Found::Zip), "hidden.zip");
        assert_eq!(
            placed_name(Some("x/y.torrent"), Found::Torrent),
            "y.torrent"
        );
        assert_eq!(placed_name(Some(""), Found::Zip), "download.zip");
    }

    #[test]
    fn the_type_and_its_cap_are_checked_together() {
        assert!(identify(b"d8:announce", Some(10)).is_ok());
        let e = identify(b"d8:announce", Some(17 << 20)).expect_err("too large");
        assert!(
            e.to_string().contains("16 MiB, the most a torrent may be"),
            "{e}"
        );
        let e = identify(b"<html>", None).expect_err("html");
        assert_eq!(e.to_string(), NOT_ACCEPTED);
        let e = identify(b"\x1f\x8b\x08\0", None).expect_err("gzip");
        assert_eq!(e.to_string(), COMPRESSED);
    }

    #[test]
    fn tokens_differ_across_runs_and_a_late_bind_of_an_ended_fetch_is_dropped() {
        let (a, b) = (Fetches::default(), Fetches::default());
        let scheduler = Scheduler::new();
        let (ta, tb) = (a.issue(), b.issue());
        assert_ne!(ta, tb, "two runs start at different nonces");
        assert!(ta < 1 << 53 && tb < 1 << 53);
        let t = a.issue();
        assert_eq!(t, ta + 1);
        a.close(t);
        a.bind(t, JobId::new(4), &scheduler);
        assert!(!a.cancel(t, &scheduler), "an ended fetch is not kept open");
    }

    /// Binds its token as a fetch does, cancels it once bound when `late`, and stops
    /// where the job must.
    struct Probe {
        token: u64,
        late: bool,
    }

    #[async_trait]
    impl Job for Probe {
        fn kind(&self) -> JobKind {
            JobKind::UrlFetch
        }
        fn lane(&self) -> Lane {
            Lane::Fetch
        }
        async fn run(&self, ctx: &JobContext) -> Result<()> {
            let fetches = &ctx.app.fetches;
            fetches.bind(self.token, ctx.id, &ctx.app.scheduler);
            if self.late {
                assert!(fetches.cancel(self.token, &ctx.app.scheduler));
            }
            let stopped = ctx.stop.stopped();
            fetches.close(self.token);
            stopped
        }
    }

    #[tokio::test]
    async fn a_cancel_reaches_the_job_before_and_after_it_is_known() {
        let (_dir, app) = crate::app::testutil::state();
        let early = app.fetches.issue();
        assert!(app.fetches.cancel(early, &app.scheduler));
        for (token, late) in [(early, false), (app.fetches.issue(), true)] {
            let id = Scheduler::run_inline(&app, Arc::new(Probe { token, late }))
                .await
                .expect("run");
            let row = app.db.read(move |c| crate::db::jobs::get(c, id)).await;
            let row = row.expect("get").expect("row");
            assert_eq!(row.progress, Some(json!({ "error": "Cancelled." })));
            assert!(!app.fetches.cancel(token, &app.scheduler), "closed");
        }
    }

    /// Holds the fetch lane until released.
    struct Hold(Arc<tokio::sync::Notify>);

    #[async_trait]
    impl Job for Hold {
        fn kind(&self) -> JobKind {
            JobKind::UrlFetch
        }
        fn lane(&self) -> Lane {
            Lane::Fetch
        }
        async fn run(&self, _ctx: &JobContext) -> Result<()> {
            self.0.notified().await;
            Ok(())
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_fetch_cancelled_while_queued_closes_its_token() {
        let (_dir, app) = crate::app::testutil::state();
        Scheduler::start(&app);
        let release = Arc::new(tokio::sync::Notify::new());
        Scheduler::enqueue(&app, Arc::new(Hold(Arc::clone(&release))))
            .await
            .expect("hold");
        let url = FetchUrl::parse("https://example.invalid/set.dat").expect("url");
        let job = UrlFetch::new(&app, url);
        let token = job.token();
        let id = Scheduler::enqueue(&app, Arc::new(job))
            .await
            .expect("queue");
        app.fetches.bind(token, id, &app.scheduler);
        assert!(app.fetches.cancel(token, &app.scheduler));
        release.notify_one();
        crate::testing::eventually("the token to close", || async {
            !app.fetches.cancel(token, &app.scheduler)
        })
        .await;
        let row = app.db.read(move |c| crate::db::jobs::get(c, id)).await;
        let row = row.expect("get").expect("row");
        assert_eq!(row.state, crate::db::jobs::JobState::Failed);
        assert_eq!(row.progress, Some(json!({ "error": "Cancelled." })));
    }

    #[test]
    fn a_full_card_is_named_and_other_errors_pass() {
        let full = card_full(std::io::Error::from(std::io::ErrorKind::StorageFull).into());
        assert_eq!(full.to_string(), CARD_FULL);
        let other = card_full(Error::FetchRefused("x".into()));
        assert_eq!(other.to_string(), "x");
        let card = std::path::Path::new("/nonexistent/card");
        if let Some(dir) = ram_dir(card) {
            assert!(dir.ends_with(RAM_SUBDIR));
        }
    }

    #[test]
    fn a_view_names_only_what_is_known() {
        let mut v = View {
            token: 3,
            ..View::default()
        };
        let value = |v: &View, phase| v.progress(phase).into_value();
        assert_eq!(
            value(&v, "connecting"),
            json!({ "token": 3, "phase": "connecting", "bytes": 0 })
        );
        v.total = Some(9);
        v.file = Some("a.dat".into());
        assert_eq!(value(&v, "receiving")["bytes_total"], 9);
        assert_eq!(value(&v, "receiving")["file"], "a.dat");
        for phase in ["checking", "placing"] {
            assert!(value(&v, phase).get("bytes").is_none());
            assert!(value(&v, phase).get("bytes_total").is_none());
        }
        assert_eq!(value(&v, "placed")["bytes_total"], 9);
    }
}
