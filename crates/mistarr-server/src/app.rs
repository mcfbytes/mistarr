//! Shared server state and the startup sequence of `docs/ARCHITECTURE.md` "Startup".

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::{Duration, Instant};

use mistarr_clients::DownloadClient;
use mistarr_mister::launch::{CommandSink, FifoSink};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::client::ClientKey;
use crate::config::{Config, RuntimeSettings, SettingsPatch};
use crate::db::settings::{self, keys};
use crate::db::{self, Db};
use crate::error::{Error, Result};
use crate::events::{EventBus, EventKind};
use crate::jobs::detect_client::{ClientStatus, DetectClient};
use crate::jobs::gate::Gate;
use crate::jobs::{self, corename, poll, source_import, transfer, Scheduler};

/// Runtime knobs that are not part of `mistarr.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// The file MiSTer writes the running core's name to.
    pub corename_path: PathBuf,
    /// How often it is read.
    pub corename_poll: Duration,
    /// How often each SSE connection receives an unsolicited `status`.
    pub status_interval: Duration,
    /// How often `sources/` is scanned for new files.
    pub sources_poll: Duration,
    /// Seconds a file in `sources/` must be unmodified before it is imported.
    pub sources_min_age_secs: u64,
    /// How often resolving magnets not yet started in the client are retried.
    pub magnet_poll: Duration,
    /// How often a started resolving magnet's file list is checked.
    pub magnet_started_poll: Duration,
    /// How often `dats/` is listed.
    pub dats_poll: Duration,
    /// How old a file's mtime must be before it is imported.
    pub dats_min_age: Duration,
    /// Poll interval while a download is transferring or checking.
    pub poll_active: Duration,
    /// Poll interval otherwise.
    pub poll_idle: Duration,
    /// Poll interval after repeated client failures.
    pub poll_backoff: Duration,
    /// The FIFO MiSTer Main reads commands from.
    pub command_path: PathBuf,
    /// Directory the launch MGL is written to; tmpfs on the board, so the SD card is spared.
    pub launch_dir: PathBuf,
    /// How long after one launch another is refused as `busy`.
    pub launch_gap: Duration,
    /// How often client detection re-runs while no client answers.
    pub redetect_poll: Duration,
    /// Whether the poller asks for re-detection once the client is unreachable.
    pub redetect_on_unreachable: bool,
    /// The directory that opts the `Buildroot_MiSTer` Transmission service in.
    pub transmission_opt_in: PathBuf,
    /// The `Buildroot_MiSTer` Transmission init script.
    pub transmission_init: PathBuf,
    /// Where installed clients are looked for; `None` reads `PATH`.
    pub client_search_path: Option<std::ffi::OsString>,
    /// How long `POST /system/client/start` waits for the client to answer.
    pub client_start_wait: Duration,
    /// The `ionice` that idles the daemon's I/O while a core runs; `None` never changes it.
    pub ionice: Option<PathBuf>,
    /// A PEM bundle an https fetch trusts in place of the system's; `None` finds that.
    pub ca_file: Option<PathBuf>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            corename_path: PathBuf::from(mistarr_mister::CORENAME_PATH),
            corename_poll: Duration::from_secs(2),
            status_interval: Duration::from_secs(30),
            sources_poll: Duration::from_secs(10),
            sources_min_age_secs: mistarr_sources::watch::DEFAULT_MIN_AGE_SECS,
            magnet_poll: Duration::from_secs(15),
            magnet_started_poll: Duration::from_secs(2),
            dats_poll: Duration::from_secs(10),
            dats_min_age: Duration::from_secs(2),
            poll_active: Duration::from_secs(5),
            poll_idle: Duration::from_secs(60),
            poll_backoff: Duration::from_secs(300),
            command_path: PathBuf::from(mistarr_mister::launch::COMMAND_PATH),
            launch_dir: PathBuf::from("/tmp"),
            launch_gap: Duration::from_secs(3),
            redetect_poll: Duration::from_secs(60),
            redetect_on_unreachable: true,
            transmission_opt_in: PathBuf::from(mistarr_clients::launch::TRANSMISSION_OPT_IN),
            transmission_init: PathBuf::from(mistarr_clients::launch::TRANSMISSION_INIT),
            client_search_path: None,
            client_start_wait: Duration::from_secs(10),
            ionice: Some(PathBuf::from("ionice")),
            ca_file: None,
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
    /// Live progress of running jobs, kept in memory only.
    pub live: crate::jobs::progress::LiveProgress,
    /// Uploaded files whose import job is still being recorded.
    pub placed: crate::incoming::Placed,
    /// URL fetches queued or running, by token.
    pub fetches: crate::jobs::url_fetch::Fetches,
    /// When the server started.
    pub started: Instant,
    /// Runtime knobs.
    pub options: Options,
    /// Wakes the download poller to re-check its cadence after a transfer starts.
    pub poll_wake: tokio::sync::Notify,
    /// Wakes client re-detection, as when a client that answered stops answering.
    pub redetect: tokio::sync::Notify,
    /// Serialises client detection so an older probe never overwrites a newer one.
    pub(crate) detect_lock: tokio::sync::Mutex<()>,
    /// Held while `POST /system/client/start` runs, so a second one is `busy`.
    pub(crate) client_start: tokio::sync::Mutex<()>,
    shutdown: watch::Sender<bool>,
    settings_write: tokio::sync::Mutex<()>,
    client: RwLock<Option<(ClientKey, Arc<dyn DownloadClient>)>>,
    commands: RwLock<Arc<dyn CommandSink>>,
    /// Serialises launches and holds when the last one was sent.
    pub(crate) launch_lock: tokio::sync::Mutex<Option<Instant>>,
    /// The daemon's I/O class, when `options.ionice` names a tool to set it.
    pub(crate) io_priority: Option<Arc<jobs::io_priority::IoPriority>>,
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
            live: crate::jobs::progress::LiveProgress::default(),
            placed: crate::incoming::Placed::default(),
            fetches: crate::jobs::url_fetch::Fetches::default(),
            started: Instant::now(),
            poll_wake: tokio::sync::Notify::new(),
            redetect: tokio::sync::Notify::new(),
            detect_lock: tokio::sync::Mutex::new(()),
            client_start: tokio::sync::Mutex::new(()),
            shutdown: watch::Sender::new(false),
            settings_write: tokio::sync::Mutex::new(()),
            client: RwLock::new(None),
            commands: RwLock::new(Arc::new(FifoSink::new(&options.command_path))),
            launch_lock: tokio::sync::Mutex::new(None),
            io_priority: options.ionice.as_deref().map(|program| {
                let setter = Arc::new(jobs::io_priority::Ionice::new(program));
                Arc::new(jobs::io_priority::IoPriority::new(
                    setter,
                    Path::new(jobs::io_priority::TASK_DIR),
                ))
            }),
            options,
        })
    }

    /// Where launch commands for MiSTer Main go: the FIFO at `options.command_path`.
    #[must_use]
    pub fn command_sink(&self) -> Arc<dyn CommandSink> {
        Arc::clone(&self.commands.read().unwrap_or_else(PoisonError::into_inner))
    }

    /// Replaces the command sink, for tests that record launches.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_command_sink(&self, sink: Arc<dyn CommandSink>) {
        *self
            .commands
            .write()
            .unwrap_or_else(PoisonError::into_inner) = sink;
    }

    /// The download client from the last `detect_client` run, or `None` when
    /// detection found none. It is a Transmission or rtorrent handle for the
    /// detected URL, rtorrent's carrying `client.remote_path_map`; it is
    /// replaced only when the detected kind, URL or path map changes. Take a
    /// fresh handle per operation rather than keeping one, and expect calls to
    /// fail with `Unreachable` when the client is down.
    #[must_use]
    pub fn client(&self) -> Option<Arc<dyn DownloadClient>> {
        self.client
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|(_, c)| Arc::clone(c))
    }

    /// Points [`AppState::client`] at what `status` found. The current handle
    /// stays unless the probe answered from a different client, or only the
    /// path map changed; a probe that found nothing never drops it.
    pub fn refresh_client(&self, status: &ClientStatus) {
        let Some(key) = ClientKey::from_detection(status, &self.config().client) else {
            return;
        };
        let mut slot = self.client.write().unwrap_or_else(PoisonError::into_inner);
        let replace = match slot.as_ref() {
            None => true,
            Some((k, _)) if *k == key => false,
            Some((k, _)) => status.reachable || (k.kind == key.kind && k.url == key.url),
        };
        if replace {
            if let Some(client) = key.build() {
                *slot = Some((key, client));
            }
        }
    }

    /// Installs `client` as the detected client, for tests that script one in process.
    #[cfg(test)]
    pub(crate) fn set_client(&self, client: Arc<dyn DownloadClient>) {
        let key = ClientKey {
            kind: mistarr_clients::ClientKind::Transmission,
            url: String::new(),
            path_map: Vec::new(),
        };
        *self.client.write().unwrap_or_else(PoisonError::into_inner) = Some((key, client));
    }

    /// Where installed clients are looked for and how they are started.
    #[must_use]
    pub fn launcher(&self) -> mistarr_clients::launch::Launcher {
        mistarr_clients::launch::Launcher {
            transmission_opt_in: self.options.transmission_opt_in.clone(),
            transmission_init: self.options.transmission_init.clone(),
            data_dir: self.config().paths.data,
            search_path: self.options.client_search_path.clone(),
            timeout: mistarr_clients::launch::START_TIMEOUT,
        }
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
        self.update_config(|c| c.overlay(runtime.clone()));
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
    _lock: crate::lock::InstanceLock,
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

/// What startup reads and settles in the database before anything runs: saved runtime
/// settings, the platforms with an unfinished scan, and those whose DAT families resolved.
type Prepared = (
    Result<Option<RuntimeSettings>>,
    Vec<mistarr_core::PlatformId>,
    Vec<mistarr_core::PlatformId>,
);

/// Seeds the platforms, refreshes DAT family keys and leaves one current version per family.
fn prepare_catalog(c: &mut rusqlite::Connection) -> Result<Prepared> {
    let added = db::platforms::seed(c, &mistarr_mister::platforms::PLATFORMS)?;
    if added > 0 {
        tracing::info!(added, "seeded platforms");
    }
    let unfinished = db::files::platforms_with_progress(c)?;
    let tx = c.transaction()?;
    db::dats::refresh_families(&tx)?;
    let resolved = db::dats::resolve_families(&tx)?;
    crate::db::commit(tx)?;
    Ok((
        settings::get_json::<RuntimeSettings>(c, keys::RUNTIME),
        unfinished,
        resolved,
    ))
}

/// Runs the startup sequence and returns once the HTTP server is listening.
/// The data directory stays locked to this server until it is shut down.
///
/// # Errors
///
/// [`Error::AlreadyRunning`] when another server uses the data directory,
/// [`Error::Io`] when a directory cannot be created or the address cannot be
/// bound, [`Error::Db`] or [`Error::Migration`] when the database cannot be opened.
pub async fn start(mut config: Config, options: Options) -> Result<Running> {
    // Step 1, loading the config, is the caller's.
    for dir in config.paths.layout() {
        std::fs::create_dir_all(&dir)?;
    }
    let lock = crate::lock::InstanceLock::acquire(&config.paths.data)?;

    // Step 2: database, migrations, platform seed, runtime settings.
    let (db, unfinished_scans, resolved) = open_db(&mut config)?;
    let scan_interval = config.jobs.scan_interval_minutes;
    let app = AppState::new(config, db, options);
    // The gate starts closed for a loaded core, so no heavy job slips through before the first poll.
    app.gate
        .set_corename(corename::read(&app.options.corename_path));

    // Jobs a previous process left open go back on their lanes before anything new is queued.
    let reconciled = jobs::reconcile(&app).await?;
    if reconciled != jobs::Reconciled::default() {
        tracing::info!(
            requeued = reconciled.requeued,
            failed = reconciled.failed,
            dropped = reconciled.dropped,
            "reconciled unfinished jobs"
        );
    }

    for platform in &resolved {
        Scheduler::enqueue(
            &app,
            Arc::new(jobs::dat_import::Recompute::new(&platform.0)),
        )
        .await?;
    }

    // Step 3: download client.
    Scheduler::run_inline(&app, Arc::new(DetectClient)).await?;

    // Step 4: installed cores.
    detect_cores(&app)?;

    queue_startup_jobs(&app, unfinished_scans).await?;

    // Step 5: watchers, scheduler and HTTP.
    let tasks = spawn_tasks(&app, scan_interval);
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
        _lock: lock,
    })
}

/// Opens the database, prepares the catalog and applies the saved runtime
/// settings to `config`; returns the database, the platforms whose scan a
/// previous run left unfinished, and the platforms whose DAT families resolved.
pub(crate) fn open_db(
    config: &mut Config,
) -> Result<(
    Db,
    Vec<mistarr_core::PlatformId>,
    Vec<mistarr_core::PlatformId>,
)> {
    if let Some(dir) = std::env::var_os(crate::db::SQLITE_TMPDIR) {
        tracing::info!(dir = %Path::new(&dir).display(), "SQLite temporary files");
    }
    let path = config.paths.db();
    crate::migrating::clear_stale(&config.paths.data)?;
    // Leftovers of an import in RAM cut short; the database itself is always whole.
    let swapping = db::ram::swap_files(&path);
    db::ram::clean_stale(&path, &config.memory.import_dir)?;
    crate::jobs::url_fetch::spool::clean_stale(&config.paths.tmp());
    if config.memory.import_floor_mib == 0 {
        tracing::warn!(
            "[memory] import_floor_mib is 0: a DAT import in RAM may leave the core no memory"
        );
    }
    let progress = match crate::db::migrate::pending(&path)? {
        Some((from, to)) => {
            tracing::info!(from, to, "migrating the database");
            crate::migrating::Migrating::begin(&config.paths.data, &path, from, to)
                .inspect_err(
                    |e| tracing::warn!(error = %e, "cannot write the migration progress file"),
                )
                .ok()
        }
        None => None,
    };
    migrate_in_ram(config, progress.as_ref())?;
    if swapping && !path.exists() {
        return Err(std::io::Error::other(format!(
            "{} could not be recovered from a swap cut short; check the card and restore \
             mistarr.db.prev",
            path.display()
        ))
        .into());
    }
    let db = match &progress {
        Some(m) => Db::open_counting(&path, &m.steps())?,
        None => Db::open(&path)?,
    };
    let (stored, unfinished, resolved) = db.write_blocking(prepare_catalog)?;
    let stored = stored.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "ignoring unreadable saved settings; using the config file");
        None
    });
    if let Some(rt) = stored {
        config.overlay(rt);
    }
    Ok((db, unfinished, resolved))
}

/// Runs pending migrations on a copy of the database in RAM when memory allows, so an
/// index rebuild reaches the card as whole MiB writes; `Db::open` migrates what is left.
fn migrate_in_ram(config: &Config, progress: Option<&crate::migrating::Migrating>) -> Result<()> {
    let plan = db::ram::Plan {
        dir: config.memory.import_dir.clone(),
        floor: config.memory.import_floor_mib.saturating_mul(1024 * 1024),
        job: 0,
        input: 0,
    };
    match db::ram::migrate_in_ram(
        &config.paths.db(),
        &plan,
        progress.map(crate::migrating::Migrating::steps).as_ref(),
    ) {
        Ok(Some(r)) => tracing::info!(
            mib = r.bytes.div_ceil(1024 * 1024),
            card_writes = r.card_writes,
            migrate_ms = r.work.as_millis(),
            write_ms = r.write_back.as_millis(),
            "database migrated in RAM"
        ),
        Ok(None) => {}
        Err(e @ Error::SchemaTooNew { .. }) => return Err(e),
        // A swap that could not put the old file back; opening would create an empty one.
        Err(e) if !config.paths.db().exists() => return Err(e),
        Err(e) => tracing::warn!(error = %e, "migrating the database in place"),
    }
    Ok(())
}

/// Queues the jobs every start runs: unfinished scans (never arcade's), CHD decoding when
/// its setting is on, the arcade catalogue, and a re-map of the bound sources whose roms changed.
async fn queue_startup_jobs(
    app: &Arc<AppState>,
    unfinished: Vec<mistarr_core::PlatformId>,
) -> Result<()> {
    resume_scans(app, unfinished).await?;
    jobs::chd::apply_setting(app).await?;
    jobs::arcade::enqueue_if_relevant(app).await?;
    let remap = jobs::remap::RemapSources { platforms: None };
    Scheduler::enqueue(app, Arc::new(remap)).await?;
    Ok(())
}

/// Starts the scheduler and the watchers that run until shutdown.
fn spawn_tasks(app: &Arc<AppState>, scan_interval: u32) -> Vec<tokio::task::JoinHandle<()>> {
    let mut tasks = Vec::new();
    let opts = app.options.clone();
    let gate = Arc::clone(&app.gate);
    tasks.push(tokio::spawn(async move {
        corename::watch(&opts.corename_path, opts.corename_poll, gate).await;
    }));
    tasks.push(tokio::spawn(publish_gate_changes(Arc::clone(app))));
    if let Some(priority) = &app.io_priority {
        tasks.push(tokio::spawn(jobs::io_priority::follow(
            Arc::clone(&app.gate),
            Arc::clone(priority),
            jobs::io_priority::RETRY,
        )));
    }
    if scan_interval > 0 {
        tasks.push(tokio::spawn(scan_on_timer(
            Arc::clone(app),
            Duration::from_secs(u64::from(scan_interval) * 60),
        )));
    }
    Scheduler::start(app);
    tasks.push(tokio::spawn(source_import::watch(Arc::clone(app))));
    tasks.push(tokio::spawn(source_import::resolve_pending(Arc::clone(
        app,
    ))));
    tasks.push(tokio::spawn(crate::jobs::dat_import::watch(Arc::clone(
        app,
    ))));
    tasks.push(tokio::spawn(crate::jobs::import::watch(Arc::clone(app))));
    tasks.push(tokio::spawn(transfer::watch(Arc::clone(app))));
    tasks.push(tokio::spawn(poll::run(Arc::clone(app))));
    tasks.push(tokio::spawn(crate::jobs::detect_client::watch(Arc::clone(
        app,
    ))));
    tasks.push(tokio::spawn(poll::follow_gate(Arc::clone(app))));

    tasks
}

/// Re-enqueues each platform's scan left unfinished by a previous run, skipping
/// arcade, which has no library scan whatever `scan_progress` holds.
async fn resume_scans(
    app: &Arc<AppState>,
    unfinished: Vec<mistarr_core::PlatformId>,
) -> Result<()> {
    for platform_id in unfinished
        .into_iter()
        .filter(|id| !jobs::scan::is_arcade(id))
    {
        tracing::info!(platform = %platform_id.0, "resuming interrupted scan");
        Scheduler::enqueue(
            app,
            Arc::new(jobs::scan::ScanJob {
                platform_id: Some(platform_id),
            }),
        )
        .await?;
    }
    Ok(())
}

/// Marks platforms whose core is installed under the SD root and returns them.
pub(crate) fn detect_cores(app: &AppState) -> Result<Vec<mistarr_core::PlatformId>> {
    let root = app.config().paths.root;
    let cores = mistarr_mister::corename::installed_cores(&root);
    let present: Vec<_> = cores.into_iter().flat_map(|c| c.platforms).collect();
    tracing::info!(platforms = present.len(), "installed cores detected");
    app.db
        .write_blocking(|c| db::platforms::set_core_present(c, &present))?;
    Ok(present)
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

    /// A test's directory, and the RAM directory its imports copy the database into.
    pub struct TestDir {
        root: tempfile::TempDir,
        ram: tempfile::TempDir,
    }

    impl TestDir {
        /// The directory holding the paths of the test's config.
        pub fn path(&self) -> &Path {
            self.root.path()
        }

        /// `[memory] import_dir`.
        pub fn ram(&self) -> &Path {
            self.ram.path()
        }
    }

    /// App state over a fresh database with paths inside the returned directory.
    pub fn state() -> (TestDir, Arc<AppState>) {
        state_with(|_| {})
    }

    /// [`state`] with its options adjusted by `f`.
    pub fn state_with(f: impl FnOnce(&mut Options)) -> (TestDir, Arc<AppState>) {
        let dir = TestDir {
            root: tempfile::tempdir().expect("tempdir"),
            ram: db::testutil::ram_dir(),
        };
        let mut config = Config::default();
        config.paths.root = dir.path().to_path_buf();
        config.paths.games = dir.path().join("games");
        config.paths.data = dir.path().join("data");
        config.memory.import_dir = dir.ram().to_path_buf();
        std::fs::create_dir_all(&config.paths.data).expect("mkdir");
        let db = Db::open(&config.paths.db()).expect("db");
        db.write_blocking(|c| {
            db::platforms::seed(c, &mistarr_mister::platforms::PLATFORMS).map(|_| ())
        })
        .expect("seed");
        let mut options = Options {
            corename_path: dir.path().join("CORENAME"),
            command_path: dir.path().join("MiSTer_cmd"),
            launch_dir: dir.path().to_path_buf(),
            launch_gap: Duration::ZERO,
            ionice: None,
            ..Options::default()
        };
        f(&mut options);
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

    #[tokio::test]
    async fn a_claimed_arcade_core_marks_its_row_present() {
        let (dir, app) = testutil::state();
        let cores = dir.path().join("_Arcade/cores");
        std::fs::create_dir_all(&cores).expect("mkdir");
        std::fs::write(cores.join("jtngp_20240101.rbf"), b"").expect("write");
        detect_cores(&app).expect("detect");
        let rows = app.db.read(db::platforms::list).await.expect("list");
        let mut present: Vec<_> = rows
            .iter()
            .filter(|r| r.core_present)
            .map(|r| r.id.0.as_str())
            .collect();
        present.sort_unstable();
        assert_eq!(present, ["arcade", "ngp"]);
    }

    #[tokio::test]
    async fn client_handle_follows_detection() {
        let (_dir, app) = testutil::state();
        assert!(app.client().is_none());
        let found = ClientStatus {
            kind: Some(mistarr_clients::ClientKind::Rtorrent),
            url: Some("127.0.0.1:1".into()),
            reachable: false,
            version: None,
            rtorrent_on_path: false,
            checked_at: 0,
            ..ClientStatus::default()
        };
        app.refresh_client(&found);
        let first = app.client().expect("client");
        app.refresh_client(&found);
        assert!(Arc::ptr_eq(&first, &app.client().expect("client")));
        app.update_config(|c| {
            c.client.remote_path_map = vec![mistarr_clients::PathMapping::new("/r", "/l")];
        });
        app.refresh_client(&found);
        assert!(!Arc::ptr_eq(&first, &app.client().expect("client")));
        let second = app.client().expect("client");
        app.refresh_client(&ClientStatus {
            kind: None,
            url: None,
            ..found.clone()
        });
        assert!(Arc::ptr_eq(&second, &app.client().expect("kept")));
        let other = ClientStatus {
            url: Some("127.0.0.1:2".into()),
            ..found.clone()
        };
        app.refresh_client(&other);
        assert!(Arc::ptr_eq(&second, &app.client().expect("kept")));
        app.refresh_client(&ClientStatus {
            reachable: true,
            ..other
        });
        assert!(!Arc::ptr_eq(&second, &app.client().expect("replaced")));
    }

    #[test]
    fn default_options_follow_the_board() {
        let o = Options::default();
        assert_eq!(o.corename_path, PathBuf::from("/tmp/CORENAME"));
        assert_eq!(o.corename_poll, Duration::from_secs(2));
        assert_eq!(o.command_path, PathBuf::from("/dev/MiSTer_cmd"));
        assert_eq!(o.launch_dir, PathBuf::from("/tmp"));
        assert_eq!(o.launch_gap, Duration::from_secs(3));
        assert_eq!(o.ionice, Some(PathBuf::from("ionice")));
    }

    #[test]
    fn command_sink_is_replaceable() {
        let (_dir, app) = testutil::state();
        assert!(!app.command_sink().present());
        app.set_command_sink(Arc::new(mistarr_mister::launch::RecordingSink::new()));
        assert!(app.command_sink().present());
    }

    /// The timer queues its scan on the heavy lane, so it sits behind the
    /// gate rather than running while a core is loaded.
    #[tokio::test]
    async fn scan_timer_waits_for_the_gate() {
        let (_dir, app) = testutil::state();
        app.gate.set_corename(Some("SNES".into()));
        Scheduler::start(&app);
        // One tick, then stop the loop so exactly one scan is ever queued.
        let timer = tokio::spawn(scan_on_timer(Arc::clone(&app), Duration::from_millis(20)));
        tokio::time::sleep(Duration::from_millis(80)).await;
        timer.abort();
        let _ = timer.await;

        let n = app
            .db
            .read(|c| db::jobs::count_kind(c, "scan"))
            .await
            .expect("count");
        assert_eq!(n, 1, "exactly one scan queued while paused");
        let (rows, _) = app
            .db
            .read(|c| db::jobs::list_active(c, 10, 0))
            .await
            .expect("list");
        let scan = rows.iter().find(|r| r.kind == "scan").expect("queued");
        let id = scan.id;
        assert_eq!(
            scan.state,
            db::jobs::JobState::Queued,
            "must not run while the core gate is closed"
        );

        app.gate
            .set_corename(Some(crate::jobs::gate::MENU.to_owned()));
        for _ in 0..200 {
            let row = app
                .db
                .read(move |c| db::jobs::get(c, id))
                .await
                .expect("read")
                .expect("row");
            if row.state == db::jobs::JobState::Done {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("scan never ran once the gate opened");
    }
}
