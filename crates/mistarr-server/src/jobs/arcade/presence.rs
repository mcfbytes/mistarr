//! Arcade presence pass: which zips live MRAs name are on disk under `games/mame` and
//! `games/hbmame`; see `docs/ARCHITECTURE.md` "Arcade presence pass".

use std::cmp::Ordering;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mistarr_core::hash::{zip_members, HashError};
use mistarr_core::PlatformId;
use mistarr_mister::launch::split_zip_member;
use mistarr_mister::platforms::Platform;
use rusqlite::Connection;
use serde_json::json;

use crate::db::arcade as arcade_rows;
use crate::db::files::{self, FileId, FileRow, FileState, Hashed};
use crate::db::Db;
use crate::error::Result;
use crate::jobs::scan::{extension, file_meta};
use crate::jobs::JobContext;

/// Zips stated, looked up and written per batch; each batch is one write transaction.
const BATCH: usize = 500;

/// `files` rows read per page while pruning one directory.
const PRUNE_PAGE: usize = 2000;

/// What one run did, folded into the catalogue's own final progress event.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Stats {
    /// Zips seen under `games/mame` and `games/hbmame`.
    pub(super) zips: usize,
    /// Presence rows written or refreshed.
    pub(super) recorded: usize,
    /// `files` rows removed because their zip, member or naming MRA is gone.
    pub(super) pruned: usize,
}

/// One zip on disk, from one stat.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Zip {
    /// `{dir}/{name}`, as `files.rel_path` stores it.
    rel: String,
    /// Size in bytes.
    size: i64,
    /// Modification time, Unix seconds.
    mtime: i64,
}

/// Member rows of a zip that changed on disk, reconciled once its central directory is read.
#[derive(Debug)]
struct Recheck {
    /// The zip as stated this run.
    zip: Zip,
    /// The zip roms live MRAs give it, lowest first; empty when none names it.
    roms: Vec<i64>,
    /// Its `zip#member` rows.
    members: Vec<FileRow>,
}

/// One batch's writes, applied in one transaction.
#[derive(Debug, Default, PartialEq, Eq)]
struct Changes {
    /// Rows to delete.
    drop: Vec<String>,
    /// Presence rows to write: the zip, and the MRA zip rom it stands for.
    record: Vec<(Zip, i64)>,
    /// Member rows whose hashes still apply: the new mtime.
    restamp: Vec<(FileId, i64)>,
    /// Member rows whose content changed: new size, mtime and CRC32.
    reverify: Vec<(FileId, i64, i64, String)>,
}

/// `platform`'s zip directories under `games/`: its core directory and any legacy one.
fn zip_dirs(platform: &'static Platform) -> impl Iterator<Item = &'static str> {
    std::iter::once(platform.core_dir).chain(platform.legacy_dirs.iter().copied())
}

/// Compares two names as exFAT does, ignoring ASCII case.
fn cmp_nocase(a: &str, b: &str) -> Ordering {
    a.bytes()
        .map(|c| c.to_ascii_lowercase())
        .cmp(b.bytes().map(|c| c.to_ascii_lowercase()))
}

/// Zip file names directly under `dir`, sorted by [`cmp_nocase`]; empty when `dir` is
/// gone, an error when it exists but cannot be read.
fn zip_names(dir: &Path) -> io::Result<Vec<String>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out: Vec<String> = entries
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| extension(Path::new(n)).as_deref() == Some("zip"))
        .collect();
    out.sort_unstable_by(|a, b| cmp_nocase(a, b));
    Ok(out)
}

/// Stats zip `name` under `dir`; one that cannot be stated is logged and left out, and
/// so are its rows, which stay as they are.
fn stat(games: &Path, dir: &str, name: &str) -> Option<Zip> {
    let rel = format!("{dir}/{name}");
    match file_meta(&games.join(&rel)) {
        Ok((size, mtime)) => Some(Zip { rel, size, mtime }),
        Err(e) => {
            tracing::warn!(path = %rel, error = %e, "cannot stat zip; keeping its rows");
            None
        }
    }
}

/// The presence row `zip` needs when no member row stands for it: one against a zip rom
/// of a live MRA naming it, the lowest of `roms` when written. The row is left alone while
/// the zip's size and mtime and one of `roms` still match it, so a row `verify_siblings`
/// promoted under another MRA's rom stays promoted; it is removed when no MRA names the zip.
fn record(zip: &Zip, roms: &[i64], bare: Option<&FileRow>, out: &mut Changes) {
    let Some(&first) = roms.first() else {
        if let Some(b) = bare {
            out.drop.push(b.rel_path.clone());
        }
        return;
    };
    match bare {
        Some(b)
            if (b.size, b.mtime) == (zip.size, zip.mtime)
                && b.rom_id.is_some_and(|r| roms.contains(&r)) => {}
        // Rewritten at the row's own spelling, which may differ in case from the listing.
        Some(b) => out.record.push((
            Zip {
                rel: b.rel_path.clone(),
                ..zip.clone()
            },
            first,
        )),
        None => out.record.push((zip.clone(), first)),
    }
}

/// Decides `zip`'s rows without reading it. Member rows (the import path's) stand for the
/// zip and replace any presence row; they are left alone unless the zip's mtime moved.
fn decide(
    zip: &Zip,
    roms: &[i64],
    bare: Vec<FileRow>,
    members: Vec<FileRow>,
    out: &mut Changes,
) -> Option<Recheck> {
    let mut bare = bare.into_iter();
    let first = bare.next();
    // A second spelling of the same zip on a case-insensitive card is one row too many.
    out.drop.extend(bare.map(|b| b.rel_path));
    if members.is_empty() {
        record(zip, roms, first.as_ref(), out);
        return None;
    }
    if let Some(b) = first {
        out.drop.push(b.rel_path);
    }
    if members.iter().all(|m| m.mtime == zip.mtime) {
        return None;
    }
    Some(Recheck {
        zip: zip.clone(),
        roms: roms.to_vec(),
        members,
    })
}

/// Stats, looks up and decides each zip of `names` under `dir`, reading a central
/// directory only for a [`Recheck`]. The reader is held for one zip's lookup at a time,
/// never across a read from the SD card.
fn plan_batch(
    db: &Db,
    games: &Path,
    pid: &PlatformId,
    live: &HashMap<String, Vec<i64>>,
    dir: &str,
    names: &[String],
) -> Result<Changes> {
    let mut out = Changes::default();
    for name in names {
        let Some(zip) = stat(games, dir, name) else {
            continue;
        };
        let roms = live
            .get(&zip.rel.to_ascii_lowercase())
            .map_or(&[][..], Vec::as_slice);
        let (bare, members) = db.read_blocking(|c| files::zip_rows_nocase(c, pid, &zip.rel))?;
        if let Some(rc) = decide(&zip, roms, bare, members, &mut out) {
            recheck(games, rc, &mut out);
        }
    }
    Ok(out)
}

/// Reads a changed zip's central directory, never decompressing, and reconciles its member
/// rows: same size and CRC32 keeps the row's hashes and state, a different one marks it for
/// re-verification, a missing member drops it. An unreadable zip keeps every row as it is.
fn recheck(games: &Path, rc: Recheck, out: &mut Changes) {
    let listed = File::open(games.join(&rc.zip.rel))
        .map_err(HashError::from)
        .and_then(zip_members);
    let listed = match listed {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(path = %rc.zip.rel, error = %e, "cannot read zip; keeping its rows");
            return;
        }
    };
    let by_name: HashMap<&str, _> = listed.iter().map(|m| (m.name.as_str(), m)).collect();
    let mut kept = 0;
    for row in rc.members {
        let name = split_zip_member(&row.rel_path).1.unwrap_or_default();
        let Some(m) = by_name.get(name) else {
            out.drop.push(row.rel_path);
            continue;
        };
        kept += 1;
        let size = i64::try_from(m.size).unwrap_or(i64::MAX);
        let same_crc = row
            .crc32
            .as_deref()
            .is_some_and(|c| c.eq_ignore_ascii_case(&m.crc32));
        if row.size == size && same_crc {
            out.restamp.push((row.id, rc.zip.mtime));
        } else {
            out.reverify
                .push((row.id, size, rc.zip.mtime, m.crc32.clone()));
        }
    }
    if kept == 0 {
        record(&rc.zip, &rc.roms, None, out);
    }
}

/// Applies one batch's changes in one transaction; returns rows recorded and removed.
fn write_changes(
    conn: &mut Connection,
    pid: &PlatformId,
    changes: &Changes,
    now: i64,
) -> Result<(usize, usize)> {
    let tx = conn.transaction()?;
    let dropped = files::delete_paths(&tx, pid, &changes.drop)?;
    for (zip, rom) in &changes.record {
        let none = Hashed::default();
        let state = FileState::Unverified;
        files::upsert(
            &tx,
            pid,
            &zip.rel,
            zip.size,
            zip.mtime,
            &none,
            Some(*rom),
            state,
            now,
        )?;
    }
    for (id, mtime) in &changes.restamp {
        files::restamp(&tx, *id, *mtime, now)?;
    }
    for (id, size, mtime, crc) in &changes.reverify {
        files::reverify(&tx, *id, *size, *mtime, crc, now)?;
    }
    tx.commit()?;
    Ok((changes.record.len(), dropped))
}

/// Rows of `page` under `dir` whose zip is neither in `names` (sorted by [`cmp_nocase`])
/// nor on disk. A row whose zip cannot be checked is kept.
fn gone(games: &Path, dir: &str, names: &[String], page: Vec<String>) -> Vec<String> {
    page.into_iter()
        .filter(|rel| {
            let container = split_zip_member(rel).0;
            let listed = container
                .strip_prefix(dir)
                .and_then(|n| n.strip_prefix('/'))
                .is_some_and(|n| names.binary_search_by(|x| cmp_nocase(x, n)).is_ok());
            if listed {
                return false;
            }
            // Outside the listing: a nested path or a zip added since, checked on disk.
            match games.join(container).try_exists() {
                Ok(present) => !present,
                Err(e) => {
                    tracing::warn!(path = %container, error = %e, "cannot check zip; keeping its rows");
                    false
                }
            }
        })
        .collect()
}

/// Deletes the rows under `dir` whose zip is gone, a page at a time, holding the reader
/// only while a page is read.
async fn prune_dir(
    ctx: &JobContext,
    pid: &PlatformId,
    games: &Path,
    dir: &'static str,
    names: Arc<Vec<String>>,
) -> Result<usize> {
    let mut after = String::new();
    let mut pruned = 0;
    loop {
        ctx.checkpoint().await?;
        let (p, from) = (pid.clone(), after.clone());
        let page = ctx
            .app
            .db
            .read(move |c| files::paths_under(c, &p, dir, &from, PRUNE_PAGE))
            .await?;
        let Some(last) = page.last() else {
            break;
        };
        after.clone_from(last);
        let full = page.len() == PRUNE_PAGE;
        let (g, n) = (games.to_path_buf(), Arc::clone(&names));
        let doomed = super::blocking(move || gone(&g, dir, &n, page)).await?;
        if !doomed.is_empty() {
            let p = pid.clone();
            pruned += ctx
                .app
                .db
                .write(move |c| {
                    let tx = c.transaction()?;
                    let n = files::delete_paths(&tx, &p, &doomed)?;
                    tx.commit()?;
                    Ok(n)
                })
                .await?;
        }
        if !full {
            break;
        }
    }
    Ok(pruned)
}

/// Records a presence row for every zip under `games/mame` and `games/hbmame` that a live
/// MRA names and no import row stands for, reconciles import rows of zips that changed,
/// and prunes rows whose zip is gone. Runs after the catalogue commits its titles, so the
/// live MRA zips it reads once per run are this run's.
pub(super) async fn run(ctx: &JobContext) -> Result<Stats> {
    let games: PathBuf = ctx.app.config().paths.games.clone();
    let Some(platform) = mistarr_mister::platforms::by_id(super::PLATFORM) else {
        return Ok(Stats::default());
    };
    if !games.is_dir() {
        tracing::warn!(path = %games.display(), "games directory missing; presence pass skipped");
        return Ok(Stats::default());
    }
    ctx.checkpoint().await?;
    let live = Arc::new(
        ctx.app
            .db
            .read(|c| arcade_rows::live_zip_roms(c, super::PLATFORM))
            .await?,
    );
    let listed: Vec<(&'static str, io::Result<Vec<String>>)> = super::blocking({
        let games = games.clone();
        move || {
            zip_dirs(platform)
                .map(|d| (d, zip_names(&games.join(d))))
                .collect()
        }
    })
    .await?;
    let mut listing: Vec<(&'static str, Arc<Vec<String>>)> = Vec::new();
    for (dir, names) in listed {
        match names {
            Ok(names) => listing.push((dir, Arc::new(names))),
            // Nothing under a directory that cannot be listed is recorded or pruned this run.
            Err(e) => tracing::warn!(dir, error = %e, "cannot list zips; keeping their rows"),
        }
    }
    let pid = PlatformId(super::PLATFORM.to_owned());
    let mut stats = Stats {
        zips: listing.iter().map(|(_, n)| n.len()).sum(),
        ..Stats::default()
    };
    let mut done = 0;
    for (dir, names) in &listing {
        let dir: &'static str = dir;
        for batch in names.chunks(BATCH) {
            ctx.checkpoint().await?;
            let (db, g, p, l, b) = (
                ctx.app.db.clone(),
                games.clone(),
                pid.clone(),
                Arc::clone(&live),
                batch.to_vec(),
            );
            let changes = super::blocking(move || plan_batch(&db, &g, &p, &l, dir, &b)).await??;
            let (p, now) = (pid.clone(), crate::unix_now());
            let (recorded, dropped) = ctx
                .app
                .db
                .write(move |c| write_changes(c, &p, &changes, now))
                .await?;
            stats.recorded += recorded;
            stats.pruned += dropped;
            done += batch.len();
            ctx.progress(json!({ "presence_done": done, "presence_total": stats.zips }))
                .await?;
        }
        stats.pruned += prune_dir(ctx, &pid, &games, dir, Arc::clone(names)).await?;
    }
    Ok(stats)
}

#[cfg(test)]
mod tests;
