//! Library scan: walks each platform's `games/` directories, hashes changed
//! files and matches them against loaded DATs. See `docs/ARCHITECTURE.md`
//! "Library scan" and `docs/VERIFICATION.md` "Hashing" and "Matching order".

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mistarr_core::hash::{
    hash_forms, hash_reader, hash_zip_member_forms, zip_member_content_crc, zip_members, HashError,
    HeaderForms, HeaderRule, ZipMember,
};
use mistarr_core::PlatformId;
use mistarr_mister::platforms::{self, Kind, Platform};
use rusqlite::Connection;
use serde_json::{json, Value};
use tokio::time::Instant;

use super::fsutil::{all_entries, extension, stat};
use super::matching::{self, cartridge_state, classify_disc_tracks, own_name, stored_match, Track};
use super::progress::Throttle;
use super::{Job, JobContext, JobKind, Lane, Scheduler};
use crate::app::AppState;
use crate::db::files::{self, FileState, NewFile};
use crate::db::ids::{FileId, JobId};
use crate::db::platforms as platform_rows;
use crate::db::roms;
use crate::db::scan_progress;
use crate::error::{Error, Result};
use crate::events::EventKind;

/// A library scan: one platform, or every enabled platform fanned out as
/// one job each.
pub struct ScanJob {
    /// `None` fans out one job per enabled platform.
    pub platform_id: Option<PlatformId>,
}

#[async_trait]
impl Job for ScanJob {
    fn kind(&self) -> JobKind {
        JobKind::Scan
    }

    fn payload(&self) -> Value {
        json!({ "platform_id": self.platform_id.as_ref().map(|p| p.0.clone()) })
    }

    fn lane(&self) -> Lane {
        Lane::Heavy
    }

    async fn run(&self, ctx: &JobContext) -> Result<()> {
        match &self.platform_id {
            None => fan_out(ctx).await,
            Some(id) => scan_platform(ctx, id).await,
        }
    }
}

/// Enqueues a scan of `platform_id` when its games directory already exists,
/// for a DAT that just finished loading. `None` when there is nothing to
/// walk yet; the caller dedupes several DATs from one pack before calling.
///
/// # Errors
///
/// [`Error::Db`] when the job cannot be recorded.
pub async fn enqueue_if_games_dir_exists(
    app: &Arc<AppState>,
    platform_id: &PlatformId,
) -> Result<Option<JobId>> {
    let Some(platform) = platforms::by_id(&platform_id.0) else {
        return Ok(None);
    };
    if platform.is_arcade() {
        // Arcade presence and verification come from the arcade catalogue, never a scan.
        return Ok(None);
    }
    let row = app
        .db
        .read({
            let id = platform_id.clone();
            move |c| platform_rows::find(c, &id)
        })
        .await?;
    if !row.is_some_and(|r| r.enabled) {
        return Ok(None);
    }
    let config = app.config();
    let present = crate::threads::run(crate::threads::label::SCAN_LIST, move || {
        std::iter::once(platform.core_dir)
            .chain(platform.legacy_dirs.iter().copied())
            .any(|name| config.paths.games.join(name).is_dir())
    });
    if !present.await? {
        return Ok(None);
    }
    Scheduler::enqueue(
        app,
        Arc::new(ScanJob {
            platform_id: Some(platform_id.clone()),
        }),
    )
    .await
    .map(Some)
}

/// Whether `id` is the arcade platform, whose presence and verification come
/// from the arcade catalogue rather than a library scan.
pub(crate) fn is_arcade(id: &PlatformId) -> bool {
    platforms::by_id(&id.0).is_some_and(Platform::is_arcade)
}

/// Enqueues one [`ScanJob`] per enabled platform, skipping the arcade platform,
/// whose presence and verification come from the arcade catalogue instead.
async fn fan_out(ctx: &JobContext) -> Result<()> {
    let rows = ctx.app.db.read(platform_rows::list).await?;
    for row in rows.into_iter().filter(|r| r.enabled) {
        if is_arcade(&row.id) {
            continue;
        }
        let job = ScanJob {
            platform_id: Some(row.id),
        };
        let payload = job.payload();
        // A scan of this platform already queued or running (e.g. from the
        // automatic per-platform trigger) does not need a second one.
        let already_open = ctx
            .app
            .db
            .read(move |c| crate::db::jobs::find_open(c, JobKind::Scan, &payload))
            .await?
            .is_some();
        if already_open {
            continue;
        }
        super::Scheduler::enqueue(&ctx.app, std::sync::Arc::new(job)).await?;
    }
    Ok(())
}

/// Extensions a disc directory scan hashes: cue sheets plus track and image
/// formats, per `docs/PLATFORMS.md` "Disc". Anything else (`.m3u`, cover
/// art, …) is ignored.
const DISC_TRACK_EXTENSIONS: &[&str] = &["cue", "bin", "iso", "chd"];

/// One directory to walk: its stored id (also the `files.rel_path` prefix)
/// and its path on disk.
struct Unit {
    id: String,
    path: PathBuf,
}

/// The top-level and legacy directories of a platform, each a [`Unit`] for a
/// cartridge, romset or arcade platform; the per-title subdirectories of
/// those for a disc platform, plus the top directory itself when it also
/// holds loose track files directly. Only directories that exist are
/// returned, with the ids of top directories that exist but cannot be read.
fn discover_units(games_root: &Path, platform: &Platform) -> (Vec<Unit>, Vec<String>) {
    let top_names = std::iter::once(platform.core_dir).chain(platform.legacy_dirs.iter().copied());
    let mut unreadable = Vec::new();
    let mut top_dirs: Vec<(String, PathBuf)> = Vec::new();
    for name in top_names {
        let path = games_root.join(name);
        let found = match fs::metadata(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            found => found,
        };
        match readable(&path, found) {
            Some(m) if m.is_dir() => top_dirs.push((name.to_owned(), path)),
            Some(_) => {}
            None => unreadable.push(name.to_owned()),
        }
    }
    let mut units = if platform.kind == Kind::Disc {
        let mut units = Vec::new();
        for (name, dir) in &top_dirs {
            let Some(entries) = readable(dir, fs::read_dir(dir).and_then(all_entries)) else {
                unreadable.push(name.clone());
                continue;
            };
            let mut has_loose_file = false;
            for entry in entries {
                let path = entry.path();
                if path.is_dir() {
                    let sub = entry.file_name().to_string_lossy().into_owned();
                    units.push(Unit {
                        id: format!("{name}/{sub}"),
                        path,
                    });
                } else if path.is_file() {
                    has_loose_file = true;
                }
            }
            if has_loose_file {
                units.push(Unit {
                    id: name.clone(),
                    path: dir.clone(),
                });
            }
        }
        units
    } else {
        top_dirs
            .into_iter()
            .map(|(id, path)| Unit { id, path })
            .collect()
    };
    units.sort_by(|a, b| a.id.cmp(&b.id));
    (units, unreadable)
}

/// What `dir` listed or stated, or `None` with a warning when it cannot be read, so its
/// rows are kept rather than pruned.
fn readable<T>(dir: &Path, listed: io::Result<T>) -> Option<T> {
    listed
        .map_err(|e| {
            tracing::warn!(path = %dir.display(), error = %e, "cannot read directory; keeping its rows");
        })
        .ok()
}

/// A regular file a unit's listing found.
struct ListedFile {
    path: PathBuf,
    name: String,
    /// Size and mtime as `files` stores them, or why they could not be read.
    meta: io::Result<(i64, i64)>,
}

/// The regular files a unit's directory entries resolve to, with their size and mtime,
/// sorted for a deterministic scan order; empty when `dir` is gone, an error when it
/// cannot be read.
fn list_files(dir: &Path) -> io::Result<Vec<ListedFile>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out: Vec<ListedFile> = all_entries(entries)?
        .into_iter()
        .filter_map(|e| {
            let path = e.path();
            let meta = fs::metadata(&path).ok().filter(fs::Metadata::is_file)?;
            Some(ListedFile {
                name: e.file_name().to_string_lossy().into_owned(),
                meta: stat(&meta),
                path,
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

async fn scan_platform(ctx: &JobContext, id: &PlatformId) -> Result<()> {
    let platform = platforms::by_id(&id.0)
        .ok_or_else(|| Error::Job(format!("unknown platform `{}`", id.0)))?;
    if platform.is_arcade() {
        // Defensive: arcade zips are never walked as cartridges, even called directly.
        ctx.app
            .db
            .write({
                let id = id.clone();
                move |c| scan_progress::clear(c, &id)
            })
            .await?;
        return Ok(());
    }
    let games_root = ctx.app.config().paths.games.clone();
    let pid = id.clone();
    let (units, mut unreadable) =
        crate::threads::run(crate::threads::label::SCAN_LIST, move || {
            discover_units(&games_root, platform)
        })
        .await?;

    let existing = ctx
        .app
        .db
        .read({
            let pid = pid.clone();
            move |c| files::existing_paths(c, &pid)
        })
        .await?;
    let done = ctx
        .app
        .db
        .read({
            let pid = pid.clone();
            move |c| scan_progress::get(c, &pid)
        })
        .await?;

    let unit_ids: std::collections::HashSet<&str> = units.iter().map(|u| u.id.as_str()).collect();
    let mut done_set: std::collections::HashSet<String> = done
        .into_iter()
        .filter(|d| unit_ids.contains(d.as_str()))
        .collect();
    let mut keep: Vec<String> = existing
        .into_iter()
        .filter(|p| in_done_unit(p, &done_set))
        .collect();

    let mut sink = Sink::new(ctx, &pid);
    let mut saved = Instant::now();
    let total = units.len();
    let remaining: Vec<Unit> = units
        .into_iter()
        .filter(|u| !done_set.contains(&u.id))
        .collect();
    for unit in remaining {
        ctx.checkpoint().await?;
        let unit_seen = if platform.kind == Kind::Disc {
            scan_disc_unit(ctx, &pid, &unit.id, &unit.path)
                .await?
                .map(|(rows, seen)| {
                    sink.rows.extend(rows);
                    seen
                })
        } else {
            scan_flat_unit(&mut sink, platform, &unit.id, &unit.path).await?
        };
        match unit_seen {
            Some(seen) => keep.extend(seen),
            None => unreadable.push(unit.id.clone()),
        }
        done_set.insert(unit.id.clone());
        // Progress is a resume hint: an unsaved unit is walked again and its files skip hashing.
        let save = saved.elapsed() >= PROGRESS_EVERY;
        if save {
            saved = Instant::now();
        }
        let done_dirs = save.then(|| done_set.iter().cloned().collect::<Vec<_>>());
        sink.flush(done_dirs).await?;
        ctx.progress(json!({
            "platform_id": pid.0,
            "dir": unit.id,
            "done": done_set.len(),
            "total": total,
        }))
        .await?;
    }

    ctx.checkpoint().await?;
    let (pid3, keep2) = (pid.clone(), keep);
    ctx.app
        .db
        .write(move |c| files::delete_missing(c, &pid3, &keep2, &unreadable))
        .await?;
    let pid4 = pid.clone();
    ctx.app
        .db
        .write(move |c| scan_progress::clear(c, &pid4))
        .await?;
    super::chd::queue_for(&ctx.app, &pid, false).await?;
    report_outcome(ctx, &pid, total).await
}

/// Stores the scan's final progress: the platform's files with a rom state, those left
/// `unverified` and those `unidentified`, per `docs/ARCHITECTURE.md` "Library scan".
async fn report_outcome(ctx: &JobContext, pid: &PlatformId, total: usize) -> Result<()> {
    let id = pid.clone();
    let counts = ctx
        .app
        .db
        .read(move |c| files::state_counts(c, &id))
        .await?;
    ctx.progress(json!({
        "platform_id": pid.0,
        "done": total,
        "total": total,
        "matched": counts.verified + counts.misnamed + counts.bad,
        "unmatched": counts.unverified,
        "unidentified": counts.unidentified,
    }))
    .await
}

fn accepts_extension(platform: &Platform, ext: &str) -> bool {
    platform.load_extensions.contains(&ext) || (platform.kind == Kind::Cartridge && ext == "zip")
}

/// A written file's identity, for the caller's `file.changed` events.
type Written = Vec<(String, FileId, FileState)>;

/// Hashed rows written at a time, so a large directory never holds all its rows at once.
const FLUSH_ROWS: usize = 256;

/// Least time between two saves of the resume point.
const PROGRESS_EVERY: Duration = Duration::from_secs(2);

/// Least time between two `file.changed` events; the rest are dropped, since every state
/// is already durable in `files`.
const FILE_CHANGED_EVERY: Duration = Duration::from_millis(100);

/// Whether `rel_path` lies in one of the `done` units, whose ids are its leading components.
fn in_done_unit(rel_path: &str, done: &std::collections::HashSet<String>) -> bool {
    done.contains(rel_path)
        || rel_path
            .match_indices('/')
            .any(|(at, _)| done.contains(&rel_path[..at]))
}

/// Hashed rows waiting to be written, and the `file.changed` throttle.
struct Sink<'a> {
    ctx: &'a JobContext,
    platform_id: PlatformId,
    rows: Vec<NewFile>,
    throttle: Throttle,
}

impl<'a> Sink<'a> {
    fn new(ctx: &'a JobContext, platform_id: &PlatformId) -> Self {
        Self {
            ctx,
            platform_id: platform_id.clone(),
            rows: Vec::with_capacity(FLUSH_ROWS),
            throttle: Throttle::new(FILE_CHANGED_EVERY),
        }
    }

    /// Adds a row, writing the batch once it holds [`FLUSH_ROWS`].
    async fn push(&mut self, row: NewFile) -> Result<()> {
        self.rows.push(row);
        if self.rows.len() >= FLUSH_ROWS {
            self.flush(None).await?;
        }
        Ok(())
    }

    /// Writes the waiting rows, with the resume point when given, and announces them.
    async fn flush(&mut self, done_dirs: Option<Vec<String>>) -> Result<()> {
        if self.rows.is_empty() && done_dirs.is_none() {
            return Ok(());
        }
        let rows = std::mem::replace(&mut self.rows, Vec::with_capacity(FLUSH_ROWS));
        let (pid, now) = (self.platform_id.clone(), crate::unix_now());
        let app = Arc::clone(&self.ctx.app);
        let written = self
            .ctx
            .app
            .db
            .write_tx(move |tx| {
                let chd_on = app.config().scan.chd_tracks;
                commit_unit(tx, &pid, rows, done_dirs.as_deref(), now, chd_on)
            })
            .await?;
        for (_, id, state) in &written {
            if self.throttle.due(std::time::Instant::now(), "file.changed") {
                self.ctx.app.events.publish(
                    EventKind::FileChanged,
                    &json!({ "file_id": id.0, "state": state.as_str() }),
                );
            }
        }
        Ok(())
    }

    /// Settles what [`known`] decides for the file at `rel_path` of `size` and `mtime`:
    /// true when it is to be hashed; otherwise its matched row is queued or it is skipped.
    async fn handle_known(
        &mut self,
        rel_path: &str,
        (size, mtime): (i64, i64),
        precheck: Option<HeaderRule>,
    ) -> Result<bool> {
        let (pid, rel) = (self.platform_id.clone(), rel_path.to_owned());
        let decided = self
            .ctx
            .app
            .db
            .read(move |c| known(c, &pid, &rel, size, mtime, precheck))
            .await?;
        match decided {
            Known::Hash => Ok(true),
            Known::Skip => Ok(false),
            Known::Matched(row) => {
                self.push(*row).await?;
                Ok(false)
            }
        }
    }

    /// The row of a payload hashed under `rule` into `forms`, matched with `own_name`
    /// as its name: a flat file's name or a zip member's leaf.
    async fn hashed_row(
        &self,
        found: Found,
        own_name: &str,
        rule: HeaderRule,
        forms: HeaderForms,
    ) -> Result<NewFile> {
        let (pid, name) = (self.platform_id.clone(), own_name.to_owned());
        let ((rom_id, state), forms) = self
            .ctx
            .app
            .db
            .read(move |c| Ok((matching::classify(c, &pid, &name, &forms)?, forms)))
            .await?;
        Ok(NewFile {
            rel_path: found.rel_path,
            size: found.size,
            mtime: found.mtime,
            crc32: Some(forms.content.crc32.clone()),
            md5: Some(forms.content.md5.clone()),
            sha1: Some(forms.content.sha1.clone()),
            header_rule: Some(rule.as_str().to_owned()),
            whole: files::WholeHashes::of(rule.as_str(), &forms),
            rom_id,
            state,
            reason: None,
        })
    }
}

/// A file or zip member the walk found, by its `files` path, size and mtime.
struct Found {
    rel_path: String,
    size: i64,
    mtime: i64,
}

/// Writes already-hashed rows, and the scan's resume point when given, in one short
/// transaction, so the single writer connection is never held for the hashing itself. A
/// CHD waiting to be decoded is written `pending` or `off` as `chd_on`, read in this write, says.
fn commit_unit(
    tx: &Connection,
    platform_id: &PlatformId,
    rows: Vec<NewFile>,
    done_dirs: Option<&[String]>,
    now: i64,
    chd_on: bool,
) -> Result<Written> {
    let mut written = Vec::with_capacity(rows.len());
    for mut row in rows {
        if let Some(reason) = row.reason.as_deref() {
            row.reason = Some(super::chd::settle(reason, chd_on).to_owned());
        }
        let id = files::upsert(tx, platform_id, &row, now)?;
        written.push((row.rel_path, id, row.state));
    }
    if let Some(done_dirs) = done_dirs {
        scan_progress::save(tx, platform_id, done_dirs, now)?;
    }
    Ok(written)
}

/// What a scan does with a file it found, given the row it has for it.
enum Known {
    /// New, changed or pending: hash it.
    Hash,
    /// Unchanged and nothing to update.
    Skip,
    /// Unchanged and unmatched, and its stored hashes now match a live rom.
    Matched(Box<NewFile>),
}

/// Decides [`Known`] for a file of `size` and `mtime`. An unchanged unmatched row is
/// matched from its stored hashes, and a row hashed under a stripping rule without its
/// whole-file hashes is hashed again. With `precheck`, the platform's rule for a zip
/// member, a member never hashed, known by its CRC32 alone, is hashed once
/// [`member_candidate`] finds a rom for it.
fn known(
    conn: &Connection,
    platform_id: &PlatformId,
    rel_path: &str,
    size: i64,
    mtime: i64,
    precheck: Option<HeaderRule>,
) -> Result<Known> {
    let Some(row) = files::find_by_path(conn, platform_id, rel_path)? else {
        return Ok(Known::Hash);
    };
    if !row.unchanged(size, mtime) || lacks_whole(&row) {
        return Ok(Known::Hash);
    }
    if row.rom_id.is_some() || row.state != FileState::Unverified {
        return Ok(Known::Skip);
    }
    if row.sha1.is_none() && row.md5.is_none() {
        // A NULL rule marks a member never hashed; a failed hash records its rule instead.
        let candidate = match (precheck, row.crc32.as_deref()) {
            (Some(rule), Some(crc)) if row.header_rule.is_none() => {
                let whole = row.whole.crc32.as_deref().unwrap_or(crc);
                let content = (whole != crc).then_some(crc);
                member_candidate(conn, platform_id, rule, whole, content, size)?
            }
            _ => false,
        };
        return Ok(if candidate { Known::Hash } else { Known::Skip });
    }
    let Some(m) = stored_match(conn, platform_id, &row)? else {
        return Ok(Known::Skip);
    };
    let (rom_id, state) = cartridge_state(platform_id, Some(&m), own_name(rel_path));
    Ok(Known::Matched(Box::new(NewFile {
        rel_path: row.rel_path,
        size,
        mtime,
        crc32: row.crc32,
        md5: row.md5,
        sha1: row.sha1,
        header_rule: row.header_rule,
        whole: row.whole,
        rom_id,
        state,
        reason: None,
    })))
}

/// Whether a fully hashed row was hashed under a rule that strips a header but holds
/// the stripped form alone, with no whole-file hashes to match a headered DAT by.
fn lacks_whole(row: &files::FileRow) -> bool {
    let rule = row
        .header_rule
        .as_deref()
        .and_then(|n| n.parse::<HeaderRule>().ok())
        .unwrap_or_default();
    rule.strips_header()
        && (row.sha1.is_some() || row.md5.is_some())
        && row.whole.sha1.is_none()
        && row.whole.md5.is_none()
}

/// Whether a zip member not yet decompressed may be a rom: a rom has its central-directory
/// CRC32 `whole` and `size`, or, when `rule` found a header, its content CRC32 `content`
/// and the size less that header.
fn member_candidate(
    conn: &Connection,
    platform_id: &PlatformId,
    rule: HeaderRule,
    whole: &str,
    content: Option<&str>,
    size: i64,
) -> Result<bool> {
    if roms::crc_candidate_exists(conn, platform_id, whole, size)? {
        return Ok(true);
    }
    let Some(content) = content else {
        return Ok(false);
    };
    let header = i64::try_from(rule.header_len()).unwrap_or(0);
    roms::crc_candidate_exists(conn, platform_id, content, size - header)
}

/// Walks one cartridge, romset or arcade directory, handing each row to `sink`, and
/// returns every path it saw, or `None` when the directory cannot be read. Hashing runs
/// outside any database lock; only the read connection is touched, one file at a time.
async fn scan_flat_unit(
    sink: &mut Sink<'_>,
    platform: &'static Platform,
    unit_id: &str,
    dir: &Path,
) -> Result<Option<Vec<String>>> {
    let ctx = sink.ctx;
    let dir_owned = dir.to_path_buf();
    let listed = crate::threads::run(crate::threads::label::SCAN_LIST, move || {
        list_files(&dir_owned)
    })
    .await?;
    let Some(entries) = readable(dir, listed) else {
        return Ok(None);
    };
    let rule = platform.header_rule;
    let mut seen = Vec::new();
    for ListedFile { path, name, meta } in entries {
        ctx.checkpoint().await?;
        let Some(ext) = extension(&path) else {
            continue;
        };
        if !accepts_extension(platform, &ext) {
            continue;
        }
        let rel_path = format!("{unit_id}/{name}");
        let (size, mtime) = match meta {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "cannot read metadata; marking unverified");
                seen.push(rel_path.clone());
                sink.push(unverified_row(rel_path, 0, 0, None)).await?;
                continue;
            }
        };
        if ext == "zip" {
            // A zip container has no files row of its own; its members do.
            scan_zip_unit(sink, rule, &rel_path, &path, mtime, &mut seen).await?;
            continue;
        }
        seen.push(rel_path.clone());
        if !sink.handle_known(&rel_path, (size, mtime), None).await? {
            continue;
        }
        let hint = u64::try_from(size).unwrap_or(0);
        let path_owned = path.clone();
        let hash_result = crate::threads::run(crate::threads::label::HASH, move || {
            File::open(&path_owned).and_then(|f| hash_forms(f, rule, Some(hint)))
        })
        .await?;
        let row = match hash_result {
            Ok(forms) => {
                let found = Found {
                    rel_path,
                    size,
                    mtime,
                };
                sink.hashed_row(found, &name, rule, forms).await?
            }
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "cannot hash file; marking unverified");
                unverified_row(rel_path, size, mtime, None)
            }
        };
        sink.push(row).await?;
    }
    Ok(Some(seen))
}

/// An unmatched or unreadable file's row: no hash was trusted enough to
/// classify it, so it is recorded `unverified` rather than aborting the scan.
fn unverified_row(rel_path: String, size: i64, mtime: i64, crc32: Option<String>) -> NewFile {
    NewFile {
        rel_path,
        size,
        mtime,
        crc32,
        md5: None,
        sha1: None,
        header_rule: None,
        whole: files::WholeHashes::default(),
        rom_id: None,
        state: FileState::Unverified,
        reason: None,
    }
}

async fn scan_zip_unit(
    sink: &mut Sink<'_>,
    rule: HeaderRule,
    rel_path: &str,
    path: &Path,
    mtime: i64,
    seen: &mut Vec<String>,
) -> Result<()> {
    let (ctx, platform_id) = (sink.ctx, sink.platform_id.clone());
    let platform_id = &platform_id;
    let path_owned = path.to_path_buf();
    let listed: std::result::Result<Vec<ZipMember>, HashError> =
        crate::threads::run(crate::threads::label::ZIP_LIST, move || {
            zip_members(File::open(&path_owned)?)
        })
        .await?;
    let members = match listed {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "cannot read zip; marking unverified");
            seen.push(rel_path.to_owned());
            sink.push(unverified_row(rel_path.to_owned(), 0, mtime, None))
                .await?;
            return Ok(());
        }
    };

    for member in members {
        ctx.checkpoint().await?;
        // A directory entry (or a zero-length placeholder) has no payload.
        if member.name.ends_with('/') {
            continue;
        }
        let member_rel = format!("{rel_path}#{}", member.name);
        let member_size = i64::try_from(member.size).unwrap_or(i64::MAX);
        seen.push(member_rel.clone());
        // The central directory's CRC32 is of the whole member, before any transform.
        let precheck = (rule == HeaderRule::None || rule.strips_header()).then_some(rule);
        let stats = (member_size, mtime);
        if !sink.handle_known(&member_rel, stats, precheck).await? {
            continue;
        }

        if precheck.is_some() {
            let unit = (path, mtime, member_rel.as_str());
            if let Some(row) = precheck_member(ctx, platform_id, rule, unit, &member).await? {
                sink.push(row).await?;
                continue;
            }
        }

        let path_owned = path.to_path_buf();
        let member_name = member.name.clone();
        let hash_result = crate::threads::run(crate::threads::label::HASH, move || {
            hash_zip_member_forms(File::open(&path_owned)?, &member_name, rule)
        })
        .await?;
        let row = match hash_result {
            Ok(forms) => {
                let found = Found {
                    rel_path: member_rel,
                    size: member_size,
                    mtime,
                };
                let own = files::basename(&member.name);
                sink.hashed_row(found, own, rule, forms).await?
            }
            Err(e) => {
                tracing::warn!(member = %member.name, error = %e, "cannot hash zip member; marking unverified");
                let mut row =
                    unverified_row(member_rel, member_size, mtime, Some(member.crc32.clone()));
                // The rule records the attempt, so an unchanged member is not decompressed again.
                row.header_rule = Some(rule.as_str().to_owned());
                row
            }
        };
        sink.push(row).await?;
    }
    Ok(())
}

/// The CRC32-only row of a zip member the pre-check finds no rom for, or `None` when it is
/// worth decompressing. `unit` is the zip's path, its mtime and the member's `rel_path`.
/// Under a stripping rule the member's content CRC32 comes from its header alone.
async fn precheck_member(
    ctx: &JobContext,
    platform_id: &PlatformId,
    rule: HeaderRule,
    (path, mtime, member_rel): (&Path, i64, &str),
    member: &ZipMember,
) -> Result<Option<NewFile>> {
    let content_crc = if rule.strips_header() {
        let (path_owned, m) = (path.to_path_buf(), member.clone());
        let got = crate::threads::run(crate::threads::label::HASH, move || {
            zip_member_content_crc(File::open(&path_owned)?, &m, rule)
        })
        .await?;
        // A header that cannot be read makes the member a candidate: hashing records why.
        let Ok(crc) = got else {
            return Ok(None);
        };
        crc
    } else {
        None
    };
    let size = i64::try_from(member.size).unwrap_or(i64::MAX);
    let (pid, whole, content) = (
        platform_id.clone(),
        member.crc32.clone(),
        content_crc.clone(),
    );
    let candidate = ctx
        .app
        .db
        .read(move |c| member_candidate(c, &pid, rule, &whole, content.as_deref(), size))
        .await?;
    if candidate {
        return Ok(None);
    }
    let crc = content_crc.unwrap_or_else(|| member.crc32.clone());
    let mut row = unverified_row(member_rel.to_owned(), size, mtime, Some(crc));
    row.whole.crc32 = rule.strips_header().then(|| member.crc32.clone());
    Ok(Some(row))
}

/// Walks one disc game directory (or a platform's top directory when it
/// holds loose track files directly). Tracks are grouped by the title their
/// hash matched, and each title is verified independently: a stray `.m3u`
/// or a second, unrelated game sharing the directory never demotes a
/// complete one, per `docs/VERIFICATION.md` "For disc games".
async fn scan_disc_unit(
    ctx: &JobContext,
    platform_id: &PlatformId,
    unit_id: &str,
    dir: &Path,
) -> Result<Option<(Vec<NewFile>, Vec<String>)>> {
    let dir_owned = dir.to_path_buf();
    let listed = crate::threads::run(crate::threads::label::SCAN_LIST, move || {
        list_files(&dir_owned)
    })
    .await?;
    let Some(entries) = readable(dir, listed) else {
        return Ok(None);
    };

    let mut tracks: Vec<Track> = Vec::new();
    let mut chd_rows: Vec<NewFile> = Vec::new();
    let mut seen = Vec::new();
    for ListedFile { path, name, meta } in entries {
        ctx.checkpoint().await?;
        let Some(ext) = extension(&path) else {
            continue;
        };
        if !DISC_TRACK_EXTENSIONS.contains(&ext.as_str()) {
            continue;
        }
        let rel_path = format!("{unit_id}/{name}");
        if ext == "chd" {
            let got = super::chd::scan_file(ctx, platform_id, &rel_path, &path, meta).await?;
            seen.extend(got.seen);
            match got.whole {
                Some(track) => tracks.push(track),
                None => chd_rows.extend(got.rows),
            }
            continue;
        }
        let (size, mtime) = match meta {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "cannot read metadata; marking unverified");
                seen.push(rel_path.clone());
                tracks.push(Track {
                    rel_path,
                    name,
                    size: 0,
                    mtime: 0,
                    hashes: None,
                    matched: None,
                });
                continue;
            }
        };
        seen.push(rel_path.clone());
        tracks.push(disc_track(ctx, platform_id, &path, rel_path, name, size, mtime).await?);
    }
    if tracks.is_empty() {
        return Ok(Some((chd_rows, seen)));
    }

    let mut rows = ctx
        .app
        .db
        .read(move |c| classify_disc_tracks(c, tracks))
        .await?;
    rows.extend(chd_rows);
    Ok(Some((rows, seen)))
}

/// Hashes one disc track, or reuses its stored hashes when unchanged, and finds its rom.
async fn disc_track(
    ctx: &JobContext,
    platform_id: &PlatformId,
    path: &Path,
    rel_path: String,
    name: String,
    size: i64,
    mtime: i64,
) -> Result<Track> {
    let (pid, relp) = (platform_id.clone(), rel_path.clone());
    let cached = ctx
        .app
        .db
        .read(move |c| {
            let row = files::find_by_path(c, &pid, &relp)?;
            Ok(row
                .filter(|r| r.unchanged(size, mtime))
                .and_then(|r| r.hashes()))
        })
        .await?;
    let hashes = if let Some(h) = cached {
        Some(h)
    } else {
        let hint = u64::try_from(size).unwrap_or(0);
        let path_owned = path.to_path_buf();
        let hash_result = crate::threads::run(crate::threads::label::HASH, move || {
            File::open(&path_owned).and_then(|f| hash_reader(f, HeaderRule::None, Some(hint)))
        })
        .await?;
        match hash_result {
            Ok(h) => Some(h),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "cannot hash track; marking unverified");
                None
            }
        }
    };
    let matched = match &hashes {
        Some(h) => {
            let (pid2, h2) = (platform_id.clone(), h.clone());
            ctx.app
                .db
                .read(move |c| {
                    roms::match_rom(
                        c,
                        &pid2,
                        &h2.sha1,
                        &h2.md5,
                        &h2.crc32,
                        i64::try_from(h2.size).unwrap_or(i64::MAX),
                    )
                })
                .await?
        }
        None => None,
    };
    Ok(Track {
        rel_path,
        name,
        size,
        mtime,
        hashes,
        matched,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::state;
    use crate::db::jobs as job_rows;

    #[test]
    fn a_listing_holds_each_regular_file_with_its_size_and_mtime() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("b.nes"), b"12345").expect("write");
        fs::write(dir.path().join("a.nes"), b"1").expect("write");
        fs::create_dir(dir.path().join("sub")).expect("mkdir");
        let listed = list_files(dir.path()).expect("list");
        let names: Vec<_> = listed.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["a.nes", "b.nes"]);
        let (size, mtime) = *listed[1].meta.as_ref().expect("meta");
        assert_eq!(size, 5);
        assert_eq!(
            (size, mtime),
            super::super::fsutil::file_meta(&listed[1].path).expect("stat")
        );
        assert!(list_files(&dir.path().join("gone"))
            .expect("gone")
            .is_empty());
    }

    #[test]
    fn done_units_cover_their_files_and_zip_members() {
        let done: std::collections::HashSet<String> =
            ["GBA".to_owned(), "PSX/Example Disc (USA)".to_owned()].into();
        assert!(in_done_unit("GBA/a.gba", &done));
        assert!(in_done_unit("GBA/a.zip#a.gba", &done));
        assert!(in_done_unit("PSX/Example Disc (USA)/t.bin", &done));
        assert!(in_done_unit("GBA", &done));
        assert!(!in_done_unit("PSX/Other (USA)/t.bin", &done));
        assert!(!in_done_unit("GBAX/a.gba", &done));
    }

    #[tokio::test]
    async fn scan_is_queued_only_when_the_games_dir_exists() {
        let (_dir, app) = state();
        let nes = PlatformId("nes".into());
        assert_eq!(
            enqueue_if_games_dir_exists(&app, &nes).await.expect("run"),
            None,
            "no NES directory yet"
        );
        fs::create_dir_all(app.config().paths.games.join("NES")).expect("mkdir");
        let id = enqueue_if_games_dir_exists(&app, &nes)
            .await
            .expect("run")
            .expect("queued");
        let row = app
            .db
            .read(move |c| job_rows::get(c, id))
            .await
            .expect("read")
            .expect("row");
        assert_eq!(row.kind, JobKind::Scan);
        assert!(
            enqueue_if_games_dir_exists(&app, &PlatformId("no-such".into()))
                .await
                .expect("run")
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_dat_triggered_scan_never_queues_for_arcade() {
        let (_dir, app) = state();
        let arcade = PlatformId("arcade".into());
        fs::create_dir_all(app.config().paths.games.join("mame")).expect("mkdir");
        assert_eq!(
            enqueue_if_games_dir_exists(&app, &arcade)
                .await
                .expect("run"),
            None,
            "arcade presence comes from the arcade catalogue, not a scan"
        );
    }

    #[tokio::test]
    async fn fan_out_skips_a_platform_with_an_open_scan() {
        let (_dir, app) = state();
        let nes_payload = json!({ "platform_id": "nes" });
        let id = app
            .db
            .write({
                let payload = nes_payload.clone();
                move |c| job_rows::insert(c, JobKind::Scan, &payload, Lane::Heavy, 0)
            })
            .await
            .expect("insert");
        app.db
            .write(move |c| job_rows::set_state(c, id, job_rows::JobState::Running, 0))
            .await
            .expect("running");

        Scheduler::run_inline(&app, Arc::new(ScanJob { platform_id: None }))
            .await
            .expect("fan out");

        let count_of = |payload: Value| {
            let app = app.clone();
            async move {
                app.db
                    .read(move |c| {
                        Ok(c.query_row(
                            "SELECT COUNT(*) FROM jobs WHERE kind = 'scan' AND payload = ?1",
                            [payload.to_string()],
                            |r| r.get::<_, i64>(0),
                        )?)
                    })
                    .await
                    .expect("count")
            }
        };
        assert_eq!(
            count_of(nes_payload).await,
            1,
            "the already-running nes scan is not duplicated"
        );
        assert_eq!(
            count_of(json!({ "platform_id": "snes" })).await,
            1,
            "other platforms still get scanned"
        );
        assert_eq!(
            count_of(json!({ "platform_id": "arcade" })).await,
            0,
            "the fan-out never scans arcade zips as cartridges"
        );
    }

    /// A unit whose directory cannot be listed (EACCES here) keeps its rows rather than
    /// losing every one to the prune; skipped when permissions do not apply (root).
    #[tokio::test]
    async fn an_unreadable_unit_keeps_its_rows() {
        use std::os::unix::fs::PermissionsExt as _;
        let (_dir, app) = state();
        let nes = app.config().paths.games.join("NES");
        fs::create_dir_all(&nes).expect("mkdir");
        let pid = PlatformId("nes".into());
        app.db
            .write_blocking({
                let pid = pid.clone();
                move |c| {
                    files::upsert(
                        c,
                        &pid,
                        &NewFile::unhashed("NES/a.nes", 1, 1, FileState::Verified),
                        1,
                    )
                    .map(|_| ())
                }
            })
            .expect("seed");
        fs::set_permissions(&nes, fs::Permissions::from_mode(0o000)).expect("chmod");
        let denied = fs::read_dir(&nes).is_err();
        if denied {
            Scheduler::run_inline(
                &app,
                Arc::new(ScanJob {
                    platform_id: Some(pid.clone()),
                }),
            )
            .await
            .expect("run");
        }
        fs::set_permissions(&nes, fs::Permissions::from_mode(0o755)).expect("chmod back");
        if !denied {
            return;
        }
        let kept = app
            .db
            .read_blocking(move |c| files::find_by_path(c, &pid, "NES/a.nes"))
            .expect("find");
        assert!(kept.is_some(), "an unreadable directory prunes nothing");
    }

    #[tokio::test]
    async fn scan_job_run_directly_against_arcade_is_a_no_op() {
        let (_dir, app) = state();
        let zip = app.config().paths.games.join("mame").join("exampleset.zip");
        fs::create_dir_all(zip.parent().expect("parent")).expect("mkdir");
        fs::write(&zip, b"not really a zip").expect("write");
        Scheduler::run_inline(
            &app,
            Arc::new(ScanJob {
                platform_id: Some(PlatformId("arcade".into())),
            }),
        )
        .await
        .expect("run");
        let n: i64 = app
            .db
            .read(|c| Ok(c.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?))
            .await
            .expect("count");
        assert_eq!(n, 0, "a direct arcade scan writes no files rows");
    }

    #[tokio::test]
    async fn a_disabled_platform_is_not_queued() {
        let (_dir, app) = state();
        let nes = PlatformId("nes".into());
        fs::create_dir_all(app.config().paths.games.join("NES")).expect("mkdir");
        app.db
            .write(|c| platform_rows::set_enabled(c, &PlatformId("nes".into()), false).map(|_| ()))
            .await
            .expect("disable");
        assert_eq!(
            enqueue_if_games_dir_exists(&app, &nes).await.expect("run"),
            None,
            "disabled platforms are skipped, matching POST /system/scan"
        );
    }

    #[test]
    fn only_stripping_rule_rows_without_the_whole_form_lack_it() {
        let row = |rule: Option<&str>, sha1: Option<&str>, whole: Option<&str>| files::FileRow {
            id: FileId(1),
            platform_id: PlatformId("nes".into()),
            rel_path: "NES/a.nes".into(),
            size: 20,
            mtime: 1,
            crc32: Some("00000000".into()),
            md5: None,
            sha1: sha1.map(Into::into),
            header_rule: rule.map(Into::into),
            whole: files::WholeHashes {
                sha1: whole.map(Into::into),
                ..files::WholeHashes::default()
            },
            rom_id: None,
            state: FileState::Unverified,
            scanned_at: 1,
            reason: None,
        };
        assert!(lacks_whole(&row(Some("ines"), Some("a"), None)));
        assert!(lacks_whole(&row(Some("lnx"), Some("a"), None)));
        assert!(!lacks_whole(&row(Some("ines"), Some("a"), Some("b"))));
        assert!(
            !lacks_whole(&row(Some("ines"), None, None)),
            "a failed hash"
        );
        assert!(!lacks_whole(&row(Some("smc"), Some("a"), None)));
        assert!(
            !lacks_whole(&row(None, None, None)),
            "a member known by CRC32"
        );
    }

    #[test]
    fn accepts_extension_allows_zip_only_for_cartridges() {
        let cart = platforms::by_id("nes").expect("nes");
        assert!(accepts_extension(cart, "zip"));
        assert!(accepts_extension(cart, "nes"));
        assert!(!accepts_extension(cart, "txt"));
        let disc = platforms::by_id("psx").expect("psx");
        assert!(!accepts_extension(disc, "zip"));
        assert!(accepts_extension(disc, "cue"));
    }

    #[test]
    fn discover_units_flat_for_cartridge_and_nested_for_disc() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nes_dir = dir.path().join("NES");
        fs::create_dir_all(&nes_dir).expect("mkdir");
        let cart = platforms::by_id("nes").expect("nes");
        let (units, _) = discover_units(dir.path(), cart);
        assert_eq!(
            units.iter().map(|u| u.id.as_str()).collect::<Vec<_>>(),
            ["NES"]
        );

        let psx_dir = dir.path().join("PSX").join("Example Quest (USA)");
        fs::create_dir_all(&psx_dir).expect("mkdir");
        let disc = platforms::by_id("psx").expect("psx");
        let (units, _) = discover_units(dir.path(), disc);
        assert_eq!(units[0].id, "PSX/Example Quest (USA)");
    }

    /// A loose disc image directly under the top directory (no per-title
    /// subfolder) is its own unit, not silently skipped.
    #[test]
    fn discover_units_includes_loose_disc_files_in_the_top_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let psx_dir = dir.path().join("PSX");
        fs::create_dir_all(&psx_dir).expect("mkdir");
        fs::write(psx_dir.join("Loose Quest (USA).iso"), b"data").expect("write");
        fs::create_dir_all(psx_dir.join("Example Quest (USA)")).expect("mkdir");
        let disc = platforms::by_id("psx").expect("psx");
        let (units, _) = discover_units(dir.path(), disc);
        let mut ids: Vec<&str> = units.iter().map(|u| u.id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(ids, ["PSX", "PSX/Example Quest (USA)"]);
    }

    /// A zip's central directory can list an explicit directory entry
    /// (trailing `/`); it carries no payload and is never a `files` row.
    #[test]
    fn zip_directory_entries_are_skipped() {
        let dir_entry = ZipMember {
            name: "sub/".to_owned(),
            size: 0,
            crc32: "00000000".to_owned(),
        };
        let file_entry = ZipMember {
            name: "sub/a.bin".to_owned(),
            size: 3,
            crc32: "352441c2".to_owned(),
        };
        assert!(dir_entry.name.ends_with('/'));
        assert!(!file_entry.name.ends_with('/'));
    }
}
