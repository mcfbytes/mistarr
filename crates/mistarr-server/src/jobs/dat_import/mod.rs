//! The DAT import and 1G1R recompute jobs and the `dats/` watcher; the flow is
//! `docs/ARCHITECTURE.md` "DAT import".

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use mistarr_core::select::Prefs;
use mistarr_core::PlatformId;
use mistarr_sources::intake::{self, StableFiles, LOADED_DIR};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use self::recompute::{pass_progress, recompute_blocking, recompute_pass, Pass, Tally};
use self::stream::import_from;
use super::fsutil::extension;
use super::progress::{Progress, Reporter};
use super::stop::StopToken;
use super::watch::wizard;
use super::{follow_up, Job, JobContext, JobKind, Lane, Scheduler};
use crate::app::AppState;
use crate::db::dats;
use crate::db::ids::DatVersionId;
use crate::db::ids::JobId;
use crate::db::ram::{self, Ram};
use crate::db::Db;
use crate::error::{Error, Result};
use crate::events::{DatLoaded, DatRejected, Event};

/// Largest DAT or DAT pack an upload or a URL fetch accepts; daily packs of every
/// system fit well inside.
pub const MAX_DAT_BYTES: u64 = 512 * 1024 * 1024;

/// How long a pause lasts while a core runs, for the parse and the copy into RAM.
const YIELD_FOR: Duration = Duration::from_millis(20);

/// Longest pause between two chunks of the copy into RAM or back while a core runs.
const YIELD_AT_MOST: Duration = Duration::from_secs(1);

/// Binding an unbound version: re-read it from `dats/loaded/` for one platform.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Bind {
    version: DatVersionId,
    platform: PlatformId,
    dat_name: String,
    dat_version: String,
}

/// Loads one file from `dats/`: a `.dat` or `.xml` DAT, or a `.zip` whose
/// `.dat` and `.xml` members are separate DATs. The file ends in `dats/loaded/`
/// when any member loaded, else in `dats/rejected/` with a reason file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatImport {
    path: PathBuf,
    bind: Option<Bind>,
}

/// What a [`DatImport`] stores as its payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Payload {
    #[serde(serialize_with = "super::path_text")]
    path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dat_version_id: Option<DatVersionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    platform_id: Option<PlatformId>,
}

impl DatImport {
    /// The job a stored payload describes, `None` when it does not read as one. A
    /// bind request's payload names no DAT, so such a job is never run; see [`Self::binds`].
    ///
    /// ```
    /// use mistarr_server::jobs::{dat_import::DatImport, Job};
    /// let job = DatImport::from_payload(&serde_json::json!({ "path": "/d/a.dat" })).unwrap();
    /// assert_eq!(job.detail().as_deref(), Some("a.dat"));
    /// assert!(!job.binds());
    /// ```
    #[must_use]
    pub fn from_payload(payload: &Value) -> Option<Self> {
        let p: Payload = serde_json::from_value(payload.clone()).ok()?;
        let bind = match (p.dat_version_id, p.platform_id) {
            (Some(version), Some(platform)) => Some(Bind {
                version,
                platform,
                dat_name: String::new(),
                dat_version: String::new(),
            }),
            _ => None,
        };
        Some(Self { path: p.path, bind })
    }

    /// Whether the job binds an unbound version rather than loading a dropped file.
    #[must_use]
    pub fn binds(&self) -> bool {
        self.bind.is_some()
    }

    /// A job for a file in `dats/`.
    ///
    /// ```
    /// use mistarr_server::jobs::{dat_import::DatImport, Job};
    /// let job = DatImport::new("/data/dats/a.dat".as_ref());
    /// assert_eq!(job.payload()["path"], "/data/dats/a.dat");
    /// ```
    #[must_use]
    pub fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            bind: None,
        }
    }

    /// A job that loads the titles of `row`, an unbound version, for `platform`.
    /// `loaded` is the `dats/loaded/` directory holding its source file.
    ///
    /// ```
    /// use mistarr_server::db::dats::DatVersionRow;
    /// use mistarr_server::db::ids::DatVersionId;
    /// use mistarr_server::jobs::{dat_import::DatImport, Job};
    /// let row = DatVersionRow { id: DatVersionId::new(3), platform_id: None, dat_name: "Test Console".into(),
    ///     version: "1".into(), source_file: "t.dat".into(), loaded_at: 0, superseded_by: None,
    ///     game_count: 1, retired: false, family: "test console".into(), reason: None,
    ///     suggested: Vec::new() };
    /// let nes = mistarr_core::PlatformId::new("nes");
    /// let job = DatImport::bind(&row, &nes, "/d/loaded".as_ref());
    /// assert_eq!(job.payload()["dat_version_id"], 3);
    /// ```
    #[must_use]
    pub fn bind(row: &dats::DatVersionRow, platform: &PlatformId, loaded: &Path) -> Self {
        Self {
            path: loaded.join(&row.source_file),
            bind: Some(Bind {
                version: row.id,
                platform: platform.clone(),
                dat_name: row.dat_name.clone(),
                dat_version: row.version.clone(),
            }),
        }
    }
}

/// One DAT inside the file.
#[derive(Debug, Clone, Copy)]
enum Member {
    Plain,
    Zip(usize),
}

/// The result of reading one member.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    Loaded(Loaded),
    Rejected(String),
    Skipped,
}

/// A member that was stored.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Loaded {
    version: DatVersionId,
    platform: Option<PlatformId>,
    games: u64,
    has_titles: bool,
    retired: usize,
}

/// What one member import needs besides the connection.
struct Request {
    source_file: String,
    file_stem: String,
    bind: Option<Bind>,
    prefs: Prefs,
    now: i64,
    stop: StopToken,
    meter: Option<Meter>,
    /// Fail with [`Error::Paused`] on a manual pause instead of waiting it out, for an
    /// import that holds the writer meanwhile.
    abort_on_hold: bool,
    /// For an import in RAM, the `MemAvailable` bytes below which it gives up its copy.
    floor: Option<u64>,
}

/// What the members of a file did, and whether their platforms' recompute already ran.
struct Imported {
    outcomes: Vec<Outcome>,
    recomputed: bool,
}

/// Bytes an index pass must read before it is shown; a Logiqx DAT leaves that
/// pass after its header, so only a DB export's real index pass shows.
const INDEX_SHOWN_AFTER: u64 = 64 * 1024;

/// Live progress of a file's import: `{ file, members, done, games, phase,
/// bytes, bytes_total }`, and `reason` when it runs on the card, sent through the
/// job's [`Reporter`] without the database.
struct Meter {
    reporter: Reporter,
    file: String,
    members: usize,
    done: AtomicUsize,
    games_before: AtomicU64,
    games: AtomicU64,
    reason: Option<String>,
}

impl Meter {
    fn new(reporter: Reporter, file: &str, members: usize, reason: Option<&str>) -> Self {
        Self {
            reporter,
            file: file.to_owned(),
            members,
            done: AtomicUsize::new(0),
            games_before: AtomicU64::new(0),
            games: AtomicU64::new(0),
            reason: reason.map(str::to_owned),
        }
    }

    /// Starts the member after `done` others, which loaded `games_before` games.
    fn at_member(&self, done: usize, games_before: u64) {
        self.done.store(done, Ordering::Relaxed);
        self.games_before.store(games_before, Ordering::Relaxed);
        self.games.store(0, Ordering::Relaxed);
    }

    /// Reports `read` of `total` bytes in a reading `phase`; the index pass reports no share.
    fn bytes(&self, phase: &str, read: u64, total: u64) {
        if phase == "indexing" {
            // Its own share would restart the bar at reading; it shows as a band instead.
            if read >= INDEX_SHOWN_AFTER {
                self.phase(phase);
            }
            return;
        }
        self.reporter
            .report(phase, || self.value(phase, Some((read, total))));
    }

    /// Reports the start of a `phase` whose share done is unknown.
    fn phase(&self, phase: &str) {
        self.reporter.report(phase, || self.value(phase, None));
    }

    fn value(&self, phase: &str, bytes: Option<(u64, u64)>) -> Progress {
        let games = self.games_before.load(Ordering::Relaxed) + self.games.load(Ordering::Relaxed);
        let mut p = Progress::phase(phase)
            .with("file", &self.file)
            .with("members", self.members)
            .with("games", games);
        p.done = u64::try_from(self.done.load(Ordering::Relaxed)).ok();
        if let Some((read, total)) = bytes {
            p = p.bytes(read.min(total), Some(total));
        }
        if let Some(r) = &self.reason {
            p = p.with("reason", r);
        }
        p
    }
}

/// Reports a phase change when a meter is attached.
fn phase(req: &Request, phase: &str) {
    if let Some(m) = &req.meter {
        m.phase(phase);
    }
}

#[async_trait]
impl Job for DatImport {
    fn kind(&self) -> JobKind {
        JobKind::DatImport
    }

    fn payload(&self) -> Value {
        super::to_payload(&Payload {
            path: self.path.clone(),
            dat_version_id: self.bind.as_ref().map(|b| b.version),
            platform_id: self.bind.as_ref().map(|b| b.platform.clone()),
        })
    }

    fn detail(&self) -> Option<String> {
        super::file_detail(&self.path)
    }

    fn lane(&self) -> Lane {
        Lane::Background
    }

    async fn run(&self, ctx: &JobContext) -> Result<()> {
        let file = file_name(&self.path);
        if self.bind.is_some() {
            return self.run_bind(ctx, &file).await;
        }
        let path = self.path.clone();
        let listed = crate::threads::run(crate::threads::label::DAT_IMPORT, move || {
            path.is_file().then(|| match list_members(&path) {
                Ok(members) => intake::plan(&path, LOADED_DIR).map(|c| Ok((members, c))),
                Err(reason) => Ok(Err(reason)),
            })
        })
        .await?;
        let (members, target) = match listed {
            None => return Ok(()),
            Some(Ok(Ok(found))) => found,
            Some(Ok(Err(reason))) => return reject(&ctx.app, &self.path, &file, &reason).await,
            Some(Err(e)) => return Err(e.into()),
        };
        let stored = file_name(&target);
        let Imported {
            outcomes,
            recomputed,
        } = self.import_members(ctx, &members, &stored).await?;
        let mut loaded = Vec::new();
        let mut reasons = Vec::new();
        for o in outcomes {
            match o {
                Outcome::Loaded(l) => loaded.push(l),
                Outcome::Rejected(r) => reasons.push(r),
                Outcome::Skipped => {}
            }
        }
        if loaded.is_empty() {
            return reject(&ctx.app, &self.path, &file, &reasons.join("\n")).await;
        }
        let (path, planned) = (self.path.clone(), target);
        crate::threads::run(crate::threads::label::DAT_IMPORT, move || {
            intake::place(&path, &planned)
        })
        .await??;
        for reason in &reasons {
            publish_rejected(&ctx.app, &file, reason);
        }
        for l in &loaded {
            tracing::info!(
                file,
                version = %l.version,
                games = l.games,
                titles = l.has_titles,
                retired = l.retired,
                "DAT loaded"
            );
            publish_loaded(&ctx.app, l, &file);
        }
        follow_up_load(&ctx.app, &loaded, recomputed).await;
        Ok(())
    }
}

impl DatImport {
    /// Loads the titles of an unbound version from its file in `dats/loaded/`.
    /// Anything short of that publishes `dat.rejected` and fails the job.
    async fn run_bind(&self, ctx: &JobContext, file: &str) -> Result<()> {
        let fail = |reason: String| {
            publish_rejected(&ctx.app, file, &reason);
            Err(Error::Job(reason))
        };
        let path = self.path.clone();
        let listed = crate::threads::run(crate::threads::label::DAT_IMPORT, move || {
            path.is_file().then(|| list_members(&path))
        })
        .await?;
        let members = match listed {
            None => return fail(format!("{file} is no longer in dats/{LOADED_DIR}/")),
            Some(Ok(m)) => m,
            Some(Err(reason)) => return fail(reason),
        };
        let mut reasons = Vec::new();
        let imported = self.import_members(ctx, &members, file).await?;
        for outcome in imported.outcomes {
            match outcome {
                Outcome::Loaded(l) => {
                    publish_loaded(&ctx.app, &l, file);
                    let loaded = std::slice::from_ref(&l);
                    follow_up_load(&ctx.app, loaded, imported.recomputed).await;
                    return Ok(());
                }
                Outcome::Rejected(r) => reasons.push(r),
                Outcome::Skipped => {}
            }
        }
        if reasons.is_empty() {
            reasons.push(format!("{file} no longer holds that DAT"));
        }
        fail(reasons.join("\n"))
    }

    /// Imports every member on a copy of the database in RAM, with their platforms'
    /// recompute, and swaps the copy in; in place, member by member, when memory or room
    /// is short. A manual pause drops the copy, waits, and starts again.
    async fn import_members(
        &self,
        ctx: &JobContext,
        members: &[Member],
        source_file: &str,
    ) -> Result<Imported> {
        let reason = loop {
            match self.import_in_ram(ctx, members, source_file).await {
                Ok(Ram::Done(outcomes, report)) => {
                    log_report(source_file, &report);
                    return Ok(Imported {
                        outcomes,
                        recomputed: true,
                    });
                }
                Ok(Ram::Fallback(why)) => {
                    let reason = why.summary;
                    tracing::info!(
                        file = source_file,
                        reason,
                        detail = why.detail,
                        "DAT imported in place"
                    );
                    ctx.progress(json!({
                        "file": source_file,
                        "members": members.len(),
                        "phase": IN_PLACE,
                        "reason": reason,
                    }))
                    .await?;
                    break reason;
                }
                Err(Error::Paused) => ctx.checkpoint().await?,
                Err(e) => return Err(e),
            }
        };
        Ok(Imported {
            outcomes: self
                .import_in_place(ctx, members, source_file, reason)
                .await?,
            recomputed: false,
        })
    }

    /// The request for one member of this file; `floor` is set for an import in RAM.
    fn request(
        &self,
        ctx: &JobContext,
        source_file: &str,
        floor: Option<u64>,
        meter: Option<Meter>,
    ) -> Request {
        Request {
            source_file: source_file.to_owned(),
            file_stem: stem(&self.path),
            bind: self.bind.clone(),
            prefs: ctx.app.config().prefs.select.clone(),
            now: crate::unix_now(),
            stop: ctx.stop.clone(),
            meter,
            abort_on_hold: floor.is_some(),
            floor,
        }
    }

    /// Holds the writer and runs every member, then the recompute of each platform they
    /// loaded into, on a copy in RAM through [`ram::run`].
    async fn import_in_ram(
        &self,
        ctx: &JobContext,
        members: &[Member],
        source_file: &str,
    ) -> Result<Ram<Vec<Outcome>>> {
        let config = ctx.app.config();
        let floor = config.memory.import_floor_mib.saturating_mul(1024 * 1024);
        let (path, listed) = (self.path.clone(), members.to_vec());
        let input = crate::threads::run(crate::threads::label::DAT_IMPORT, move || {
            members_size(&path, &listed)
        })
        .await?;
        let plan = ram::Plan {
            dir: config.memory.import_dir.clone(),
            floor,
            job: Some(ctx.id),
            input,
        };
        let meter = Meter::new(ctx.reporter(), source_file, members.len(), None);
        let req = self.request(ctx, source_file, Some(floor), Some(meter));
        let mut watch = RamWatch {
            reporter: ctx.reporter(),
            id: ctx.id,
            file: source_file.to_owned(),
            members: members.len(),
            stop: ctx.stop.clone(),
            chunk_started: Instant::now(),
        };
        let path = self.path.clone();
        let members = members.to_vec();
        ctx.app
            .db
            .hold_writer(crate::threads::label::DAT_IMPORT, move |held| {
                let (id, file, count) = (watch.id, watch.file.clone(), watch.members);
                let mut store = |db: &Db, done, games| store(db, id, &file, count, done, games);
                ram::run(held, &plan, &mut watch, |db| {
                    import_all(db, &path, &members, &req, &mut store)
                })
            })
            .await
    }

    /// Imports every member in place on a blocking thread, checkpointing between them.
    /// `reason` says why it runs in place, and goes into every progress it stores.
    async fn import_in_place(
        &self,
        ctx: &JobContext,
        members: &[Member],
        source_file: &str,
        reason: &str,
    ) -> Result<Vec<Outcome>> {
        let mut outcomes = Vec::with_capacity(members.len());
        let mut games = 0;
        for (done, &member) in members.iter().enumerate() {
            ctx.checkpoint().await?;
            let meter = Meter::new(ctx.reporter(), source_file, members.len(), Some(reason));
            meter.at_member(done, games);
            let req = self.request(ctx, source_file, None, Some(meter));
            let path = self.path.clone();
            let db = ctx.app.db.clone();
            let outcome = crate::threads::run(crate::threads::label::DAT_IMPORT, move || {
                import_from(&db, &path, member, &req)
            })
            .await??;
            if let Outcome::Loaded(l) = &outcome {
                games += l.games;
            }
            outcomes.push(outcome);
            ctx.progress(json!({
                "file": source_file,
                "members": members.len(),
                "done": done + 1,
                "games": games,
                "phase": IN_PLACE,
                "reason": reason,
            }))
            .await?;
        }
        Ok(outcomes)
    }
}

/// `phase` of a DAT import that runs on the card.
const IN_PLACE: &str = "importing in place";

/// Imports `members` of `path` into `db`, the copy in RAM, then recomputes each platform
/// a member loaded into, as the queued recompute would; asks for the copy to be written
/// back when any member loaded.
fn import_all(
    db: &Db,
    path: &Path,
    members: &[Member],
    req: &Request,
    progress: &mut dyn FnMut(&Db, usize, u64) -> Result<()>,
) -> Result<(Vec<Outcome>, bool)> {
    let mut outcomes = Vec::with_capacity(members.len());
    let mut games = 0;
    for (done, &member) in members.iter().enumerate() {
        req.stop.check()?;
        // The first member follows the check that allowed the copy.
        if done > 0 {
            room(req)?;
        }
        if let Some(m) = &req.meter {
            m.at_member(done, games);
        }
        let outcome = import_from(db, path, member, req)?;
        if let Outcome::Loaded(l) = &outcome {
            games += l.games;
        }
        outcomes.push(outcome);
        progress(db, done + 1, games)?;
    }
    let mut platforms: Vec<&PlatformId> = Vec::new();
    for o in &outcomes {
        if let Outcome::Loaded(Loaded {
            platform: Some(p), ..
        }) = o
        {
            if !platforms.contains(&p) {
                platforms.push(p);
            }
        }
    }
    let report = |pass: Pass, tally: &Tally| {
        if let Some(m) = &req.meter {
            m.reporter.report(pass.label(), || {
                m.value(pass.label(), None)
                    .with("checked", tally.checked)
                    .with("matched", tally.matched)
            });
        }
    };
    for p in platforms {
        recompute_blocking(
            db,
            p,
            &req.prefs,
            &|| req.stop.check().and_then(|()| room(req)),
            &report,
        )?;
    }
    let loaded = outcomes.iter().any(|o| matches!(o, Outcome::Loaded(_)));
    Ok((outcomes, loaded))
}

/// [`Error::NoRoom`] when memory fell below the floor of a request in RAM.
fn room(req: &Request) -> Result<()> {
    req.floor.map_or(Ok(()), ram::memory_left)
}

/// Uncompressed bytes of `members` of `path`, 0 for those that cannot be read.
fn members_size(path: &Path, members: &[Member]) -> u64 {
    let Ok(file) = File::open(path) else {
        return 0;
    };
    if members.iter().all(|m| matches!(m, Member::Plain)) {
        return file.metadata().map_or(0, |m| m.len());
    }
    let Ok(mut archive) = zip::ZipArchive::new(BufReader::new(file)) else {
        return 0;
    };
    members
        .iter()
        .filter_map(|m| match m {
            Member::Zip(i) => archive.by_index_raw(*i).ok().map(|e| e.size()),
            Member::Plain => None,
        })
        .sum()
}

/// Reports an import in RAM's copy and write-back as live progress, stops it between
/// their steps when the job must stop or, while copying in, on a pause, and while a
/// core runs rests as long as the last chunk took.
struct RamWatch {
    reporter: Reporter,
    id: JobId,
    file: String,
    members: usize,
    stop: StopToken,
    chunk_started: Instant,
}

impl ram::Watch for RamWatch {
    fn phase(&mut self, phase: ram::Phase) {
        self.chunk_started = Instant::now();
        // The load reports its own phases through the meter.
        if phase == ram::Phase::Importing {
            return;
        }
        self.reporter.report(phase.label(), || {
            Progress::phase(phase.label())
                .with("file", &self.file)
                .with("members", self.members)
        });
    }

    fn between(&mut self, phase: ram::Phase) -> Result<()> {
        self.stop.stopped()?;
        if self.stop.core_running() {
            // At most half the time busy while a core runs.
            let took = self.chunk_started.elapsed();
            std::thread::sleep(took.clamp(YIELD_FOR, YIELD_AT_MOST));
        }
        self.chunk_started = Instant::now();
        // The write-back lasts seconds and a pause lets it finish.
        if phase != ram::Phase::Writing {
            self.stop.check()?;
        }
        Ok(())
    }
}

/// Stores the members `done` on the copy's row of job `id`, so the swap keeps the last.
fn store(db: &Db, id: JobId, file: &str, members: usize, done: usize, games: u64) -> Result<()> {
    let body = json!({
        "file": file,
        "members": members,
        "done": done,
        "games": games,
        "phase": ram::Phase::Importing.label(),
    });
    let now = crate::unix_now();
    db.write_blocking(|c| crate::db::jobs::set_progress(c, id, &body, now))
}

fn log_report(file: &str, r: &ram::Report) {
    tracing::info!(
        file,
        mib = r.bytes.div_ceil(1024 * 1024),
        card_writes = r.card_writes,
        copy_ms = r.copy_in.as_millis(),
        import_ms = r.work.as_millis(),
        write_ms = r.write_back.as_millis(),
        "DAT imported in RAM"
    );
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The DATs a file holds, or why it is not a DAT file.
fn list_members(path: &Path) -> std::result::Result<Vec<Member>, String> {
    match extension(path).unwrap_or_default().as_str() {
        "dat" | "xml" => Ok(vec![Member::Plain]),
        "zip" => {
            let file = File::open(path).map_err(|e| e.to_string())?;
            let archive = zip::ZipArchive::new(BufReader::new(file))
                .map_err(|e| format!("invalid zip archive: {e}"))?;
            let members: Vec<_> = (0..archive.len())
                .filter(|&i| archive.name_for_index(i).is_some_and(is_dat_name))
                .map(Member::Zip)
                .collect();
            if members.is_empty() {
                return Err("zip archive contains no .dat or .xml files".to_owned());
            }
            Ok(members)
        }
        _ => Err("not a DAT: expected a .dat, .xml or .zip file".to_owned()),
    }
}

fn is_dat_name(name: &str) -> bool {
    let ext = extension(Path::new(name));
    !name.ends_with('/') && matches!(ext.as_deref(), Some("dat" | "xml"))
}

/// Queues what follows the members that loaded titles, once per platform however
/// many DATs of a pack loaded into it, then checks whether the wizard just became complete.
async fn follow_up_load(app: &Arc<AppState>, loaded: &[Loaded], recomputed: bool) {
    let mut platforms: Vec<PlatformId> = Vec::new();
    for p in loaded.iter().filter_map(|l| l.platform.as_ref()) {
        if !platforms.contains(p) {
            platforms.push(p.clone());
        }
    }
    follow_up::catalogue_changed(app, &platforms, recomputed).await;
    if let Err(e) = wizard::on_change(app).await {
        tracing::warn!(error = %e, "cannot check wizard completion");
    }
}

fn publish_loaded(app: &AppState, l: &Loaded, file: &str) {
    app.events.publish(&Event::DatLoaded(DatLoaded {
        dat_version_id: l.version,
        file,
        platform_id: l.platform.as_ref(),
    }));
}

fn publish_rejected(app: &AppState, file: &str, reason: &str) {
    app.events
        .publish(&Event::DatRejected(DatRejected { file, reason }));
}

/// Moves a file into `rejected/` with `<name>.reason.txt` beside it, on a blocking
/// thread, and publishes `dat.rejected`.
async fn reject(app: &AppState, path: &Path, file: &str, reason: &str) -> Result<()> {
    let (path, text) = (path.to_path_buf(), reason.to_owned());
    crate::threads::run(crate::threads::label::DAT_IMPORT, move || {
        intake::reject(&path, &text)
    })
    .await??;
    tracing::warn!(file, reason, "DAT rejected");
    publish_rejected(app, file, reason);
    Ok(())
}

/// Matches files of retired roms again, then, outside arcade, the platform's unmatched
/// files; recomputes the 1G1R picks of one platform under the current preferences, then
/// queues what [`follow_up::recomputed`] says. Its payload is the struct itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recompute {
    #[serde(rename = "platform_id")]
    platform: PlatformId,
}

impl Recompute {
    /// A job for `platform`.
    ///
    /// ```
    /// use mistarr_server::jobs::{dat_import::Recompute, Job};
    /// let nes = mistarr_core::PlatformId::new("nes");
    /// assert_eq!(Recompute::new(&nes).payload()["platform_id"], "nes");
    /// ```
    #[must_use]
    pub fn new(platform: &PlatformId) -> Self {
        Self {
            platform: platform.clone(),
        }
    }

    /// Enqueues one job per platform.
    ///
    /// # Errors
    ///
    /// [`Error::Db`] when the platforms cannot be listed or a job cannot be recorded.
    pub async fn enqueue_all(app: &Arc<AppState>) -> Result<()> {
        let platforms = app.db.read(crate::db::platforms::list).await?;
        for p in platforms {
            Scheduler::enqueue(app, Arc::new(Self::new(&p.id))).await?;
        }
        Ok(())
    }
}

#[async_trait]
impl Job for Recompute {
    fn kind(&self) -> JobKind {
        JobKind::Recompute
    }

    fn payload(&self) -> Value {
        super::to_payload(self)
    }

    fn detail(&self) -> Option<String> {
        Some(self.platform.as_str().to_owned())
    }

    fn lane(&self) -> Lane {
        Lane::Background
    }

    async fn run(&self, ctx: &JobContext) -> Result<()> {
        let reporter = ctx.reporter();
        let prefs = Arc::new(ctx.app.config().prefs.select.clone());
        let mut tally = Tally::default();
        let mut pass = Pass::Retired;
        while pass != Pass::Done {
            reporter.report(pass.label(), || pass_progress(pass, &tally));
            ctx.checkpoint().await?;
            let (platform, prefs) = (self.platform.clone(), Arc::clone(&prefs));
            (pass, tally) = ctx
                .app
                .db
                .write_tx(move |tx| {
                    let mut tally = tally;
                    let next = recompute_pass(tx, &platform, &prefs, pass, &mut tally)?;
                    Ok((next, tally))
                })
                .await?;
        }
        let Tally {
            matched, picked, ..
        } = tally;
        ctx.progress(json!({ "groups": picked.groups, "picks": picked.picks, "matched": matched }))
            .await?;
        follow_up::recomputed(&ctx.app, &self.platform).await;
        Ok(())
    }
}

/// Polls `dats/` every `options.dats_poll` and enqueues a [`DatImport`] per stable file;
/// see [`super::drop_watch::run`].
pub async fn watch(app: Arc<AppState>) {
    let dir = app.config().paths.dats();
    let files = StableFiles::new(app.options.dats_min_age, |name| !name.starts_with('.'));
    let poll = app.options.dats_poll;
    super::drop_watch::run(
        app,
        dir,
        files,
        poll,
        crate::threads::label::DAT_WATCH,
        |path| Arc::new(DatImport::new(path)),
    )
    .await;
}

mod recompute;
mod stream;
#[cfg(test)]
mod sync_writes;
#[cfg(test)]
mod tests;
