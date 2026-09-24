//! Shared server state and the startup sequence of `docs/ARCHITECTURE.md` "Startup".

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, PoisonError, RwLock};
use std::time::{Duration, Instant};

use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::config::{Config, RuntimeSettings, SettingsPatch};
use crate::db::settings::{self, keys};
use crate::db::{self, Db};
use crate::error::{Error, Result};
use crate::events::{EventBus, EventKind};
use crate::jobs::detect_client::DetectClient;
use crate::jobs::gate::Gate;
use crate::jobs::{self, corename, Scheduler};

/// Runtime knobs that are not part of `mistarr.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// The file MiSTer writes the running core's name to.
    pub corename_path: PathBuf,
    /// How often it is read.
    pub corename_poll: Duration,
    /// How often each SSE connection receives an unsolicited `status`.
    pub status_interval: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            corename_path: PathBuf::from(mistarr_mister::CORENAME_PATH),
            corename_poll: Duration::from_secs(2),
            status_interval: Duration::from_secs(30),
        }
    }
}

/// Everything request handlers and jobs share.
pub struct AppState {
    config: RwLock<Config>,
    /// The database.
    pub db: Db,
    /// The SSE bus.
    pub events: EventBus,
    /// The scheduler gate.
    pub gate: Arc<Gate>,
    /// The job scheduler.
    pub scheduler: Scheduler,
    /// When the server started.
    pub started: Instant,
    /// Runtime knobs.
    pub options: Options,
    shutdown: watch::Sender<bool>,
    settings_write: tokio::sync::Mutex<()>,
}

impl AppState {
    /// Wraps an opened database and a config.
    #[must_use]
    pub fn new(config: Config, db: Db, options: Options) -> Arc<Self> {
        Arc::new(Self {
            config: RwLock::new(config),
            db,
            events: EventBus::new(),
            gate: Arc::new(Gate::new()),
            scheduler: Scheduler::new(),
            started: Instant::now(),
            options,
            shutdown: watch::Sender::new(false),
            settings_write: tokio::sync::Mutex::new(()),
        })
    }

    /// A copy of the effective config.
    #[must_use]
    pub fn config(&self) -> Config {
        self.config
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Changes the effective config in place.
    pub fn update_config(&self, f: impl FnOnce(&mut Config)) {
        f(&mut self.config.write().unwrap_or_else(PoisonError::into_inner));
    }

    /// A receiver that turns true when the server is shutting down.
    #[must_use]
    pub fn shutdown_signal(&self) -> watch::Receiver<bool> {
        self.shutdown.subscribe()
    }

    /// Tells SSE streams, job lanes and checkpoints to stop.
    pub fn begin_shutdown(&self) {
        self.shutdown.send_replace(true);
    }

    /// Applies a settings patch, stores the resulting runtime settings and
    /// makes them effective. Calls are serialised, so concurrent patches to
    /// different sections all survive. Returns the new settings and whether
    /// `client` changed.
    ///
    /// # Errors
    ///
    /// [`Error::Db`] or [`Error::Stored`] when the settings cannot be saved;
    /// the effective config is then unchanged.
    pub async fn update_settings(&self, patch: &SettingsPatch) -> Result<(RuntimeSettings, bool)> {
        let _serial = self.settings_write.lock().await;
        let mut next = self.config();
        let before = next.client.clone();
        next.apply(patch);
        let runtime = next.runtime();
        let stored = runtime.clone();
        self.db
            .write(move |c| settings::set_json(c, keys::RUNTIME, &stored))
            .await?;
        self.update_config(|c| {
            c.client.clone_from(&runtime.client);
            c.limits = runtime.limits;
            c.prefs.clone_from(&runtime.prefs);
        });
        let client_changed = runtime.client != before;
        Ok((runtime, client_changed))
    }
}

/// A started server.
pub struct Running {
    /// The bound HTTP address.
    pub addr: SocketAddr,
    /// Shared state, for tests and the binary.
    pub app: Arc<AppState>,
    server: JoinHandle<std::io::Result<()>>,
    tasks: Vec<JoinHandle<()>>,
}

impl Running {
    /// Stops accepting requests, ends SSE streams, cancels jobs at their next
    /// checkpoint and waits up to five seconds each for the job lanes and
    /// in-flight requests.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the server loop failed.
    pub async fn shutdown(self) -> Result<()> {
        self.app.begin_shutdown();
        for t in &self.tasks {
            t.abort();
        }
        self.app.scheduler.stop(Duration::from_secs(5)).await;
        let mut server = self.server;
        match tokio::time::timeout(Duration::from_secs(5), &mut server).await {
            Ok(Ok(r)) => Ok(r?),
            Ok(Err(e)) => Err(Error::Task(e.to_string())),
            Err(_) => {
                server.abort();
                Ok(())
            }
        }
    }
}

/// Runs the startup sequence and returns once the HTTP server is listening.
///
/// # Errors
///
/// [`Error::Io`] when a directory cannot be created or the address cannot be
/// bound, [`Error::Db`] or [`Error::Migration`] when the database cannot be opened.
pub async fn start(mut config: Config, options: Options) -> Result<Running> {
    // Step 1, loading the config, is the caller's.
    for dir in config.paths.layout() {
        std::fs::create_dir_all(&dir)?;
    }

    // Step 2: database, migrations, platform seed, runtime settings.
    let db = Db::open(&config.paths.db())?;
    let (stored, unfinished_scans) = db.write_blocking(|c| {
        let added = db::platforms::seed(c, &mistarr_mister::platforms::PLATFORMS)?;
        if added > 0 {
            tracing::info!(added, "seeded platforms");
        }
        let interrupted = db::jobs::fail_interrupted(c, crate::unix_now())?;
        if !interrupted.is_empty() {
            tracing::warn!(count = interrupted.len(), "marked interrupted jobs failed");
        }
        let unfinished = db::files::platforms_with_progress(c)?;
        Ok((
            settings::get_json::<RuntimeSettings>(c, keys::RUNTIME),
            unfinished,
        ))
    })?;
    let stored = stored.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "ignoring unreadable saved settings; using the config file");
        None
    });
    if let Some(rt) = stored {
        config.client = rt.client;
        config.limits = rt.limits;
        config.prefs = rt.prefs;
    }
    let scan_interval = config.jobs.scan_interval_minutes;
    let app = AppState::new(config, db, options);

    // Step 3: download client.
    Scheduler::run_inline(&app, Arc::new(DetectClient)).await?;

    // Step 4: installed cores.
    detect_cores(&app)?;

    // Resume any scan left unfinished by a previous run.
    for platform_id in unfinished_scans {
        tracing::info!(platform = %platform_id.0, "resuming interrupted scan");
        Scheduler::enqueue(
            &app,
            Arc::new(jobs::scan::ScanJob {
                platform_id: Some(platform_id),
            }),
        )
        .await?;
    }

    // Step 5: watchers, scheduler and HTTP.
    let mut tasks = Vec::new();
    let opts = app.options.clone();
    let gate = Arc::clone(&app.gate);
    tasks.push(tokio::spawn(async move {
        corename::watch(&opts.corename_path, opts.corename_poll, gate).await;
    }));
    tasks.push(tokio::spawn(publish_gate_changes(Arc::clone(&app))));
    if scan_interval > 0 {
        tasks.push(tokio::spawn(scan_on_timer(
            Arc::clone(&app),
            Duration::from_secs(u64::from(scan_interval) * 60),
        )));
    }
    Scheduler::start(&app);

    let listener = tokio::net::TcpListener::bind(app.config().server.listen.as_str()).await?;
    let addr = listener.local_addr()?;
    let router = crate::http::router(Arc::clone(&app));
    let mut stop = app.shutdown_signal();
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = stop.wait_for(|s| *s).await;
            })
            .await
    });
    tracing::info!(%addr, "listening");

    // Step 6, opening on the wizard, is decided by the UI from `/system/wizard`.
    Ok(Running {
        addr,
        app,
        server,
        tasks,
    })
}

/// Marks platforms whose core is installed under the SD root.
fn detect_cores(app: &AppState) -> Result<()> {
    let root = app.config().paths.root;
    let cores = mistarr_mister::corename::installed_cores(&root);
    let present: Vec<_> = cores.into_iter().flat_map(|c| c.platforms).collect();
    tracing::info!(platforms = present.len(), "installed cores detected");
    app.db
        .write_blocking(|c| db::platforms::set_core_present(c, &present))
}

/// Enqueues a full library scan every `interval`, from `[jobs] scan_interval_minutes`.
async fn scan_on_timer(app: Arc<AppState>, interval: Duration) {
    let mut stop = app.shutdown_signal();
    let mut ticker = tokio::time::interval(interval);
    ticker.tick().await;
    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            _ = stop.wait_for(|s| *s) => return,
        }
        if let Err(e) =
            Scheduler::enqueue(&app, Arc::new(jobs::scan::ScanJob { platform_id: None })).await
        {
            tracing::warn!(error = %e, "cannot enqueue scheduled scan");
        }
    }
}

/// Publishes `status` whenever the gate changes.
async fn publish_gate_changes(app: Arc<AppState>) {
    let mut rx = app.gate.subscribe();
    while rx.changed().await.is_ok() {
        let status = crate::status::snapshot(&app).await;
        app.events.publish(EventKind::Status, &status);
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;

    /// App state over a fresh database with paths inside the returned directory.
    pub fn state() -> (tempfile::TempDir, Arc<AppState>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = Config::default();
        config.paths.root = dir.path().to_path_buf();
        config.paths.games = dir.path().join("games");
        config.paths.data = dir.path().join("data");
        std::fs::create_dir_all(&config.paths.data).expect("mkdir");
        let db = Db::open(&config.paths.db()).expect("db");
        db.write_blocking(|c| {
            db::platforms::seed(c, &mistarr_mister::platforms::PLATFORMS).map(|_| ())
        })
        .expect("seed");
        let options = Options {
            corename_path: dir.path().join("CORENAME"),
            ..Options::default()
        };
        (dir, AppState::new(config, db, options))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn config_updates_are_visible() {
        let (_dir, app) = testutil::state();
        app.update_config(|c| c.limits.up_kbps_core = 3);
        assert_eq!(app.config().limits.up_kbps_core, 3);
        assert!(!*app.shutdown_signal().borrow());
        app.begin_shutdown();
        assert!(*app.shutdown_signal().borrow());
    }

    #[tokio::test]
    async fn concurrent_settings_updates_all_persist() {
        let (_dir, app) = testutil::state();
        let limits: SettingsPatch =
            serde_json::from_str(r#"{"limits":{"up_kbps_core":9}}"#).expect("json");
        let prefs: SettingsPatch =
            serde_json::from_str(r#"{"prefs":{"languages":["Fr"]}}"#).expect("json");
        let (a, b) = tokio::join!(app.update_settings(&limits), app.update_settings(&prefs));
        assert!(!a.expect("limits").1 && !b.expect("prefs").1);
        let stored: RuntimeSettings = app
            .db
            .read(|c| settings::get_json(c, keys::RUNTIME))
            .await
            .expect("read")
            .expect("stored");
        assert_eq!(stored.limits.up_kbps_core, 9);
        assert_eq!(stored.prefs.languages, ["Fr"]);
        assert_eq!(app.config().runtime(), stored);
    }

    #[tokio::test]
    async fn cores_under_root_mark_platforms_present() {
        let (dir, app) = testutil::state();
        let console = dir.path().join("_Console");
        std::fs::create_dir_all(&console).expect("mkdir");
        std::fs::write(console.join("SNES_20240101.rbf"), b"").expect("write");
        detect_cores(&app).expect("detect");
        let rows = app.db.read(db::platforms::list).await.expect("list");
        let present: Vec<_> = rows
            .iter()
            .filter(|r| r.core_present)
            .map(|r| r.id.0.as_str())
            .collect();
        assert_eq!(present, ["snes"]);
    }

    #[test]
    fn default_options_follow_the_board() {
        let o = Options::default();
        assert_eq!(o.corename_path, PathBuf::from("/tmp/CORENAME"));
        assert_eq!(o.corename_poll, Duration::from_secs(2));
    }
}
