//! Arcade presence: reads every `games/mame` and `games/hbmame` zip's central
//! directory (member names, sizes and CRC32, never decompressed) so DAT-sourced
//! arcade titles get CRC32-level verification and a zip a live MRA names gets a
//! row for `verify_siblings` to promote, without a full library scan ever
//! walking arcade. See `docs/ARCHITECTURE.md` "Arcade presence pass".

use std::fs::File;
use std::path::{Path, PathBuf};

use mistarr_core::hash::{zip_members, HashError};
use mistarr_core::{HashSet as Hashes, PlatformId};
use mistarr_mister::platforms::Platform;
use rusqlite::Connection;
use serde_json::json;

use crate::db::arcade as arcade_rows;
use crate::db::files::{self, FileState, Hashed, NewFile};
use crate::db::Db;
use crate::error::Result;
use crate::jobs::scan::{classify, extension, file_meta, list_files, unchanged, unverified_row};
use crate::jobs::JobContext;

/// Zips read, matched and written per batch, so memory follows the batch, not the tree.
const BATCH: usize = 500;

/// What one run found, folded into the catalogue's own final progress event.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Stats {
    /// Zips seen under `games/mame` and `games/hbmame`.
    pub(super) zips: usize,
    /// `files` rows removed because their zip or member is gone.
    pub(super) pruned: usize,
}

/// One zip found under a walked directory.
#[derive(Clone)]
struct ZipFile {
    /// `mame` or `hbmame`, also the `files.rel_path` and [`arcade_rows::zip_rom_id`] prefix.
    dir: String,
    /// The zip's own file name.
    name: String,
    /// `{dir}/{name}`.
    rel: String,
    /// Where it is on disk.
    path: PathBuf,
}

/// `platform`'s `games/` directories the presence pass walks: its core directory and
/// any legacy one, `mame` and `hbmame` for arcade.
fn zip_dirs(games: &Path, platform: &'static Platform) -> Vec<(String, PathBuf)> {
    std::iter::once(platform.core_dir)
        .chain(platform.legacy_dirs.iter().copied())
        .map(|name| (name.to_owned(), games.join(name)))
        .filter(|(_, p)| p.is_dir())
        .collect()
}

/// Every zip directly under `platform`'s walked directories, sorted for a
/// deterministic pass order.
fn list_zips(games: &Path, platform: &'static Platform) -> Vec<ZipFile> {
    let mut out = Vec::new();
    for (dir, path) in zip_dirs(games, platform) {
        for (file_path, name) in list_files(&path) {
            if extension(&file_path).as_deref() != Some("zip") {
                continue;
            }
            out.push(ZipFile {
                rel: format!("{dir}/{name}"),
                dir: dir.clone(),
                name,
                path: file_path,
            });
        }
    }
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    out
}

/// Reads `zf`'s central directory and decides each member's row: a DAT match by CRC32
/// and size, a zip a live MRA names but no DAT covers, or nothing worth tracking.
/// A zip that cannot be opened gets one bare-path row instead of its members, mirroring
/// a scan's own handling of an unreadable zip.
fn scan_zip(
    conn: &Connection,
    pid: &PlatformId,
    zf: &ZipFile,
    rows: &mut Vec<NewFile>,
    seen: &mut Vec<String>,
) -> Result<()> {
    let mtime = match file_meta(&zf.path) {
        Ok((_, mtime)) => mtime,
        Err(e) => {
            tracing::warn!(path = %zf.path.display(), error = %e, "cannot read metadata; marking unverified");
            seen.push(zf.rel.clone());
            rows.push(unverified_row(zf.rel.clone(), 0, 0, None));
            return Ok(());
        }
    };
    let opened = File::open(&zf.path)
        .map_err(HashError::from)
        .and_then(zip_members);
    let members = match opened {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(path = %zf.path.display(), error = %e, "cannot read zip; marking unverified");
            seen.push(zf.rel.clone());
            rows.push(unverified_row(zf.rel.clone(), 0, mtime, None));
            return Ok(());
        }
    };
    for member in members {
        // A directory entry (or a zero-length placeholder) has no payload.
        if member.name.ends_with('/') {
            continue;
        }
        let member_rel = format!("{}#{}", zf.rel, member.name);
        let member_size = i64::try_from(member.size).unwrap_or(i64::MAX);
        seen.push(member_rel.clone());
        if unchanged(conn, pid, &member_rel, member_size, mtime)? {
            continue;
        }
        let basename = files::basename(&member.name).to_owned();
        if files::crc_candidate_exists(conn, pid, &member.crc32, member_size)? {
            // CRC32 and size from the central directory, never the full payload: weaker
            // than a scan's hash, but enough to record the DAT match and its state.
            let hashes = Hashes {
                size: member.size,
                crc32: member.crc32.clone(),
                md5: String::new(),
                sha1: String::new(),
            };
            let (rom_id, state) = classify(conn, pid, &basename, &hashes)?;
            rows.push(NewFile {
                rel_path: member_rel,
                size: member_size,
                mtime,
                crc32: Some(member.crc32),
                md5: None,
                sha1: None,
                header_rule: Some("none".to_owned()),
                rom_id,
                state,
            });
        } else if let Some(rom_id) =
            arcade_rows::zip_rom_id(conn, super::PLATFORM, &zf.dir, &zf.name)?
        {
            // No DAT covers this member, but a live MRA names the zip: give
            // `verify_siblings` a row to promote once an import reads it.
            rows.push(NewFile {
                rel_path: member_rel,
                size: member_size,
                mtime,
                crc32: Some(member.crc32),
                md5: None,
                sha1: None,
                header_rule: Some("none".to_owned()),
                rom_id: Some(rom_id),
                state: FileState::Unverified,
            });
        }
    }
    Ok(())
}

/// [`scan_zip`] over one batch, on the read connection.
fn presence_batch(
    db: &Db,
    pid: &PlatformId,
    batch: &[ZipFile],
) -> Result<(Vec<NewFile>, Vec<String>)> {
    db.read_blocking(|c| {
        let mut rows = Vec::new();
        let mut seen = Vec::new();
        for zf in batch {
            scan_zip(c, pid, zf, &mut rows, &mut seen)?;
        }
        Ok((rows, seen))
    })
}

/// Writes one batch's rows in one transaction.
fn write_batch(
    conn: &mut Connection,
    pid: &PlatformId,
    rows: Vec<NewFile>,
    now: i64,
) -> Result<()> {
    let tx = conn.transaction()?;
    for row in rows {
        let hashed = Hashed {
            crc32: row.crc32.as_deref(),
            md5: row.md5.as_deref(),
            sha1: row.sha1.as_deref(),
            header_rule: row.header_rule.as_deref(),
        };
        files::upsert(
            &tx,
            pid,
            &row.rel_path,
            row.size,
            row.mtime,
            &hashed,
            row.rom_id,
            row.state,
            now,
        )?;
    }
    crate::db::commit(tx)?;
    Ok(())
}

/// Walks `games/mame` and `games/hbmame`, records what their zips verify against the DAT
/// and the live MRA catalogue this run just stored, and prunes rows whose zip or member
/// is gone. Runs after the catalogue's own titles are committed, so a zip an MRA newly
/// names this run is already visible to [`arcade_rows::zip_rom_id`].
pub(super) async fn run(ctx: &JobContext) -> Result<Stats> {
    let games = ctx.app.config().paths.games.clone();
    let Some(platform) = mistarr_mister::platforms::by_id(super::PLATFORM) else {
        return Ok(Stats::default());
    };
    ctx.checkpoint().await?;
    let zips = super::blocking({
        let games = games.clone();
        move || list_zips(&games, platform)
    })
    .await?;
    let total = zips.len();
    let pid = PlatformId(super::PLATFORM.to_owned());
    let mut keep: Vec<String> = Vec::with_capacity(total);
    for (n, batch) in zips.chunks(BATCH).enumerate() {
        ctx.checkpoint().await?;
        let (db, pid2, batch) = (ctx.app.db.clone(), pid.clone(), batch.to_vec());
        let (rows, seen) = super::blocking(move || presence_batch(&db, &pid2, &batch)).await??;
        keep.extend(seen);
        let (pid3, now) = (pid.clone(), crate::unix_now());
        ctx.app
            .db
            .write(move |c| write_batch(c, &pid3, rows, now))
            .await?;
        ctx.progress(json!({
            "presence_done": ((n + 1) * BATCH).min(total),
            "presence_total": total,
        }))
        .await?;
    }
    ctx.checkpoint().await?;
    let pid4 = pid.clone();
    let pruned = ctx
        .app
        .db
        .write(move |c| {
            let tx = c.transaction()?;
            let pruned = files::delete_missing(&tx, &pid4, &keep)?;
            crate::db::commit(tx)?;
            Ok(pruned)
        })
        .await?;
    Ok(Stats {
        zips: total,
        pruned,
    })
}

#[cfg(test)]
mod tests;
