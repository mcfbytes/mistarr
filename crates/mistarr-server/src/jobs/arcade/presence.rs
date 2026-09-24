//! Arcade presence pass: which zips live MRAs name are on disk under `games/mame` and
//! `games/hbmame`; see `docs/ARCHITECTURE.md` "Arcade presence pass".

use std::collections::HashMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mistarr_core::hash::{zip_members, HashError};
use mistarr_core::PlatformId;
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
    /// The zip rom a live MRA gives it, if any.
    rom: Option<i64>,
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

/// Zip file names directly under `dir`, sorted bytewise as `files.rel_path` sorts.
fn zip_names(dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| extension(Path::new(n)).as_deref() == Some("zip"))
        .collect();
    out.sort_unstable();
    out
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

/// The presence row `zip` needs when no member row stands for it: one against the MRA
/// zip rom that names it, refreshed only when the zip or its rom changed; none otherwise.
fn record(zip: &Zip, rom: Option<i64>, bare: Option<&FileRow>, out: &mut Changes) {
    match (rom, bare) {
        (None, Some(b)) => out.drop.push(b.rel_path.clone()),
        (None, None) => {}
        (Some(r), Some(b)) if (b.size, b.mtime, b.rom_id) == (zip.size, zip.mtime, Some(r)) => {}
        (Some(r), _) => out.record.push((zip.clone(), r)),
    }
}

/// Decides `zip`'s rows without reading it. Member rows (the import path's) stand for the
/// zip and replace any presence row; they are left alone unless the zip's mtime moved.
fn decide(
    zip: &Zip,
    rom: Option<i64>,
    bare: Option<FileRow>,
    members: Vec<FileRow>,
    out: &mut Changes,
) -> Option<Recheck> {
    if members.is_empty() {
        record(zip, rom, bare.as_ref(), out);
        return None;
    }
    if let Some(b) = bare {
        out.drop.push(b.rel_path);
    }
    if members.iter().all(|m| m.mtime == zip.mtime) {
        return None;
    }
    Some(Recheck {
        zip: zip.clone(),
        rom,
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
    live: &HashMap<String, i64>,
    dir: &str,
    names: &[String],
) -> Result<Changes> {
    let mut out = Changes::default();
    for name in names {
        let Some(zip) = stat(games, dir, name) else {
            continue;
        };
        let rom = live.get(&zip.rel.to_ascii_lowercase()).copied();
        let (bare, members) = db.read_blocking(|c| {
            let bare = files::find_by_path(c, pid, &zip.rel)?;
            Ok((bare, files::zip_member_rows(c, pid, &zip.rel)?))
        })?;
        if let Some(rc) = decide(&zip, rom, bare, members, &mut out) {
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
        let name = row.rel_path.split_once('#').map_or("", |(_, n)| n);
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
        record(&rc.zip, rc.rom, None, out);
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

/// Rows of `page` under `dir` whose zip is not in `names` (sorted) and not on disk.
fn gone(games: &Path, dir: &str, names: &[String], page: Vec<String>) -> Vec<String> {
    page.into_iter()
        .filter(|rel| {
            let container = rel.split('#').next().unwrap_or(rel);
            let listed = container
                .strip_prefix(dir)
                .and_then(|n| n.strip_prefix('/'))
                .is_some_and(|n| names.binary_search_by(|x| x.as_str().cmp(n)).is_ok());
            // A row outside the listing (a nested path, or a zip added since) is checked on disk.
            !listed && !games.join(container).exists()
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
    let listing: Vec<(&'static str, Arc<Vec<String>>)> = super::blocking({
        let games = games.clone();
        move || {
            zip_dirs(platform)
                .map(|d| (d, Arc::new(zip_names(&games.join(d)))))
                .collect()
        }
    })
    .await?;
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
