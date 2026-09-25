//! Fetching one URL the user supplied into `dats/` or `sources/`; see `docs/ARCHITECTURE.md` "Fetching a URL".

pub mod content;
pub mod spool;

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use mistarr_clients::fetch::{FetchError, FetchUrl, Fetcher, Limits, Roots};
use serde_json::{json, Value};

use self::content::{Checked, Found, Refused, SNIFF_BYTES};
use self::spool::{Places, Spool};
use super::{Job, JobContext, Lane};
use crate::app::AppState;
use crate::error::{Error, Result};
use crate::threads::{blocking, label};

/// `jobs.kind` of [`UrlFetch`].
pub const KIND: &str = "url_fetch";

/// Why a fetched file was refused, whatever it turned out to be.
pub const NOT_ACCEPTED: &str = "This isn't a DAT, DAT pack or torrent file.";

/// The error of a fetch the user cancelled.
pub const CANCELLED: &str = "Cancelled.";

/// The error of a body that stopped short of the length the server announced.
pub const SHORT: &str = "The download ended early.";

/// The shortest and longest rest after each MiB written to the card while a core runs.
const REST_MIN: Duration = Duration::from_millis(20);
const REST_MAX: Duration = Duration::from_secs(1);

/// A fetch's cancel request, set by `DELETE /fetch/{token}`.
#[derive(Debug, Default)]
pub struct Cancel {
    set: AtomicBool,
    notify: tokio::sync::Notify,
}

impl Cancel {
    /// Asks the fetch to stop at its next chunk.
    pub fn cancel(&self) {
        self.set.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    /// Whether the fetch was asked to stop.
    #[must_use]
    pub fn is_set(&self) -> bool {
        self.set.load(Ordering::SeqCst)
    }

    /// Returns once the fetch is asked to stop.
    pub async fn wait(&self) {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_set() {
                return;
            }
            notified.await;
        }
    }
}

/// The fetches queued or running, by the token the API hands out; nothing of the URL.
#[derive(Debug, Default)]
pub struct Fetches {
    open: Mutex<HashMap<u64, Arc<Cancel>>>,
    next: AtomicU64,
}

impl Fetches {
    /// Registers a new fetch and returns its token and cancel flag.
    ///
    /// ```
    /// let fetches = mistarr_server::jobs::url_fetch::Fetches::default();
    /// let (token, flag) = fetches.open();
    /// assert!(fetches.cancel(token));
    /// assert!(flag.is_set());
    /// fetches.close(token);
    /// assert!(!fetches.cancel(token));
    /// ```
    pub fn open(&self) -> (u64, Arc<Cancel>) {
        let token = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let flag = Arc::new(Cancel::default());
        self.lock().insert(token, Arc::clone(&flag));
        (token, flag)
    }

    /// Cancels fetch `token`; false when no such fetch is open.
    pub fn cancel(&self, token: u64) -> bool {
        self.lock().get(&token).map(|c| c.cancel()).is_some()
    }

    /// Forgets fetch `token` once it has ended.
    pub fn close(&self, token: u64) {
        self.lock().remove(&token);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Arc<Cancel>>> {
        self.open.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Fetches one URL once and places the file as an upload would. The URL lives only in
/// this value: the payload holds the token, and progress the file name once known.
pub struct UrlFetch {
    url: FetchUrl,
    token: u64,
    cancel: Arc<Cancel>,
}

impl UrlFetch {
    /// A fetch of `url`, registered in `app`'s open fetches.
    #[must_use]
    pub fn new(app: &AppState, url: FetchUrl) -> Self {
        let (token, cancel) = app.fetches.open();
        Self { url, token, cancel }
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
    fn json(&self, phase: &str) -> Value {
        let mut v = json!({ "token": self.token, "phase": phase, "bytes_received": self.received });
        if let Some(t) = self.total {
            v["bytes_total"] = json!(t);
        }
        if let Some(f) = &self.file {
            v["file"] = json!(f);
        }
        v
    }
}

#[allow(clippy::needless_pass_by_value)] // Used as `map_err(fetch_error)`.
fn fetch_error(e: FetchError) -> Error {
    Error::Fetch(e.to_string())
}

fn too_large(cap: u64, what: &str) -> Error {
    Error::Fetch(format!(
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
    let found = content::sniff(head).ok_or_else(|| Error::Fetch(NOT_ACCEPTED.to_owned()))?;
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
    crate::http::sources_file_name(given, "download", ext)
}

impl UrlFetch {
    /// Fails with [`CANCELLED`] once cancelled, [`Error::Cancelled`] on shutdown.
    fn stop_point(&self, ctx: &JobContext) -> Result<()> {
        if self.cancel.is_set() {
            return Err(Error::Fetch(CANCELLED.to_owned()));
        }
        if *ctx.app.shutdown_signal().borrow() {
            return Err(Error::Cancelled);
        }
        Ok(())
    }

    /// Runs `work` until it ends, the fetch is cancelled or the server shuts down.
    async fn until_stopped<T>(&self, ctx: &JobContext, work: impl Future<Output = T>) -> Result<T> {
        let mut stop = ctx.app.shutdown_signal();
        tokio::select! {
            out = work => Ok(out),
            () = self.cancel.wait() => Err(Error::Fetch(CANCELLED.to_owned())),
            _ = stop.wait_for(|s| *s) => Err(Error::Cancelled),
        }
    }

    async fn fetcher(&self, app: &AppState) -> Result<Fetcher> {
        let ca = app.options.ca_file.clone();
        let secure = self.url.is_secure();
        let roots = blocking(label::FETCH, move || match ca {
            _ if !secure => Ok(Roots::empty()),
            Some(path) => Roots::from_pem_file(&path),
            None => Ok(Roots::system_or_bundled()),
        })
        .await
        .map_err(|e| Error::Task(e.to_string()))?
        .map_err(|e| Error::Fetch(format!("The CA bundle cannot be read: {e}.")))?;
        if secure {
            tracing::debug!(origin = ?roots.origin(), count = roots.len(), "TLS roots");
        }
        Fetcher::new(roots, Limits::default()).map_err(fetch_error)
    }

    async fn fetch(&self, ctx: &JobContext) -> Result<()> {
        let app = &ctx.app;
        let reporter = ctx.reporter();
        let mut view = View {
            token: self.token,
            ..View::default()
        };
        reporter.report("connecting", || view.json("connecting"));
        self.stop_point(ctx)?;
        tracing::debug!(host = self.url.host(), "fetching a URL");
        let fetcher = self.fetcher(app).await?;
        let mut resp = self
            .until_stopped(ctx, fetcher.get(&self.url))
            .await?
            .map_err(fetch_error)?;
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
        let mut spool = Spool::create(places(app), self.token, view.total).await?;
        let mut head = Vec::with_capacity(SNIFF_BYTES);
        let mut found = None;
        while let Some(chunk) = self
            .until_stopped(ctx, resp.chunk())
            .await?
            .map_err(fetch_error)?
        {
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
            reporter.report("receiving", || view.json("receiving"));
        }
        let found = match found {
            Some(f) => f,
            None => identify(&head, view.total)?,
        };
        let name = placed_name(hint.as_deref(), found);
        view.file = Some(name.clone());
        spool.finish().await?;
        if view.total.is_some_and(|t| t != view.received) {
            return Err(Error::Fetch(SHORT.to_owned()));
        }
        reporter.report("checking", || view.json("checking"));
        let checked = self.check(ctx, found, &spool).await?;
        self.stop_point(ctx)?;
        reporter.report("placing", || view.json("placing"));
        let placed = match checked {
            Checked::Torrent { bytes, infohash } => {
                drop(spool);
                let file = crate::http::SourceFile {
                    name,
                    bytes,
                    infohash,
                    is_torrent: true,
                };
                crate::http::place_source(app, file)
                    .await
                    .map_err(|e| Error::Fetch(e.message))?
            }
            Checked::Dat => {
                let dir = app.config().paths.dats();
                std::fs::create_dir_all(&dir)?;
                let part = crate::http::dat_part_path(&dir);
                let gate = Arc::clone(&app.gate);
                let pace = move |took: Duration| {
                    if gate.state().core_running() {
                        took.clamp(REST_MIN, REST_MAX)
                    } else {
                        Duration::ZERO
                    }
                };
                spool.place(part.clone(), pace).await?;
                crate::http::place_dat_part(app, &part, &name).await?
            }
        };
        let mut done = view.json("placed");
        done["file"] = json!(placed.file);
        done["target"] = json!(found.target());
        done["placed"] = serde_json::to_value(&placed).unwrap_or(Value::Null);
        ctx.progress(done).await
    }

    /// Checks the spooled file on a blocking thread, stopping when cancelled.
    async fn check(&self, ctx: &JobContext, found: Found, spool: &Spool) -> Result<Checked> {
        let path = spool.path().to_path_buf();
        let cancel = Arc::clone(&self.cancel);
        let shutdown = ctx.app.shutdown_signal();
        let checked = blocking(label::FETCH, move || {
            content::check(found, &path, &|| cancel.is_set() || *shutdown.borrow())
        })
        .await
        .map_err(|e| Error::Task(e.to_string()))?;
        match checked {
            Ok(c) => Ok(c),
            Err(Refused::NotAccepted(why)) => {
                tracing::debug!(why, "a fetched file was refused");
                Err(Error::Fetch(NOT_ACCEPTED.to_owned()))
            }
            Err(Refused::Stopped) => {
                self.stop_point(ctx)?;
                Err(Error::Fetch(CANCELLED.to_owned()))
            }
            Err(Refused::Io(e)) => Err(e.into()),
        }
    }
}

/// Where a spool may go: SQLite's temporary directory when it is in RAM, else the card.
fn places(app: &AppState) -> Places {
    let config = app.config();
    let card = config.paths.tmp();
    let ram = std::env::var_os(crate::db::SQLITE_TMPDIR)
        .map(PathBuf::from)
        .filter(|d| *d != card);
    Places {
        ram,
        card,
        floor: config.memory.import_floor_mib.saturating_mul(1024 * 1024),
    }
}

#[async_trait]
impl Job for UrlFetch {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn payload(&self) -> Value {
        json!({ "fetch": self.token })
    }

    fn lane(&self) -> Lane {
        Lane::Fetch
    }

    async fn run(&self, ctx: &JobContext) -> Result<()> {
        let ran = self.fetch(ctx).await;
        ctx.app.fetches.close(self.token);
        ran
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }

    #[tokio::test]
    async fn a_cancel_wakes_a_waiter() {
        let c = Arc::new(Cancel::default());
        let waiter = Arc::clone(&c);
        let task = tokio::spawn(async move { waiter.wait().await });
        tokio::task::yield_now().await;
        c.cancel();
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("woken")
            .expect("join");
        c.wait().await;
    }

    #[test]
    fn a_view_names_only_what_is_known() {
        let mut v = View {
            token: 3,
            ..View::default()
        };
        assert_eq!(
            v.json("connecting"),
            json!({ "token": 3, "phase": "connecting", "bytes_received": 0 })
        );
        v.total = Some(9);
        v.file = Some("a.dat".into());
        assert_eq!(v.json("receiving")["bytes_total"], 9);
        assert_eq!(v.json("receiving")["file"], "a.dat");
    }
}
