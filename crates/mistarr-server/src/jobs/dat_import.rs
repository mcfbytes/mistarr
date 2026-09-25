//! The DAT import and 1G1R recompute jobs and the `dats/` watcher; the flow is
//! `docs/ARCHITECTURE.md` "DAT import".

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use mistarr_core::dat::{
    export_name, export_parents, split_version, DatGame, DatHeader, DatStream, ExportOptions,
};
use mistarr_core::hash::HeaderRule;
use mistarr_core::naming::{group_key, parse_name};
use mistarr_core::select::{HiddenFlag, Prefs};
use mistarr_core::PlatformId;
use rusqlite::Connection;
use serde_json::{json, Value};
use tokio::sync::watch;

use super::gate::GateState;
use super::{scan, wizard, Job, JobContext, Lane, Scheduler};
use crate::app::AppState;
use crate::config::PrefsConfig;
use crate::db::dat_stage::{self, StagedGame, StagedRom};
use crate::db::dats::{self, DatVersionId, NewVersion};
use crate::db::files::{self, FileId, FileRow, FileState};
use crate::db::jobs::{JobId, JobState};
use crate::db::titles;
use crate::db::Db;
use crate::error::{Error, Result};
use crate::events::EventKind;

/// `jobs.kind` of [`DatImport`].
pub const KIND: &str = "dat_import";

/// `jobs.kind` of [`Recompute`].
pub const RECOMPUTE_KIND: &str = "recompute_1g1r";

/// Subdirectory of `dats/` for files that loaded.
pub const LOADED_DIR: &str = "loaded";

pub use crate::incoming::{REASON_SUFFIX, REJECTED_DIR};

/// Games read between checks for shutdown.
const CANCEL_EVERY: u64 = 500;

/// Games parsed between pauses while a core runs, so the parse never holds a CPU for long.
const YIELD_EVERY: u64 = 200;

/// How often a parse held by a manual pause looks again.
const PAUSED_POLL: Duration = Duration::from_millis(250);

/// Games parsed per write to `dat_stage`; the write lock is free between them.
const STAGE_CHUNK: usize = 2000;

/// How long each of those pauses lasts.
const YIELD_FOR: std::time::Duration = std::time::Duration::from_millis(20);

/// The 1G1R preferences of `[prefs]`; hide names that are not selection flags are ignored.
///
/// ```
/// let p = mistarr_server::jobs::dat_import::prefs(&Default::default());
/// assert_eq!(p, mistarr_core::select::Prefs::default());
/// ```
#[must_use]
pub fn prefs(cfg: &PrefsConfig) -> Prefs {
    let hide = cfg
        .hide
        .iter()
        .filter_map(|h| match h.as_str() {
            "bios" => Some(HiddenFlag::Bios),
            "beta" => Some(HiddenFlag::Beta),
            "proto" => Some(HiddenFlag::Proto),
            "demo" => Some(HiddenFlag::Demo),
            "sample" => Some(HiddenFlag::Sample),
            "program" => Some(HiddenFlag::Program),
            _ => None,
        })
        .collect();
    Prefs {
        regions: cfg.regions.clone(),
        languages: cfg.languages.clone(),
        prefer_latest_revision: cfg.prefer_latest_revision,
        hide,
    }
}

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

impl DatImport {
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
    /// use mistarr_server::db::dats::{DatVersionId, DatVersionRow};
    /// use mistarr_server::jobs::{dat_import::DatImport, Job};
    /// let row = DatVersionRow { id: DatVersionId(3), platform_id: None, dat_name: "Test Console".into(),
    ///     version: "1".into(), source_file: "t.dat".into(), loaded_at: 0, superseded_by: None,
    ///     game_count: 1, retired: false, family: "test console".into(), reason: None,
    ///     suggested: Vec::new() };
    /// let job = DatImport::bind(&row, "nes", "/d/loaded".as_ref());
    /// assert_eq!(job.payload()["dat_version_id"], 3);
    /// ```
    #[must_use]
    pub fn bind(row: &dats::DatVersionRow, platform: &str, loaded: &Path) -> Self {
        Self {
            path: loaded.join(&row.source_file),
            bind: Some(Bind {
                version: row.id,
                platform: PlatformId(platform.to_owned()),
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
    stop: watch::Receiver<bool>,
    gate: watch::Receiver<GateState>,
}

#[async_trait]
impl Job for DatImport {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn payload(&self) -> Value {
        match &self.bind {
            None => json!({ "path": self.path }),
            Some(b) => json!({
                "path": self.path,
                "dat_version_id": b.version,
                "platform_id": b.platform.0,
            }),
        }
    }

    fn lane(&self) -> Lane {
        Lane::Background
    }

    async fn run(&self, ctx: &JobContext) -> Result<()> {
        let file = file_name(&self.path);
        if self.bind.is_some() {
            return self.run_bind(ctx, &file).await;
        }
        if !self.path.is_file() {
            return Ok(());
        }
        let members = match list_members(&self.path) {
            Ok(m) => m,
            Err(reason) => return reject(&ctx.app, &self.path, &file, &reason),
        };
        let dats_dir = self.path.parent().unwrap_or_else(|| Path::new("."));
        let target = unique_path(&dats_dir.join(LOADED_DIR), &file);
        let stored = file_name(&target);
        let outcomes = self.import_members(ctx, &members, &stored).await?;
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
            return reject(&ctx.app, &self.path, &file, &reasons.join("\n"));
        }
        std::fs::create_dir_all(dats_dir.join(LOADED_DIR))?;
        std::fs::rename(&self.path, &target)?;
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
        enqueue_follow_up_work(&ctx.app, &loaded).await;
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
        if !self.path.is_file() {
            return fail(format!("{file} is no longer in dats/{LOADED_DIR}/"));
        }
        let members = match list_members(&self.path) {
            Ok(m) => m,
            Err(reason) => return fail(reason),
        };
        let mut reasons = Vec::new();
        for outcome in self.import_members(ctx, &members, file).await? {
            match outcome {
                Outcome::Loaded(l) => {
                    publish_loaded(&ctx.app, &l, file);
                    enqueue_follow_up_work(&ctx.app, std::slice::from_ref(&l)).await;
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

    /// Imports every member on a blocking thread, checkpointing between them.
    async fn import_members(
        &self,
        ctx: &JobContext,
        members: &[Member],
        source_file: &str,
    ) -> Result<Vec<Outcome>> {
        let mut outcomes = Vec::with_capacity(members.len());
        let mut games = 0;
        for (done, &member) in members.iter().enumerate() {
            ctx.checkpoint().await?;
            let req = Request {
                source_file: source_file.to_owned(),
                file_stem: stem(&self.path),
                bind: self.bind.clone(),
                prefs: prefs(&ctx.app.config().prefs),
                now: crate::unix_now(),
                stop: ctx.app.shutdown_signal(),
                gate: ctx.app.gate.subscribe(),
            };
            let path = self.path.clone();
            let db = ctx.app.db.clone();
            let outcome =
                tokio::task::spawn_blocking(move || import_from(&db, &path, member, &req))
                    .await
                    .map_err(|e| Error::Task(e.to_string()))??;
            if let Outcome::Loaded(l) = &outcome {
                games += l.games;
            }
            outcomes.push(outcome);
            ctx.progress(json!({
                "file": source_file,
                "members": members.len(),
                "done": done + 1,
                "games": games,
            }))
            .await?;
        }
        Ok(outcomes)
    }
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

fn extension(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

/// The DATs a file holds, or why it is not a DAT file.
fn list_members(path: &Path) -> std::result::Result<Vec<Member>, String> {
    match extension(path).as_str() {
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
    !name.ends_with('/') && matches!(extension(Path::new(name)).as_str(), "dat" | "xml")
}

/// Opens a member for streaming and imports it, reading a DB export twice so its
/// clones are linked. Runs on a blocking thread.
fn import_from(db: &Db, path: &Path, member: Member, req: &Request) -> Result<Outcome> {
    let parents = match with_member(path, member, |r, name| {
        Ok(export_parents(r).map_err(|e| rejected(name, &e)))
    })? {
        Ok(Ok(p)) => p,
        Ok(Err(reason)) | Err(reason) => return Ok(Outcome::Rejected(reason)),
    };
    match with_member(path, member, |r, name| {
        import_stream(db, r, req, name, parents)
    })? {
        Ok(outcome) => Ok(outcome),
        Err(reason) => Ok(Outcome::Rejected(reason)),
    }
}

/// Runs `f` on a fresh buffered reader of `member` and its name in the zip, empty for a
/// plain file; `Err` holds why a zip member cannot be opened.
fn with_member<T>(
    path: &Path,
    member: Member,
    f: impl FnOnce(&mut dyn BufRead, &str) -> Result<T>,
) -> Result<std::result::Result<T, String>> {
    let file = File::open(path)?;
    match member {
        Member::Plain => f(&mut BufReader::new(file), "").map(Ok),
        Member::Zip(index) => {
            let mut archive = match zip::ZipArchive::new(BufReader::new(file)) {
                Ok(a) => a,
                Err(e) => return Ok(Err(format!("invalid zip archive: {e}"))),
            };
            let entry = match archive.by_index(index) {
                Ok(e) => e,
                Err(e) => return Ok(Err(format!("invalid zip archive: {e}"))),
            };
            let name = entry.name().to_owned();
            let out = f(&mut BufReader::new(entry), &name);
            out.map(Ok)
        }
    }
}

/// A rejection reason, naming the zip member it came from.
fn rejected(member: &str, e: &dyn std::fmt::Display) -> String {
    if member.is_empty() {
        e.to_string()
    } else {
        format!("{member}: {e}")
    }
}

/// Streams one Logiqx DAT, or a DB export whose clones stay unlinked; see [`import_stream`].
#[cfg(test)]
fn import_member<R: BufRead>(db: &Db, reader: R, req: &Request, member: &str) -> Result<Outcome> {
    import_stream(db, reader, req, member, None)
}

/// The DAT name and version a member is stored under, and the platform it loads into.
struct Identity {
    name: String,
    version: String,
    platform: Option<String>,
}

/// A DB export's identity: the name from the member's name, else the dropped file's,
/// else the member's or file's stem less a final version group, which becomes the
/// version; a plain file being bound keeps its stored name.
fn export_identity(req: &Request, member: &str) -> Identity {
    let named = [member, req.file_stem.as_str()]
        .into_iter()
        .filter(|n| !n.is_empty())
        .find_map(export_name);
    let (name, version) = match (&req.bind, named) {
        (Some(b), _) if member.is_empty() => (b.dat_name.clone(), b.dat_version.clone()),
        (_, Some(n)) => (n.dat_name, n.version),
        _ => {
            let stem = if member.is_empty() {
                req.file_stem.clone()
            } else {
                stem(Path::new(member))
            };
            let (name, version) = split_version(&stem);
            (name.to_owned(), version.to_owned())
        }
    };
    let platform = platform_for(req, &name);
    Identity {
        name,
        version,
        platform,
    }
}

/// A Logiqx DAT's identity: its header name, else the member's stem, else the file's.
fn logiqx_identity(req: &Request, member: &str, header: &DatHeader) -> Identity {
    let name = match (&req.bind, member) {
        _ if !header.name.trim().is_empty() => header.name.clone(),
        (_, m) if !m.is_empty() => stem(Path::new(m)),
        // A plain file holds one DAT; its stored name survives the rename into loaded/.
        (Some(b), _) => b.dat_name.clone(),
        (None, _) => req.file_stem.clone(),
    };
    let platform = platform_for(req, &name);
    Identity {
        name,
        version: header.version.clone(),
        platform,
    }
}

/// How a DB export's files become roms on `platform`: its header rule, the extensions it
/// loads, and the one it writes, else the first it loads.
fn export_options(platform: Option<&str>, parents: HashMap<String, String>) -> ExportOptions {
    let row = platform.and_then(mistarr_mister::platforms::by_id);
    ExportOptions {
        header_rule: row.map_or(HeaderRule::None, |p| HeaderRule::from_name(p.header_rule)),
        extension: row
            .and_then(|p| {
                p.extension_written
                    .or_else(|| p.load_extensions.first().copied())
            })
            .map(str::to_owned),
        load_extensions: row
            .map(|p| p.load_extensions.iter().map(|e| (*e).to_owned()).collect())
            .unwrap_or_default(),
        parents,
    }
}

/// The platform a DAT named `dat_name` loads into: the one being bound, else the table's.
fn platform_for(req: &Request, dat_name: &str) -> Option<String> {
    match &req.bind {
        Some(b) => Some(b.platform.0.clone()),
        None => mistarr_mister::bind_dat_name(dat_name).map(|p| p.id.to_owned()),
    }
}

/// Opens the game stream of one DAT with the identity it is stored under; a DB export
/// is identified first, since its platform decides which files become roms.
fn open_stream<R: BufRead>(
    reader: R,
    req: &Request,
    member: &str,
    parents: Option<HashMap<String, String>>,
) -> std::result::Result<(DatStream<R>, Identity), mistarr_core::dat::DatError> {
    let Some(parents) = parents else {
        let stream = DatStream::new(reader)?;
        let id = logiqx_identity(req, member, stream.header());
        return Ok((stream, id));
    };
    let mut id = export_identity(req, member);
    let options = export_options(id.platform.as_deref(), parents);
    let stream = DatStream::with_options(reader, options)?;
    if !stream.header().version.trim().is_empty() {
        id.version.clone_from(&stream.header().version);
    }
    Ok((stream, id))
}

/// Streams one DAT into `dat_stage` in chunks of [`STAGE_CHUNK`] games, each
/// its own short write, then applies it in one transaction so readers see the
/// old titles or the new ones, never a mix. A parse error becomes
/// [`Outcome::Rejected`] and leaves the catalog untouched. `parents` is a DB
/// export's archive index, `None` for a Logiqx DAT.
fn import_stream<R: BufRead>(
    db: &Db,
    reader: R,
    req: &Request,
    member: &str,
    parents: Option<HashMap<String, String>>,
) -> Result<Outcome> {
    let prefix = if member.is_empty() {
        String::new()
    } else {
        format!("{member}: ")
    };
    let (stream, id) = match open_stream(reader, req, member, parents) {
        Ok(opened) => opened,
        Err(e) => return Ok(Outcome::Rejected(format!("{prefix}{e}"))),
    };
    let Identity {
        name: dat_name,
        version,
        platform: bound,
    } = id;
    if let Some(b) = &req.bind {
        if b.dat_name != dat_name || b.dat_version != version {
            return Ok(Outcome::Skipped);
        }
    }
    let new = NewVersion {
        dat_name: &dat_name,
        version: &version,
        source_file: &req.source_file,
        platform: bound.as_deref(),
        now: req.now,
    };
    let newer = || {
        Outcome::Rejected(format!(
            "{prefix}a newer version of {dat_name} is loaded; bind that version instead"
        ))
    };
    let (planned, current) = db.read_blocking(|c| dats::plan_version(c, &new))?;
    if req.bind.is_some() && !current {
        return Ok(newer());
    }
    let staging = planned.is_some() && current;
    if staging {
        db.write_blocking(|c| dat_stage::clear(c))?;
    }
    let mut games = 0u64;
    let mut clone_of = false;
    let mut chunk = Vec::new();
    for game in stream {
        let game = match game {
            Ok(g) => g,
            Err(e) => {
                db.write_blocking(|c| dat_stage::clear(c))?;
                return Ok(Outcome::Rejected(format!("{prefix}{e}")));
            }
        };
        games += 1;
        pace(req, games)?;
        if staging {
            clone_of |= game.clone_of.is_some();
            chunk.push(staged(&game));
            if chunk.len() >= STAGE_CHUNK {
                db.write_blocking(|c| append_chunk(c, &chunk))?;
                chunk.clear();
            }
        }
    }
    if !chunk.is_empty() {
        db.write_blocking(|c| append_chunk(c, &chunk))?;
    }
    db.write_blocking(|c| {
        let tx = c.transaction()?;
        let plan = dats::upsert_version(&tx, &new)?;
        if req.bind.is_some() && !plan.current {
            return Ok(newer());
        }
        let platform = plan.platform_id.clone().filter(|_| plan.current && staging);
        if let Some(p) = &platform {
            dats::begin_load(&tx, plan.id)?;
            dat_stage::apply(&tx, &p.0, plan.id)?;
        }
        dats::set_game_count(&tx, plan.id, games)?;
        let mut retired = 0;
        if let Some(p) = &platform {
            titles::link_parents(&tx, plan.id, clone_of)?;
            retired = dats::retire_absent(&tx, plan.id)?;
            titles::recompute_platform(&tx, &p.0, &req.prefs)?;
        }
        dat_stage::clear(&tx)?;
        crate::db::commit(tx)?;
        Ok(Outcome::Loaded(Loaded {
            version: plan.id,
            platform: plan.platform_id,
            games,
            has_titles: platform.is_some(),
            retired,
        }))
    })
}

/// Stops on shutdown, sleeps briefly while a core runs and waits out a
/// manual pause, which holds the background lane; checked every few games.
fn pace(req: &Request, games: u64) -> Result<()> {
    if games.is_multiple_of(CANCEL_EVERY) && *req.stop.borrow() {
        return Err(Error::Cancelled);
    }
    if !games.is_multiple_of(YIELD_EVERY) {
        return Ok(());
    }
    if req.gate.borrow().core_running() {
        std::thread::sleep(YIELD_FOR);
    }
    while req.gate.borrow().hold(Lane::Background).is_some() {
        if *req.stop.borrow() {
            return Err(Error::Cancelled);
        }
        std::thread::sleep(PAUSED_POLL);
    }
    Ok(())
}

/// Appends one chunk of parsed games to the stage in its own transaction.
fn append_chunk(conn: &mut Connection, chunk: &[StagedGame]) -> Result<()> {
    let tx = conn.transaction()?;
    dat_stage::append(&tx, chunk)?;
    crate::db::commit(tx)?;
    Ok(())
}

/// Parses a game's name into the row its title is stored from; regions, languages and
/// stage flags the name lacks come from the DAT's own fields.
fn staged(game: &DatGame) -> StagedGame {
    let parsed = parse_name(&game.name);
    let mut regions: Vec<String> = parsed.regions.iter().map(|r| r.name().to_owned()).collect();
    if regions.is_empty() {
        regions.clone_from(&game.regions);
    }
    let languages = if parsed.languages.is_empty() {
        game.languages.clone()
    } else {
        parsed.languages.clone()
    };
    let mut flags = parsed.flag_labels();
    for flag in game.status_flags() {
        if !parsed.flags.contains(&flag) {
            flags.push(flag.to_string());
        }
    }
    StagedGame {
        name: game.name.clone(),
        base_name: parsed.base_name.clone(),
        group_key: group_key(&parsed),
        clone_of: game.clone_of.clone(),
        regions,
        languages,
        revision: parsed.revision.as_ref().map(|r| r.label.clone()),
        flags,
        roms: game
            .roms
            .iter()
            .map(|r| StagedRom {
                name: r.name.clone(),
                size: r.size,
                crc32: r.crc32.clone(),
                md5: r.md5.clone(),
                sha1: r.sha1.clone(),
                status: r.status.as_str().to_owned(),
                header: r.header.clone(),
            })
            .collect(),
    }
}

/// Queues the recompute job, which matches files of retired roms and unmatched files
/// again from their stored hashes, and an automatic scan for each platform a DAT just
/// loaded titles for,
/// deduped so several DATs in one pack queue at most one each, binds waiting
/// sources once for the whole pack, then checks whether the wizard just
/// became complete.
async fn enqueue_follow_up_work(app: &Arc<AppState>, loaded: &[Loaded]) {
    let mut queued = HashSet::new();
    for l in loaded {
        let Some(platform) = &l.platform else {
            continue;
        };
        if !queued.insert(platform.clone()) {
            continue;
        }
        // Files of retired roms and unmatched files are matched again in the background.
        if let Err(e) = Scheduler::enqueue(app, Arc::new(Recompute::new(&platform.0))).await {
            tracing::warn!(platform = %platform.0, error = %e, "cannot enqueue the recompute");
        }
        if let Err(e) = scan::enqueue_if_games_dir_exists(app, platform).await {
            tracing::warn!(platform = %platform.0, error = %e, "cannot enqueue automatic scan");
        }
    }
    if !queued.is_empty() {
        let platforms: Vec<_> = queued.into_iter().collect();
        if let Err(e) = super::source_import::rebind_after_dat(app, &platforms).await {
            tracing::warn!(error = %e, "cannot bind waiting sources");
        }
    }
    if let Err(e) = wizard::on_change(app).await {
        tracing::warn!(error = %e, "cannot check wizard completion");
    }
}

fn publish_loaded(app: &AppState, l: &Loaded, file: &str) {
    app.events.publish(
        EventKind::DatLoaded,
        &json!({ "dat_version_id": l.version, "file": file, "platform_id": l.platform }),
    );
}

fn publish_rejected(app: &AppState, file: &str, reason: &str) {
    app.events.publish(
        EventKind::DatRejected,
        &json!({ "file": file, "reason": reason }),
    );
}

/// Moves a file into `rejected/` with `<name>.reason.txt` beside it and
/// publishes `dat.rejected`.
fn reject(app: &AppState, path: &Path, file: &str, reason: &str) -> Result<()> {
    let dir = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(REJECTED_DIR);
    std::fs::create_dir_all(&dir)?;
    let target = unique_path(&dir, file);
    std::fs::rename(path, &target)?;
    let mut reason_path = target.into_os_string();
    reason_path.push(REASON_SUFFIX);
    std::fs::write(reason_path, format!("{reason}\n"))?;
    tracing::warn!(file, reason, "DAT rejected");
    publish_rejected(app, file, reason);
    Ok(())
}

/// `dir/name`, or `dir/stem (N).ext` for the first N that is free.
///
/// ```
/// let dir = tempfile::tempdir().unwrap();
/// let first = mistarr_server::jobs::dat_import::unique_path(dir.path(), "a.dat");
/// std::fs::write(&first, b"").unwrap();
/// let second = mistarr_server::jobs::dat_import::unique_path(dir.path(), "a.dat");
/// assert!(second.ends_with("a (1).dat"));
/// ```
#[must_use]
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let p = Path::new(name);
    let stem = stem(p);
    let ext = p
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    (1..=u32::MAX)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|c| !c.exists())
        .unwrap_or(candidate)
}

/// Files matched again per transaction by [`rematch_chunk`] and [`match_unmatched_chunk`].
const REMATCH_CHUNK: u32 = 256;

/// Matches up to [`REMATCH_CHUNK`] files of retired roms on `platform` again, by their
/// stored hashes against live roms only, and returns how many it took. A file no live
/// rom lists becomes `unverified`; see [`set_matches`] for the states.
///
/// # Errors
///
/// [`Error::Db`] on SQLite failure.
pub(crate) fn rematch_chunk(conn: &Connection, platform: &PlatformId) -> Result<usize> {
    let orphans = files::retired_matches(conn, platform, REMATCH_CHUNK)?;
    set_matches(conn, platform, &orphans)?;
    Ok(orphans.len())
}

/// One page of [`match_unmatched_chunk`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UnmatchedChunk {
    /// Files read; fewer than [`REMATCH_CHUNK`] means the last page.
    pub(crate) read: usize,
    /// The highest id read, the cursor for the next page.
    pub(crate) last: FileId,
    /// Files that went from no rom to a rom.
    pub(crate) matched: usize,
}

/// Matches up to [`REMATCH_CHUNK`] unmatched files on `platform` with an id above
/// `after` against live roms by their stored hashes, without reading the files. Paging
/// by id reads a file that stays unmatched once per run.
///
/// # Errors
///
/// [`Error::Db`] on SQLite failure.
pub(crate) fn match_unmatched_chunk(
    conn: &Connection,
    platform: &PlatformId,
    after: FileId,
) -> Result<UnmatchedChunk> {
    let rows = files::unmatched_after(conn, platform, after, REMATCH_CHUNK)?;
    let matched = set_matches(conn, platform, &rows)?;
    Ok(UnmatchedChunk {
        read: rows.len(),
        last: rows.last().map_or(after, |f| f.id),
        matched,
    })
}

/// Matches `rows` again by their stored hashes against live roms and returns how many
/// went from no rom to a rom. A cartridge file takes the state a scan would give it; a
/// disc track is classified again with the other tracks of its directory by the scan's
/// all-or-nothing rule.
fn set_matches(conn: &Connection, platform: &PlatformId, rows: &[FileRow]) -> Result<usize> {
    let disc = mistarr_mister::platforms::by_id(&platform.0)
        .is_some_and(|p| p.kind == mistarr_mister::Kind::Disc);
    let mut matched = 0;
    let mut units: Vec<&str> = Vec::new();
    for f in rows {
        if let Some((dir, _)) = f.rel_path.rsplit_once('/').filter(|_| disc) {
            if !units.contains(&dir) {
                units.push(dir);
            }
            continue;
        }
        let m = scan::stored_match(conn, platform, f)?;
        let (rom, state) = scan::cartridge_state(m.as_ref(), scan::own_name(&f.rel_path));
        matched += usize::from(f.rom_id.is_none() && rom.is_some());
        set_changed(conn, f, rom, state)?;
    }
    for dir in units {
        let rows = files::in_directory(conn, platform, dir)?;
        let mut tracks = Vec::with_capacity(rows.len());
        for f in &rows {
            tracks.push(scan::Track {
                rel_path: f.rel_path.clone(),
                name: files::basename(&f.rel_path).to_owned(),
                size: f.size,
                mtime: f.mtime,
                hashes: stored_hashes(f),
                matched: scan::stored_match(conn, platform, f)?,
            });
        }
        for (f, t) in rows.iter().zip(scan::classify_disc_tracks(conn, tracks)?) {
            matched += usize::from(f.rom_id.is_none() && t.rom_id.is_some());
            set_changed(conn, f, t.rom_id, t.state)?;
        }
    }
    Ok(matched)
}

/// [`files::set_match`] only when the rom or state differs, so a recompute that changes
/// nothing writes nothing.
fn set_changed(conn: &Connection, f: &FileRow, rom: Option<i64>, state: FileState) -> Result<()> {
    if f.rom_id == rom && f.state == state {
        return Ok(());
    }
    files::set_match(conn, f.id, rom, state)
}

fn stored_hashes(f: &FileRow) -> Option<mistarr_core::HashSet> {
    Some(mistarr_core::HashSet {
        size: u64::try_from(f.size).ok()?,
        crc32: f.crc32.clone()?,
        md5: f.md5.clone()?,
        sha1: f.sha1.clone()?,
    })
}

/// Matches files of retired roms again, then, outside arcade, the platform's unmatched
/// files; recomputes the 1G1R picks of one platform under the current preferences, then
/// queues a re-map of its bound sources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recompute {
    platform: PlatformId,
}

impl Recompute {
    /// A job for `platform`.
    ///
    /// ```
    /// use mistarr_server::jobs::{dat_import::Recompute, Job};
    /// assert_eq!(Recompute::new("nes").payload()["platform_id"], "nes");
    /// ```
    #[must_use]
    pub fn new(platform: &str) -> Self {
        Self {
            platform: PlatformId(platform.to_owned()),
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
            Scheduler::enqueue(app, Arc::new(Self::new(&p.id.0))).await?;
        }
        Ok(())
    }
}

#[async_trait]
impl Job for Recompute {
    fn kind(&self) -> &'static str {
        RECOMPUTE_KIND
    }

    fn payload(&self) -> Value {
        json!({ "platform_id": self.platform.0 })
    }

    fn lane(&self) -> Lane {
        Lane::Background
    }

    async fn run(&self, ctx: &JobContext) -> Result<()> {
        loop {
            ctx.checkpoint().await?;
            let platform = self.platform.clone();
            let taken = ctx
                .app
                .db
                .write(move |c| {
                    let tx = c.transaction()?;
                    let taken = rematch_chunk(&tx, &platform)?;
                    crate::db::commit(tx)?;
                    Ok(taken)
                })
                .await?;
            if taken < REMATCH_CHUNK as usize {
                break;
            }
        }
        let mut after = FileId(0);
        let mut matched = 0;
        // Arcade files are matched by the arcade catalogue's presence pass.
        while !scan::is_arcade(&self.platform) {
            ctx.checkpoint().await?;
            let platform = self.platform.clone();
            let chunk = ctx
                .app
                .db
                .write(move |c| {
                    let tx = c.transaction()?;
                    let chunk = match_unmatched_chunk(&tx, &platform, after)?;
                    crate::db::commit(tx)?;
                    Ok(chunk)
                })
                .await?;
            matched += chunk.matched;
            after = chunk.last;
            if chunk.read < REMATCH_CHUNK as usize {
                break;
            }
        }
        ctx.checkpoint().await?;
        let prefs = prefs(&ctx.app.config().prefs);
        let platform = self.platform.0.clone();
        let r = ctx
            .app
            .db
            .write(move |c| {
                let tx = c.transaction()?;
                let r = titles::recompute_platform(&tx, &platform, &prefs)?;
                crate::db::commit(tx)?;
                Ok(r)
            })
            .await?;
        ctx.progress(json!({ "groups": r.groups, "picks": r.picks, "matched": matched }))
            .await?;
        // Groups are settled now; a re-map that ran earlier stored a stamp without them.
        super::remap::enqueue(&ctx.app, Some(vec![self.platform.clone()])).await;
        Ok(())
    }
}

/// Finds files in `dats/` that have stopped changing: mtime at least
/// `min_age` old and size equal across two polls. Each file is reported once
/// per size and mtime, so a new file under a reused name is reported again.
#[derive(Debug)]
pub struct DatWatcher {
    min_age: Duration,
    sizes: HashMap<PathBuf, u64>,
    reported: HashMap<PathBuf, (u64, Option<SystemTime>)>,
}

impl DatWatcher {
    /// A watcher with the given stability age.
    ///
    /// ```
    /// let dir = tempfile::tempdir().unwrap();
    /// std::fs::write(dir.path().join("a.dat"), b"x").unwrap();
    /// let mut w = mistarr_server::jobs::dat_import::DatWatcher::new(std::time::Duration::ZERO);
    /// assert!(w.poll(dir.path()).is_empty());
    /// assert_eq!(w.poll(dir.path()).len(), 1);
    /// assert!(w.poll(dir.path()).is_empty());
    /// ```
    #[must_use]
    pub fn new(min_age: Duration) -> Self {
        Self {
            min_age,
            sizes: HashMap::new(),
            reported: HashMap::new(),
        }
    }

    /// Reports `path` again once it is stable, for a file whose import failed.
    pub fn forget(&mut self, path: &Path) {
        self.reported.remove(path);
    }

    /// Regular files in `dir`, not dotfiles, that became stable since the last poll.
    pub fn poll(&mut self, dir: &Path) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let now = SystemTime::now();
        let mut present = HashSet::new();
        let mut stable = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if !meta.is_file() || entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            present.insert(path.clone());
            let old = meta
                .modified()
                .is_ok_and(|m| now.duration_since(m).unwrap_or(Duration::ZERO) >= self.min_age);
            let same = self.sizes.insert(path.clone(), meta.len()) == Some(meta.len());
            let signature = (meta.len(), meta.modified().ok());
            if old && same && self.reported.get(&path) != Some(&signature) {
                self.reported.insert(path.clone(), signature);
                stable.push(path);
            }
        }
        self.sizes.retain(|p, _| present.contains(p));
        self.reported.retain(|p, _| present.contains(p));
        stable.sort();
        stable
    }
}

/// Polls `dats/` every `options.dats_poll` and enqueues a [`DatImport`] per
/// stable file. A file whose job failed is enqueued again on a later poll.
pub async fn watch(app: Arc<AppState>) {
    let dir = app.config().paths.dats();
    let mut watcher = DatWatcher::new(app.options.dats_min_age);
    let mut pending: HashMap<PathBuf, JobId> = HashMap::new();
    let mut tick = tokio::time::interval(app.options.dats_poll);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let mut finished = Vec::new();
        for (path, &id) in &pending {
            match app.db.read(move |c| crate::db::jobs::get(c, id)).await {
                Ok(Some(row)) if row.state == JobState::Failed => {
                    watcher.forget(path);
                    finished.push(path.clone());
                }
                Ok(Some(row)) if !row.state.is_finished() => {}
                Ok(_) => finished.push(path.clone()),
                Err(e) => tracing::warn!(error = %e, "cannot read DAT import job"),
            }
        }
        for path in finished {
            pending.remove(&path);
        }
        for path in watcher.poll(&dir) {
            match Scheduler::enqueue(&app, Arc::new(DatImport::new(&path))).await {
                Ok(id) => {
                    pending.insert(path, id);
                }
                Err(e) => tracing::warn!(error = %e, "cannot enqueue DAT import"),
            }
        }
    }
}

#[cfg(test)]
mod tests;
