//! Shared server state and the startup sequence of `docs/ARCHITECTURE.md` "Startup".

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::{Duration, Instant};

use mistarr_core::PlatformId;
use mistarr_mister::launch::{CommandSink, FifoSink};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::client::ClientSlot;
use crate::config::{Config, ConfigProblem, RuntimeSettings};
use crate::db::settings::{self, keys};
use crate::db::{self, Db};
use crate::error::{Error, Result};
use crate::events::EventBus;
use crate::jobs::dat_import::Recompute;
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
    /// The `kill` that stops and resumes a client on the board.
    pub kill: PathBuf,
    /// The process table the client is looked for in.
    pub proc_dir: PathBuf,
    /// The frozen client's record, in the private RAM temporary directory.
    pub frozen_file: PathBuf,
    /// How often a held client is checked for having left its hold.
    pub hold_recheck: Duration,
}

impl Default for Options {
    /// [`Options::for_board`] with the RAM directory at [`crate::db::RAM_TEMP_DIR`].
    fn default() -> Self {
        Self::for_board(Path::new(crate::db::RAM_TEMP_DIR))
    }
}

impl Options {
    /// The board's paths and cadences, with the frozen client's record in `ram_dir`.
    ///
    /// ```
    /// use mistarr_server::app::Options;
    /// let o = Options::for_board(std::path::Path::new("/run/m"));
    /// assert!(o.frozen_file.starts_with("/run/m"));
    /// ```
    #[must_use]
    pub fn for_board(ram_dir: &Path) -> Self {
        Self {
            corename_path: PathBuf::from(mistarr_mister::CORENAME_PATH),
            corename_poll: Duration::from_secs(2),
            status_interval: Duration::from_secs(30),
            sources_poll: Duration::from_secs(10),
            sources_min_age_secs: mistarr_sources::intake::DEFAULT_MIN_AGE_SECS,
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
            kill: PathBuf::from("kill"),
            proc_dir: PathBuf::from("/proc"),
            frozen_file: ram_dir.join(crate::freeze::FROZEN_NAME),
            hold_recheck: Duration::from_secs(60),
        }
    }
}

/// Launch state: where commands for MiSTer Main go, and when the last launch was sent.
pub struct LaunchSlot {
    sink: RwLock<Arc<dyn CommandSink>>,
    /// Serialises launches and holds when the last one was sent.
    pub last: tokio::sync::Mutex<Option<Instant>>,
}

impl LaunchSlot {
    /// Commands go to the FIFO at `command_path`.
    ///
    /// ```
    /// let slot = mistarr_server::app::LaunchSlot::new(std::path::Path::new("/nowhere/cmd"));
    /// assert!(!slot.sink().present());
    /// ```
    #[must_use]
    pub fn new(command_path: &Path) -> Self {
        Self {
            sink: RwLock::new(Arc::new(FifoSink::new(command_path))),
            last: tokio::sync::Mutex::new(None),
        }
    }

    /// Where launch commands for MiSTer Main go.
    #[must_use]
    pub fn sink(&self) -> Arc<dyn CommandSink> {
        Arc::clone(&self.sink.read().unwrap_or_else(PoisonError::into_inner))
    }

    /// Replaces the command sink, for tests that record launches.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_sink(&self, sink: Arc<dyn CommandSink>) {
        *self.sink.write().unwrap_or_else(PoisonError::into_inner) = sink;
    }
}

/// What a settings change altered, and the settings now in force.
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::struct_excessive_bools)] // One flag per section that has side effects.
pub struct SettingsChange {
    /// The runtime settings after the change, every section present.
    pub settings: RuntimeSettings,
    /// `[client]` changed, so the client is detected again.
    pub client: bool,
    /// `[limits]` or `[transfer]` changed, so the core gate's hold is re-applied.
    pub limits: bool,
    /// The 1G1R preferences changed, so every platform is recomputed.
    pub selection: bool,
    /// `[scan]` changed, so CHD decoding is queued or dropped.
    pub scan: bool,
    /// What `/system/status` reports changed: `prefs.launch` or `[transfer]`.
    pub status: bool,
}

impl SettingsChange {
    /// The change from `before` to `after`.
    fn between(before: &Config, after: &Config) -> Self {
        let transfer = before.transfer != after.transfer;
        Self {
            settings: after.runtime(),
            client: before.client != after.client,
            limits: transfer || before.limits != after.limits,
            selection: !before.prefs.same_selection(&after.prefs),
            scan: before.scan != after.scan,
            status: transfer || before.prefs.launch != after.prefs.launch,
        }
    }
}

/// Everything request handlers and jobs share. A field is public unless it changes only
/// through a method that keeps an invariant, in which case it is private.
pub struct AppState {
    config: RwLock<Arc<Config>>,
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
    /// Wakes the core gate's client hold after the settings or the client change.
    pub limits_wake: tokio::sync::Notify,
    /// The download client.
    pub client: ClientSlot,
    /// Launching through MiSTer Main.
    pub launch: LaunchSlot,
    /// Passes of the core gate's loop, for tests that bound how often it wakes.
    #[cfg(test)]
    pub gate_passes: std::sync::atomic::AtomicU64,
    /// The daemon's I/O class, when `options.ionice` names a tool to set it.
    pub io_priority: Option<Arc<jobs::io_priority::IoPriority>>,
    shutdown: watch::Sender<bool>,
    settings_write: tokio::sync::Mutex<()>,
}

impl AppState {
    /// Wraps an opened database and a config.
    #[must_use]
    pub fn new(config: Config, db: Db, options: Options) -> Arc<Self> {
        Arc::new(Self {
            config: RwLock::new(Arc::new(config)),
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
            limits_wake: tokio::sync::Notify::new(),
            client: ClientSlot::default(),
            launch: LaunchSlot::new(&options.command_path),
            #[cfg(test)]
            gate_passes: std::sync::atomic::AtomicU64::new(0),
            shutdown: watch::Sender::new(false),
            settings_write: tokio::sync::Mutex::new(()),
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

    /// Points [`ClientSlot::get`] at what `status` found under the current `[client]`,
    /// and wakes the core gate's hold when the handle changed.
    pub fn refresh_client(&self, status: &ClientStatus) {
        if self.client.refresh(status, &self.config().client) {
            self.limits_wake.notify_one();
        }
    }

    /// Installs `client` as the detected client, for tests that script one in process.
    #[cfg(test)]
    pub(crate) fn set_client(&self, client: Arc<dyn mistarr_clients::DownloadClient>) {
        let at = crate::client::ClientEndpoint {
            kind: mistarr_clients::ClientKind::Transmission,
            url: String::new(),
        };
        self.set_client_at(at, client);
    }

    /// [`AppState::set_client`] as the client at `at`.
    #[cfg(test)]
    pub(crate) fn set_client_at(
        &self,
        at: crate::client::ClientEndpoint,
        client: Arc<dyn mistarr_clients::DownloadClient>,
    ) {
        self.client.set(at, client);
        self.limits_wake.notify_one();
    }

    /// Where installed clients are looked for and how they are started.
    #[must_use]
    pub fn launcher(&self) -> mistarr_clients::launch::Launcher {
        mistarr_clients::launch::Launcher {
            transmission_opt_in: self.options.transmission_opt_in.clone(),
            transmission_init: self.options.transmission_init.clone(),
            data_dir: self.config().paths.data.clone(),
            search_path: self.options.client_search_path.clone(),
            timeout: mistarr_clients::launch::START_TIMEOUT,
        }
    }

    /// The effective config; a change made later replaces it rather than altering it.
    #[must_use]
    pub fn config(&self) -> Arc<Config> {
        Arc::clone(&self.config.read().unwrap_or_else(PoisonError::into_inner))
    }

    /// Changes the effective config, copying it first while an earlier [`AppState::config`]
    /// still holds it.
    pub fn update_config(&self, f: impl FnOnce(&mut Config)) {
        let mut slot = self.config.write().unwrap_or_else(PoisonError::into_inner);
        f(Arc::make_mut(&mut slot));
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

    /// Replaces the sections `patch` carries, stores the resulting runtime settings,
    /// makes them effective and runs what each changed section needs: client
    /// detection, the core gate's hold, a recompute, CHD decoding, a `status` event.
    /// Calls are serialised, so concurrent patches to different sections all survive.
    ///
    /// # Errors
    ///
    /// [`Error::Settings`] when a `[client]` the patch carries has a path map entry
    /// that fails the check, [`Error::Db`] or [`Error::Stored`] when the settings
    /// cannot be saved; the effective config is then unchanged. An error queuing the
    /// follow-up work comes after the settings took effect.
    pub async fn update_settings(
        self: &Arc<Self>,
        patch: RuntimeSettings,
    ) -> Result<SettingsChange> {
        let change = {
            let _serial = self.settings_write.lock().await;
            let before = self.config();
            let mut next = Config::clone(&before);
            next.apply(&patch);
            if patch.client.is_some() && next.validate().contains(&ConfigProblem::PathMap) {
                return Err(Error::Settings(ConfigProblem::PathMap));
            }
            let change = SettingsChange::between(&before, &next);
            let stored = change.settings.clone();
            self.db
                .write(move |c| settings::set_json(c, keys::RUNTIME, &stored))
                .await?;
            self.update_config(|c| c.apply(&change.settings));
            change
        };
        if change.scan {
            jobs::chd::apply_setting(self).await?;
        }
        if change.client {
            Scheduler::enqueue(self, Arc::new(DetectClient)).await?;
        }
        if change.limits {
            self.limits_wake.notify_one();
        }
        if change.selection {
            Recompute::enqueue_all(self).await?;
        }
        if change.status {
            crate::status::publish(self).await;
        }
        Ok(change)
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
        // A stopped mistarr never leaves the client frozen.
        jobs::core_limits::thaw_for_shutdown(&self.app).await;
        self.app.scheduler.stop(Duration::from_secs(5)).await;
        let mut server = self.server;
        match tokio::time::timeout(Duration::from_secs(5), &mut server).await {
            Ok(Ok(r)) => Ok(r?),
            Ok(Err(e)) => Err(e.into()),
            Err(_) => {
                server.abort();
                Ok(())
            }
        }
    }
}

/// The opened database and what startup settled in it before anything runs.
pub struct Startup {
    /// The database, migrated, with the platforms seeded.
    pub db: Db,
    /// Platforms whose scan a previous run left unfinished.
    pub unfinished_scans: Vec<PlatformId>,
    /// Platforms whose DAT families resolved to one current version, to recompute.
    pub resolved: Vec<PlatformId>,
}

/// Seeds the platforms, refreshes DAT family keys and leaves one current version per
/// family, returning what that settled.
fn prepare_catalog(db: Db) -> Result<Startup> {
    let (unfinished_scans, resolved) = db.write_blocking(|c| {
        let added = db::platforms::seed(c, &mistarr_mister::platforms::PLATFORMS)?;
        if added > 0 {
            tracing::info!(added, "seeded platforms");
        }
        let unfinished_scans = db::files::platforms_with_progress(c)?;
        let (resolved, settled) = db::transact(c, |tx| {
            db::dats::refresh_families(tx)?;
            let resolved = db::dats::resolve_families(tx)?;
            Ok((resolved, crate::jobs::scan::settle_names(tx)?))
        })?;
        if settled > 0 {
            tracing::info!(settled, "misnamed files verified under the name rule");
        }
        Ok((unfinished_scans, resolved))
    })?;
    Ok(Startup {
        db,
        unfinished_scans,
        resolved,
    })
}

/// The runtime settings saved in `db`, if any.
fn saved_settings(db: &Db) -> Result<Option<RuntimeSettings>> {
    let stored = db.read_blocking(|c| settings::get_json::<serde_json::Value>(c, keys::RUNTIME))?;
    Ok(stored.map(RuntimeSettings::from_saved).transpose()?)
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
    let Startup {
        db,
        unfinished_scans,
        resolved,
    } = open_db(&mut config)?;
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
            unreadable = reconciled.unreadable,
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

    // Step 3: download client, resumed first if a previous run left it frozen at the menu.
    jobs::core_limits::recover_frozen(&app).await;
    Scheduler::run_inline(&app, Arc::new(DetectClient)).await?;

    // Step 4: installed cores.
    detect_cores(&app).await?;

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

/// Removes what a run cut short left behind: a stale migration progress file, the
/// leftovers of an import in RAM, and partial fetches and uploads. Returns whether a
/// database swap was cut short; the database itself is always whole.
fn clean_leftovers(config: &Config) -> Result<bool> {
    let path = config.paths.db();
    crate::migrating::clear_stale(&config.paths.data)?;
    let swapping = db::ram::swap_files(&path);
    db::ram::clean_stale(&path, &config.memory.import_dir)?;
    crate::jobs::url_fetch::spool::clean_stale(&config.paths.tmp());
    if let Some(ram) = crate::jobs::url_fetch::ram_dir(&config.paths.tmp()) {
        crate::jobs::url_fetch::spool::clean_stale(&ram);
    }
    for dir in [config.paths.dats(), config.paths.sources()] {
        crate::jobs::url_fetch::spool::clean_parts(&dir, crate::incoming::place::PART_PREFIX);
    }
    Ok(swapping)
}

/// Cleans up after an interrupted run, migrates and opens the database, prepares the
/// catalog and lays the saved runtime settings over `config`.
///
/// # Errors
///
/// [`Error::Io`] when leftovers cannot be removed or a cut-short swap left no database,
/// [`Error::SchemaTooNew`], [`Error::Migration`] or [`Error::Db`] when it cannot be opened.
pub(crate) fn open_db(config: &mut Config) -> Result<Startup> {
    if let Some(dir) = std::env::var_os(crate::db::SQLITE_TMPDIR) {
        tracing::info!(dir = %Path::new(&dir).display(), "SQLite temporary files");
    }
    // `[memory]` cannot move from the overlay below, so this reaches the log
    // before `migrate_in_ram` reads `import_floor_mib` as its floor.
    config.log_problems();
    let swapping = clean_leftovers(config)?;
    let path = config.paths.db();
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
    let startup = prepare_catalog(db)?;
    match saved_settings(&startup.db) {
        Ok(Some(saved)) => config.apply(&saved),
        Ok(None) => {}
        Err(e) => {
            tracing::warn!(error = %e, "ignoring unreadable saved settings; using the config file");
        }
    }
    // `client` can move from the overlay above, unlike `[memory]`.
    config.log_path_map_problem();
    Ok(startup)
}

/// Runs pending migrations on a copy of the database in RAM when memory allows, so an
/// index rebuild reaches the card as whole MiB writes; `Db::open` migrates what is left.
fn migrate_in_ram(config: &Config, progress: Option<&crate::migrating::Migrating>) -> Result<()> {
    let plan = db::ram::Plan {
        dir: config.memory.import_dir.clone(),
        floor: config.memory.import_floor_mib.saturating_mul(1024 * 1024),
        job: None,
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
async fn queue_startup_jobs(app: &Arc<AppState>, unfinished: Vec<PlatformId>) -> Result<()> {
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
    tasks.push(tokio::spawn(jobs::core_limits::follow_gate(Arc::clone(
        app,
    ))));

    tasks
}

/// Re-enqueues each platform's scan left unfinished by a previous run, skipping
/// arcade, which has no library scan whatever `scan_progress` holds.
async fn resume_scans(app: &Arc<AppState>, unfinished: Vec<PlatformId>) -> Result<()> {
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

/// Marks platforms whose core is installed under the SD root and returns them; the walk
/// runs on the blocking pool and the write goes through [`Db::write_tx`].
pub(crate) async fn detect_cores(app: &AppState) -> Result<Vec<PlatformId>> {
    let root = app.config().paths.root.clone();
    let present: Vec<_> = crate::threads::run(crate::threads::label::DETECT, move || {
        mistarr_mister::corename::installed_cores(&root)
    })
    .await?
    .into_iter()
    .flat_map(|c| c.platforms)
    .collect();
    tracing::info!(platforms = present.len(), "installed cores detected");
    app.db
        .write_tx(move |tx| {
            db::platforms::set_core_present(tx, &present)?;
            Ok(present)
        })
        .await
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
        crate::status::publish(&app).await;
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
            proc_dir: dir.path().join("proc"),
            frozen_file: dir.path().join("run").join(crate::freeze::FROZEN_NAME),
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
        let held = app.config();
        assert!(Arc::ptr_eq(&held, &app.config()), "reads share one config");
        app.update_config(|c| c.limits.up_kbps_core = 3);
        assert_eq!(app.config().limits.up_kbps_core, 3);
        assert_ne!(
            held.limits.up_kbps_core, 3,
            "a config handed out stays as it was"
        );
        assert!(!*app.shutdown_signal().borrow());
        app.begin_shutdown();
        assert!(*app.shutdown_signal().borrow());
    }

    #[tokio::test]
    async fn concurrent_settings_updates_all_persist() {
        let (_dir, app) = testutil::state();
        let limits: RuntimeSettings =
            serde_json::from_str(r#"{"limits":{"up_kbps_core":9}}"#).expect("json");
        let prefs: RuntimeSettings =
            serde_json::from_str(r#"{"prefs":{"languages":["Fr"]}}"#).expect("json");
        let (a, b) = tokio::join!(app.update_settings(limits), app.update_settings(prefs));
        let (a, b) = (a.expect("limits"), b.expect("prefs"));
        assert!(a.limits && !a.client && !a.selection);
        assert!(b.selection && !b.client && !b.limits);
        let stored: RuntimeSettings = app
            .db
            .read(|c| settings::get_json(c, keys::RUNTIME))
            .await
            .expect("read")
            .expect("stored");
        assert_eq!(stored.limits.map(|l| l.up_kbps_core), Some(9));
        assert_eq!(
            stored.prefs.as_ref().expect("prefs").select.languages,
            ["Fr"]
        );
        assert_eq!(app.config().runtime(), stored);
    }

    #[tokio::test]
    async fn a_settings_change_says_what_it_changed_and_refuses_a_bad_path_map() {
        let (_dir, app) = testutil::state();
        let same = app.update_settings(RuntimeSettings::default()).await;
        let same = same.expect("nothing");
        assert!(!(same.client || same.limits || same.selection || same.scan || same.status));
        assert_eq!(same.settings, app.config().runtime());
        let off: RuntimeSettings =
            serde_json::from_str(r#"{"prefs":{"launch":false},"scan":{"chd_tracks":true}}"#)
                .expect("json");
        let change = app.update_settings(off).await.expect("off");
        assert!(change.status && change.scan && !change.selection);
        let bad: RuntimeSettings =
            serde_json::from_str(r#"{"client":{"remote_path_map":[{"remote":"","local":"/l"}]}}"#)
                .expect("json");
        let refused = app.update_settings(bad).await.expect_err("bad map");
        assert!(matches!(refused, Error::Settings(ConfigProblem::PathMap)));
        assert!(app.config().client.remote_path_map.is_empty(), "unchanged");
        let client: RuntimeSettings =
            serde_json::from_str(r#"{"client":{"url":"127.0.0.1:1"}}"#).expect("json");
        assert!(app.update_settings(client).await.expect("client").client);
    }

    #[tokio::test]
    async fn cores_under_root_mark_platforms_present() {
        let (dir, app) = testutil::state();
        let console = dir.path().join("_Console");
        std::fs::create_dir_all(&console).expect("mkdir");
        std::fs::write(console.join("SNES_20240101.rbf"), b"").expect("write");
        detect_cores(&app).await.expect("detect");
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
        detect_cores(&app).await.expect("detect");
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
        assert!(app.client.get().is_none());
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
        let first = app.client.get().expect("client");
        app.refresh_client(&found);
        assert!(Arc::ptr_eq(&first, &app.client.get().expect("client")));
        app.update_config(|c| {
            c.client.remote_path_map = vec![mistarr_clients::PathMapping::new("/r", "/l")];
        });
        app.refresh_client(&found);
        assert!(!Arc::ptr_eq(&first, &app.client.get().expect("client")));
        let second = app.client.get().expect("client");
        app.refresh_client(&ClientStatus {
            kind: None,
            url: None,
            ..found.clone()
        });
        assert!(Arc::ptr_eq(&second, &app.client.get().expect("kept")));
        let other = ClientStatus {
            url: Some("127.0.0.1:2".into()),
            ..found.clone()
        };
        app.refresh_client(&other);
        assert!(Arc::ptr_eq(&second, &app.client.get().expect("kept")));
        app.refresh_client(&ClientStatus {
            reachable: true,
            ..other
        });
        assert!(!Arc::ptr_eq(&second, &app.client.get().expect("replaced")));
    }

    #[test]
    fn default_options_follow_the_board() {
        let o = Options::default();
        assert_eq!(o, Options::for_board(Path::new(crate::db::RAM_TEMP_DIR)));
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
        assert!(!app.launch.sink().present());
        app.launch
            .set_sink(Arc::new(mistarr_mister::launch::RecordingSink::new()));
        assert!(app.launch.sink().present());
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
            .read(|c| db::jobs::count_kind(c, crate::jobs::JobKind::Scan))
            .await
            .expect("count");
        assert_eq!(n, 1, "exactly one scan queued while paused");
        let page = db::sql::Page {
            limit: 10,
            offset: 0,
        };
        let rows = app
            .db
            .read(move |c| db::jobs::list_active(c, page))
            .await
            .expect("list")
            .items;
        let scan = rows
            .iter()
            .find(|r| r.kind == crate::jobs::JobKind::Scan)
            .expect("queued");
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
