//! Library scan: walks each platform's `games/` directories, hashes changed
//! files and matches them against loaded DATs. See `docs/ARCHITECTURE.md`
//! "Library scan" and `docs/VERIFICATION.md` "Hashing" and "Matching order".

use std::collections::HashMap;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mistarr_core::hash::{
    hash_reader, hash_zip_member, zip_members, HashError, HeaderRule, ZipMember,
};
use mistarr_core::{HashSet as Hashes, PlatformId};
use mistarr_mister::platforms::{self, Kind, Platform};
use rusqlite::Connection;
use serde_json::{json, Value};
use tokio::time::Instant;

use super::{Job, JobContext, Lane, Scheduler};
use crate::app::AppState;
use crate::db::files::{self, FileId, FileState, NewFile};
use crate::db::jobs::JobId;
use crate::db::platforms as platform_rows;
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
    fn kind(&self) -> &'static str {
        "scan"
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
    let games_root = app.config().paths.games;
    let present = std::iter::once(platform.core_dir)
        .chain(platform.legacy_dirs.iter().copied())
        .any(|name| games_root.join(name).is_dir());
    if !present {
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

/// Enqueues one [`ScanJob`] per enabled platform.
async fn fan_out(ctx: &JobContext) -> Result<()> {
    let rows = ctx.app.db.read(platform_rows::list).await?;
    for row in rows.into_iter().filter(|r| r.enabled) {
        let job = ScanJob {
            platform_id: Some(row.id),
        };
        let payload = job.payload();
        // A scan of this platform already queued or running (e.g. from the
        // automatic per-platform trigger) does not need a second one.
        let already_open = ctx
            .app
            .db
            .read(move |c| crate::db::jobs::find_open(c, "scan", &payload))
            .await?
            .is_some();
        if already_open {
            continue;
        }
        super::Scheduler::enqueue(&ctx.app, std::sync::Arc::new(job)).await?;
    }
    Ok(())
}

/// The header rule named in `docs/PLATFORMS.md` "Header rules".
fn header_rule(name: &str) -> HeaderRule {
    match name {
        "ines" => HeaderRule::Ines,
        "smc" => HeaderRule::Smc,
        "a78" => HeaderRule::A78,
        "lnx" => HeaderRule::Lnx,
        "n64" => HeaderRule::N64,
        _ => HeaderRule::None,
    }
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
/// returned.
fn discover_units(games_root: &Path, platform: &Platform) -> Vec<Unit> {
    let top_names = std::iter::once(platform.core_dir).chain(platform.legacy_dirs.iter().copied());
    let top_dirs: Vec<(String, PathBuf)> = top_names
        .map(|name| (name.to_owned(), games_root.join(name)))
        .filter(|(_, p)| p.is_dir())
        .collect();
    let mut units = if platform.kind == Kind::Disc {
        let mut units = Vec::new();
        for (name, dir) in &top_dirs {
            let Ok(entries) = fs::read_dir(dir) else {
                continue;
            };
            let mut has_loose_file = false;
            for entry in entries.flatten() {
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
    units
}

/// Throttles `file.changed` to at most 10 per second by dropping the rest;
/// every state is already durable in `files` regardless.
struct Throttle {
    last: Option<Instant>,
}

impl Throttle {
    const MIN_GAP: Duration = Duration::from_millis(100);

    fn new() -> Self {
        Self { last: None }
    }

    fn allow(&mut self) -> bool {
        let now = Instant::now();
        if self
            .last
            .is_some_and(|last| now.duration_since(last) < Self::MIN_GAP)
        {
            return false;
        }
        self.last = Some(now);
        true
    }
}

/// The paths every directory entry in a unit resolved to, sorted for a
/// deterministic scan order.
fn list_files(dir: &Path) -> Vec<(PathBuf, String)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(PathBuf, String)> = entries
        .flatten()
        .filter(|e| e.path().is_file())
        .map(|e| {
            let path = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            (path, name)
        })
        .collect();
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out
}

async fn scan_platform(ctx: &JobContext, id: &PlatformId) -> Result<()> {
    let platform = platforms::by_id(&id.0)
        .ok_or_else(|| Error::Job(format!("unknown platform `{}`", id.0)))?;
    let games_root = ctx.app.config().paths.games.clone();
    let pid = id.clone();
    let units = tokio::task::spawn_blocking(move || discover_units(&games_root, platform))
        .await
        .map_err(|e| Error::Task(e.to_string()))?;

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
            move |c| files::scan_progress(c, &pid)
        })
        .await?;

    let unit_ids: std::collections::HashSet<&str> = units.iter().map(|u| u.id.as_str()).collect();
    let mut done_set: std::collections::HashSet<String> = done
        .into_iter()
        .filter(|d| unit_ids.contains(d.as_str()))
        .collect();
    let mut keep: Vec<String> = existing
        .into_iter()
        .filter(|p| {
            done_set
                .iter()
                .any(|d| p == d || p.starts_with(&format!("{d}/")))
        })
        .collect();

    let mut throttle = Throttle::new();
    let total = units.len();
    let remaining: Vec<Unit> = units
        .into_iter()
        .filter(|u| !done_set.contains(&u.id))
        .collect();
    for unit in remaining {
        ctx.checkpoint().await?;
        let (rows, unit_seen) = if platform.kind == Kind::Disc {
            scan_disc_unit(ctx, &pid, &unit.id, &unit.path).await?
        } else {
            scan_flat_unit(ctx, platform, &pid, &unit.id, &unit.path).await?
        };
        keep.extend(unit_seen);
        done_set.insert(unit.id.clone());
        let done_vec: Vec<String> = done_set.iter().cloned().collect();
        let now = crate::unix_now();
        let (pid2, rows2, done_vec2) = (pid.clone(), rows, done_vec);
        let written = ctx
            .app
            .db
            .write(move |c| commit_unit(c, &pid2, rows2, &done_vec2, now))
            .await?;
        for (_, id, state) in &written {
            if throttle.allow() {
                ctx.app.events.publish(
                    EventKind::FileChanged,
                    &json!({ "file_id": id.0, "state": state.as_str() }),
                );
            }
        }
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
        .write(move |c| files::delete_missing(c, &pid3, &keep2))
        .await?;
    let pid4 = pid.clone();
    ctx.app
        .db
        .write(move |c| files::clear_scan_progress(c, &pid4))
        .await
}

fn file_meta(path: &Path) -> io::Result<(i64, i64)> {
    let meta = fs::metadata(path)?;
    let size = i64::try_from(meta.len()).unwrap_or(i64::MAX);
    let mtime = meta
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    Ok((size, mtime))
}

fn extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
}

fn accepts_extension(platform: &Platform, ext: &str) -> bool {
    platform.load_extensions.contains(&ext) || (platform.kind == Kind::Cartridge && ext == "zip")
}

/// A written file's identity, for the caller's `file.changed` events.
type Written = Vec<(String, FileId, FileState)>;

/// Writes one unit's already-hashed rows and its scan progress in one short
/// transaction, so an interruption never leaves a directory half committed
/// and the single writer connection is never held for the hashing itself.
fn commit_unit(
    conn: &mut Connection,
    platform_id: &PlatformId,
    rows: Vec<NewFile>,
    done_dirs: &[String],
    now: i64,
) -> Result<Written> {
    let tx = conn.transaction()?;
    let mut written = Vec::with_capacity(rows.len());
    for row in rows {
        let hashed = files::Hashed {
            crc32: row.crc32.as_deref(),
            md5: row.md5.as_deref(),
            sha1: row.sha1.as_deref(),
            header_rule: row.header_rule.as_deref(),
        };
        let id = files::upsert(
            &tx,
            platform_id,
            &row.rel_path,
            row.size,
            row.mtime,
            &hashed,
            row.rom_id,
            row.state,
            now,
        )?;
        written.push((row.rel_path, id, row.state));
    }
    files::save_scan_progress(&tx, platform_id, done_dirs, now)?;
    tx.commit()?;
    Ok(written)
}

/// Matches a fully hashed payload and decides its state, per
/// `docs/DATA-MODEL.md` "files.state".
fn classify(
    conn: &Connection,
    platform_id: &PlatformId,
    actual_name: &str,
    hashes: &Hashes,
) -> Result<(Option<i64>, FileState)> {
    let size = i64::try_from(hashes.size).unwrap_or(i64::MAX);
    let Some(m) = files::match_rom(
        conn,
        platform_id,
        &hashes.sha1,
        &hashes.md5,
        &hashes.crc32,
        size,
    )?
    else {
        return Ok((None, FileState::Unverified));
    };
    let state = if m.status == "baddump" {
        FileState::Bad
    } else if files::basename(&m.name) == actual_name {
        FileState::Verified
    } else {
        FileState::Misnamed
    };
    Ok((Some(m.rom_id), state))
}

fn unchanged(
    conn: &Connection,
    platform_id: &PlatformId,
    rel_path: &str,
    size: i64,
    mtime: i64,
) -> Result<bool> {
    Ok(
        files::find_by_path(conn, platform_id, rel_path)?.is_some_and(|row| {
            row.size == size && row.mtime == mtime && row.state != FileState::Pending
        }),
    )
}

/// Walks one cartridge, romset or arcade directory. Hashing runs outside
/// any database lock; only the read connection is touched, and briefly, one
/// file at a time. The caller commits the returned rows in one transaction.
async fn scan_flat_unit(
    ctx: &JobContext,
    platform: &'static Platform,
    platform_id: &PlatformId,
    unit_id: &str,
    dir: &Path,
) -> Result<(Vec<NewFile>, Vec<String>)> {
    let dir_owned = dir.to_path_buf();
    let entries = tokio::task::spawn_blocking(move || list_files(&dir_owned))
        .await
        .map_err(|e| Error::Task(e.to_string()))?;
    let rule = header_rule(platform.header_rule);
    let mut rows = Vec::new();
    let mut seen = Vec::new();
    for (path, name) in entries {
        ctx.checkpoint().await?;
        let Some(ext) = extension(&path) else {
            continue;
        };
        if !accepts_extension(platform, &ext) {
            continue;
        }
        let rel_path = format!("{unit_id}/{name}");
        let (size, mtime) = match file_meta(&path) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "cannot read metadata; marking unverified");
                seen.push(rel_path.clone());
                rows.push(unverified_row(rel_path, 0, 0, None));
                continue;
            }
        };
        if ext == "zip" {
            // A zip container has no files row of its own; its members do.
            scan_zip_unit(
                ctx,
                platform_id,
                rule,
                platform.header_rule,
                &rel_path,
                &path,
                mtime,
                &mut rows,
                &mut seen,
            )
            .await?;
            continue;
        }
        seen.push(rel_path.clone());
        let (pid, relp) = (platform_id.clone(), rel_path.clone());
        let skip = ctx
            .app
            .db
            .read(move |c| unchanged(c, &pid, &relp, size, mtime))
            .await?;
        if skip {
            continue;
        }
        let hint = u64::try_from(size).unwrap_or(0);
        let path_owned = path.clone();
        let hash_result = tokio::task::spawn_blocking(move || {
            File::open(&path_owned).and_then(|f| hash_reader(f, rule, Some(hint)))
        })
        .await
        .map_err(|e| Error::Task(e.to_string()))?;
        let row = match hash_result {
            Ok(hashes) => {
                let (pid2, name2, hashes2) = (platform_id.clone(), name.clone(), hashes.clone());
                let (rom_id, state) = ctx
                    .app
                    .db
                    .read(move |c| classify(c, &pid2, &name2, &hashes2))
                    .await?;
                NewFile {
                    rel_path,
                    size,
                    mtime,
                    crc32: Some(hashes.crc32),
                    md5: Some(hashes.md5),
                    sha1: Some(hashes.sha1),
                    header_rule: Some(platform.header_rule.to_owned()),
                    rom_id,
                    state,
                }
            }
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "cannot hash file; marking unverified");
                unverified_row(rel_path, size, mtime, None)
            }
        };
        rows.push(row);
    }
    Ok((rows, seen))
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
        rom_id: None,
        state: FileState::Unverified,
    }
}

#[allow(clippy::too_many_arguments)]
async fn scan_zip_unit(
    ctx: &JobContext,
    platform_id: &PlatformId,
    rule: HeaderRule,
    rule_name: &str,
    rel_path: &str,
    path: &Path,
    mtime: i64,
    rows: &mut Vec<NewFile>,
    seen: &mut Vec<String>,
) -> Result<()> {
    let path_owned = path.to_path_buf();
    let listed: std::result::Result<Vec<ZipMember>, HashError> =
        tokio::task::spawn_blocking(move || zip_members(File::open(&path_owned)?))
            .await
            .map_err(|e| Error::Task(e.to_string()))?;
    let members = match listed {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "cannot read zip; marking unverified");
            seen.push(rel_path.to_owned());
            rows.push(unverified_row(rel_path.to_owned(), 0, mtime, None));
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
        let (pid, mrel) = (platform_id.clone(), member_rel.clone());
        let skip = ctx
            .app
            .db
            .read(move |c| unchanged(c, &pid, &mrel, member_size, mtime))
            .await?;
        if skip {
            continue;
        }
        let basename = files::basename(&member.name).to_owned();

        // A header rule strips bytes before hashing, so the DAT's expected
        // CRC32/size never match the zip's raw member entry: always hash.
        let candidate = if rule == HeaderRule::None {
            let (pid2, crc, size2) = (platform_id.clone(), member.crc32.clone(), member_size);
            ctx.app
                .db
                .read(move |c| files::crc_candidate_exists(c, &pid2, &crc, size2))
                .await?
        } else {
            true
        };

        if !candidate {
            rows.push(unverified_row(
                member_rel,
                member_size,
                mtime,
                Some(member.crc32.clone()),
            ));
            continue;
        }

        let path_owned = path.to_path_buf();
        let member_name = member.name.clone();
        let hash_result = tokio::task::spawn_blocking(move || {
            hash_zip_member(File::open(&path_owned)?, &member_name, rule)
        })
        .await
        .map_err(|e| Error::Task(e.to_string()))?;
        match hash_result {
            Ok(hashes) => {
                let (pid2, basename2, hashes2) = (platform_id.clone(), basename, hashes.clone());
                let (rom_id, state) = ctx
                    .app
                    .db
                    .read(move |c| classify(c, &pid2, &basename2, &hashes2))
                    .await?;
                rows.push(NewFile {
                    rel_path: member_rel,
                    size: member_size,
                    mtime,
                    crc32: Some(hashes.crc32),
                    md5: Some(hashes.md5),
                    sha1: Some(hashes.sha1),
                    header_rule: Some(rule_name.to_owned()),
                    rom_id,
                    state,
                });
            }
            Err(e) => {
                tracing::warn!(member = %member.name, error = %e, "cannot hash zip member; marking unverified");
                rows.push(unverified_row(
                    member_rel,
                    member_size,
                    mtime,
                    Some(member.crc32.clone()),
                ));
            }
        }
    }
    Ok(())
}

/// One hashed track of a disc game directory, before the all-or-nothing rule
/// decides its final state. `hashes` is `None` when the track could not be
/// read; it is then always `unverified`.
struct Track {
    rel_path: String,
    name: String,
    size: i64,
    mtime: i64,
    hashes: Option<Hashes>,
    matched: Option<files::RomMatch>,
}

/// The stored hashes of an unchanged track, reused instead of re-hashing.
fn cached_hashes(
    conn: &Connection,
    platform_id: &PlatformId,
    rel_path: &str,
    size: i64,
    mtime: i64,
) -> Result<Option<Hashes>> {
    let Some(row) = files::find_by_path(conn, platform_id, rel_path)? else {
        return Ok(None);
    };
    if row.size != size || row.mtime != mtime || row.state == FileState::Pending {
        return Ok(None);
    }
    let (Some(crc32), Some(md5), Some(sha1)) = (row.crc32, row.md5, row.sha1) else {
        return Ok(None);
    };
    Ok(Some(Hashes {
        size: u64::try_from(size).unwrap_or(0),
        crc32,
        md5,
        sha1,
    }))
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
) -> Result<(Vec<NewFile>, Vec<String>)> {
    let dir_owned = dir.to_path_buf();
    let entries = tokio::task::spawn_blocking(move || list_files(&dir_owned))
        .await
        .map_err(|e| Error::Task(e.to_string()))?;

    let mut tracks: Vec<Track> = Vec::new();
    let mut seen = Vec::new();
    for (path, name) in entries {
        ctx.checkpoint().await?;
        let Some(ext) = extension(&path) else {
            continue;
        };
        if !DISC_TRACK_EXTENSIONS.contains(&ext.as_str()) {
            continue;
        }
        let rel_path = format!("{unit_id}/{name}");
        let (size, mtime) = match file_meta(&path) {
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

        let (pid, relp) = (platform_id.clone(), rel_path.clone());
        let cached = ctx
            .app
            .db
            .read(move |c| cached_hashes(c, &pid, &relp, size, mtime))
            .await?;
        let hashes = if let Some(h) = cached {
            Some(h)
        } else {
            let hint = u64::try_from(size).unwrap_or(0);
            let path_owned = path.clone();
            let hash_result = tokio::task::spawn_blocking(move || {
                File::open(&path_owned).and_then(|f| hash_reader(f, HeaderRule::None, Some(hint)))
            })
            .await
            .map_err(|e| Error::Task(e.to_string()))?;
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
                        files::match_rom(
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
        tracks.push(Track {
            rel_path,
            name,
            size,
            mtime,
            hashes,
            matched,
        });
    }
    if tracks.is_empty() {
        return Ok((Vec::new(), seen));
    }

    let rows = ctx
        .app
        .db
        .read(move |c| classify_disc_tracks(c, tracks))
        .await?;
    Ok((rows, seen))
}

/// Decides each track's final state from the all-or-nothing rule, evaluated
/// once per matched title rather than once for the whole directory.
fn classify_disc_tracks(conn: &Connection, tracks: Vec<Track>) -> Result<Vec<NewFile>> {
    let mut groups: HashMap<i64, Vec<usize>> = HashMap::new();
    for (i, t) in tracks.iter().enumerate() {
        if let Some(m) = &t.matched {
            groups.entry(m.title_id).or_default().push(i);
        }
    }
    let mut complete: HashMap<i64, bool> = HashMap::new();
    for (&title_id, idxs) in &groups {
        let want = files::count_roms_for_title(conn, title_id)?;
        let ok = i64::try_from(idxs.len()).unwrap_or(-1) == want
            && idxs.iter().all(|&i| {
                tracks[i]
                    .matched
                    .as_ref()
                    .is_some_and(|m| m.status != "baddump")
            });
        complete.insert(title_id, ok);
    }

    let mut rows = Vec::with_capacity(tracks.len());
    for t in tracks {
        let (rom_id, state) = match &t.matched {
            None => (None, FileState::Unverified),
            Some(m) if m.status == "baddump" => (Some(m.rom_id), FileState::Bad),
            Some(m) => {
                let is_complete = complete.get(&m.title_id).copied().unwrap_or(false);
                let state = if !is_complete {
                    FileState::Unverified
                } else if files::basename(&m.name) == t.name {
                    FileState::Verified
                } else {
                    FileState::Misnamed
                };
                (Some(m.rom_id), state)
            }
        };
        let (crc32, md5, sha1, header_rule) = match t.hashes {
            Some(h) => (
                Some(h.crc32),
                Some(h.md5),
                Some(h.sha1),
                Some("none".to_owned()),
            ),
            None => (None, None, None, None),
        };
        rows.push(NewFile {
            rel_path: t.rel_path,
            size: t.size,
            mtime: t.mtime,
            crc32,
            md5,
            sha1,
            header_rule,
            rom_id,
            state,
        });
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::state;
    use crate::db::jobs as job_rows;

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
        assert_eq!(row.kind, "scan");
        assert!(
            enqueue_if_games_dir_exists(&app, &PlatformId("no-such".into()))
                .await
                .expect("run")
                .is_none()
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
                move |c| job_rows::insert(c, "scan", &payload, 0)
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
    }

    #[tokio::test]
    async fn a_disabled_platform_is_not_queued() {
        let (_dir, app) = state();
        let nes = PlatformId("nes".into());
        fs::create_dir_all(app.config().paths.games.join("NES")).expect("mkdir");
        app.db
            .write(|c| platform_rows::set_enabled(c, "nes", false).map(|_| ()))
            .await
            .expect("disable");
        assert_eq!(
            enqueue_if_games_dir_exists(&app, &nes).await.expect("run"),
            None,
            "disabled platforms are skipped, matching POST /system/scan"
        );
    }

    #[test]
    fn header_rule_names_map() {
        assert!(matches!(header_rule("ines"), HeaderRule::Ines));
        assert!(matches!(header_rule("smc"), HeaderRule::Smc));
        assert!(matches!(header_rule("nope"), HeaderRule::None));
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
        let units = discover_units(dir.path(), cart);
        assert_eq!(
            units.iter().map(|u| u.id.as_str()).collect::<Vec<_>>(),
            ["NES"]
        );

        let psx_dir = dir.path().join("PSX").join("Example Quest (USA)");
        fs::create_dir_all(&psx_dir).expect("mkdir");
        let disc = platforms::by_id("psx").expect("psx");
        let units = discover_units(dir.path(), disc);
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
        let units = discover_units(dir.path(), disc);
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
