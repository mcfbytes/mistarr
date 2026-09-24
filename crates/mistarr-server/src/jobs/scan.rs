//! Library scan: walks each platform's `games/` directories, hashes changed
//! files and matches them against loaded DATs. See `docs/ARCHITECTURE.md`
//! "Library scan" and `docs/VERIFICATION.md` "Hashing" and "Matching order".

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use mistarr_core::hash::{hash_reader, hash_zip_member, zip_members, HeaderRule};
use mistarr_core::{HashSet as Hashes, PlatformId};
use mistarr_mister::platforms::{self, Kind, Platform};
use rusqlite::Connection;
use serde_json::{json, Value};
use tokio::time::Instant;

use super::{Job, JobContext, Lane};
use crate::db::files::{self, FileId, FileState, NewFile};
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

/// Enqueues one [`ScanJob`] per enabled platform.
async fn fan_out(ctx: &JobContext) -> Result<()> {
    let rows = ctx.app.db.read(platform_rows::list).await?;
    for row in rows.into_iter().filter(|r| r.enabled) {
        super::Scheduler::enqueue(
            &ctx.app,
            std::sync::Arc::new(ScanJob {
                platform_id: Some(row.id),
            }),
        )
        .await?;
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

/// One directory to walk: its stored id (also the `files.rel_path` prefix)
/// and its path on disk.
struct Unit {
    id: String,
    path: PathBuf,
}

/// The top-level and legacy directories of a platform, each a [`Unit`] for a
/// cartridge, romset or arcade platform; the per-title subdirectories of
/// those for a disc platform. Only directories that exist are returned.
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
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    let sub = entry.file_name().to_string_lossy().into_owned();
                    units.push(Unit {
                        id: format!("{name}/{sub}"),
                        path: entry.path(),
                    });
                }
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
        done_set.insert(unit.id.clone());
        let done_vec: Vec<String> = done_set.iter().cloned().collect();
        let now = crate::unix_now();
        let (pid2, unit_id, unit_path) = (pid.clone(), unit.id.clone(), unit.path.clone());
        let (written, seen) = ctx
            .app
            .db
            .write(move |c| commit_unit(c, platform, &pid2, &unit_id, &unit_path, &done_vec, now))
            .await?;
        keep.extend(seen);
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

/// A written file's identity, and every `rel_path` a directory pass considered
/// (written or skipped as unchanged), for the caller's `file.changed` events
/// and keep-list.
type CommitResult = (Vec<(String, FileId, FileState)>, Vec<String>);

/// Runs one directory's scan and its DB write in one transaction, so an
/// interruption never leaves a directory half committed.
#[allow(clippy::too_many_lines)] // One straight-line pass: list, hash, match, write.
fn commit_unit(
    conn: &mut Connection,
    platform: &'static Platform,
    platform_id: &PlatformId,
    unit_id: &str,
    unit_path: &Path,
    done_dirs: &[String],
    now: i64,
) -> Result<CommitResult> {
    let tx = conn.transaction()?;
    let mut rows: Vec<NewFile> = Vec::new();
    // Every accepted rel_path this pass considered, changed or not, so an
    // unchanged (skipped) file is kept rather than swept up as missing.
    let mut seen: Vec<String> = Vec::new();

    if platform.kind == Kind::Disc {
        scan_disc_dir(&tx, platform_id, unit_id, unit_path, &mut rows, &mut seen)?;
    } else {
        scan_flat_dir(
            &tx,
            platform,
            platform_id,
            unit_id,
            unit_path,
            &mut rows,
            &mut seen,
        )?;
    }

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
    Ok((written, seen))
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

fn scan_flat_dir(
    tx: &Connection,
    platform: &Platform,
    platform_id: &PlatformId,
    unit_id: &str,
    dir: &Path,
    rows: &mut Vec<NewFile>,
    seen: &mut Vec<String>,
) -> Result<()> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(());
    };
    let rule = header_rule(platform.header_rule);
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(ext) = extension(&path) else {
            continue;
        };
        if !accepts_extension(platform, &ext) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel_path = format!("{unit_id}/{name}");
        let (size, mtime) = file_meta(&path)?;
        if ext == "zip" {
            // A zip container has no files row of its own; its members do.
            scan_zip(
                tx,
                platform_id,
                rule,
                platform.header_rule,
                &rel_path,
                &path,
                mtime,
                rows,
                seen,
            )?;
            continue;
        }
        seen.push(rel_path.clone());
        if unchanged(tx, platform_id, &rel_path, size, mtime)? {
            continue;
        }
        let hint = u64::try_from(size).unwrap_or(0);
        let hashes = hash_reader(File::open(&path)?, rule, Some(hint))?;
        let (rom_id, state) = classify(tx, platform_id, &name, &hashes)?;
        rows.push(NewFile {
            rel_path,
            size,
            mtime,
            crc32: Some(hashes.crc32),
            md5: Some(hashes.md5),
            sha1: Some(hashes.sha1),
            header_rule: Some(platform.header_rule.to_owned()),
            rom_id,
            state,
        });
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn scan_zip(
    tx: &Connection,
    platform_id: &PlatformId,
    rule: HeaderRule,
    rule_name: &str,
    rel_path: &str,
    path: &Path,
    mtime: i64,
    rows: &mut Vec<NewFile>,
    seen: &mut Vec<String>,
) -> Result<()> {
    let members = zip_members(File::open(path)?).map_err(|e| Error::Job(e.to_string()))?;
    for member in members {
        let member_rel = format!("{rel_path}#{}", member.name);
        let member_size = i64::try_from(member.size).unwrap_or(i64::MAX);
        seen.push(member_rel.clone());
        if unchanged(tx, platform_id, &member_rel, member_size, mtime)? {
            continue;
        }
        let basename = files::basename(&member.name).to_owned();
        if files::crc_candidate_exists(tx, platform_id, &member.crc32, member_size)? {
            let hashes = hash_zip_member(File::open(path)?, &member.name, rule)
                .map_err(|e| Error::Job(e.to_string()))?;
            let (rom_id, state) = classify(tx, platform_id, &basename, &hashes)?;
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
        } else {
            rows.push(NewFile {
                rel_path: member_rel,
                size: member_size,
                mtime,
                crc32: Some(member.crc32),
                md5: None,
                sha1: None,
                header_rule: None,
                rom_id: None,
                state: FileState::Unverified,
            });
        }
    }
    Ok(())
}

/// One hashed track of a disc game directory, before the all-or-nothing rule
/// decides its final state.
struct Track {
    rel_path: String,
    name: String,
    size: i64,
    mtime: i64,
    hashes: Hashes,
    matched: Option<files::RomMatch>,
}

fn scan_disc_dir(
    tx: &Connection,
    platform_id: &PlatformId,
    unit_id: &str,
    dir: &Path,
    rows: &mut Vec<NewFile>,
    seen: &mut Vec<String>,
) -> Result<()> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(());
    };
    let mut tracks: Vec<Track> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel_path = format!("{unit_id}/{name}");
        seen.push(rel_path.clone());
        let (size, mtime) = file_meta(&path)?;
        let hint = u64::try_from(size).unwrap_or(0);
        let hashes = hash_reader(File::open(&path)?, HeaderRule::None, Some(hint))?;
        let matched = files::match_rom(
            tx,
            platform_id,
            &hashes.sha1,
            &hashes.md5,
            &hashes.crc32,
            i64::try_from(hashes.size).unwrap_or(i64::MAX),
        )?;
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
        return Ok(());
    }
    let title_id = tracks
        .first()
        .and_then(|t| t.matched.as_ref())
        .map(|m| m.title_id);
    let complete = title_id.is_some_and(|tid| {
        tracks.iter().all(|t| {
            t.matched
                .as_ref()
                .is_some_and(|m| m.title_id == tid && m.status != "baddump")
        })
    }) && title_id.is_some_and(|tid| {
        files::count_roms_for_title(tx, tid)
            .is_ok_and(|n| n == i64::try_from(tracks.len()).unwrap_or(-1))
    });
    for t in tracks {
        let state = match &t.matched {
            Some(m) if m.status == "baddump" => FileState::Bad,
            Some(_) if !complete => FileState::Unverified,
            Some(m) if files::basename(&m.name) == t.name => FileState::Verified,
            Some(_) => FileState::Misnamed,
            None => FileState::Unverified,
        };
        rows.push(NewFile {
            rel_path: t.rel_path,
            size: t.size,
            mtime: t.mtime,
            crc32: Some(t.hashes.crc32),
            md5: Some(t.hashes.md5),
            sha1: Some(t.hashes.sha1),
            header_rule: Some("none".to_owned()),
            rom_id: t.matched.map(|m| m.rom_id),
            state,
        });
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
