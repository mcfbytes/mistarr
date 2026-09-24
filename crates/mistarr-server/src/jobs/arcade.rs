//! Arcade catalogue: the MRA files under `_Arcade` become titles of the arcade platform;
//! see `docs/PLATFORMS.md` "Special adapters" and "MRA assembly".

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use mistarr_core::naming::{group_key, parse_name};
use mistarr_mister::adapter::arcade::assemble::{self, PartSource};
use mistarr_mister::adapter::arcade::mra::{self, zip_location, Mra, ZipPath};
use serde_json::json;

use super::dat_import::prefs;
use super::{Job, JobContext, Lane, Scheduler};
use crate::app::AppState;
use crate::db::arcade::{self as rows, MraTitle, MraZip};
use crate::db::jobs::JobId;
use crate::db::titles::{self, TitleId};
use crate::error::{Error, Result};

/// `jobs.kind` of [`ArcadeCatalog`].
pub const KIND: &str = "arcade_catalog";

/// The platform MRA titles belong to.
pub const PLATFORM: &str = "arcade";

/// Directory under the SD root holding the MRA files.
pub const ARCADE_DIR: &str = "_Arcade";

/// Deepest subfolder of `_Arcade` searched for MRA files.
const MAX_DEPTH: usize = 4;

/// MRA files parsed per blocking task, between checkpoints.
const BATCH: usize = 64;

/// Reads every MRA under `_Arcade`, upserts one title per MRA, retires the titles
/// whose MRA is gone, and checks each complete set against the MRA's md5.
pub struct ArcadeCatalog;

#[async_trait]
impl Job for ArcadeCatalog {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn lane(&self) -> Lane {
        Lane::Heavy
    }

    async fn run(&self, ctx: &JobContext) -> Result<()> {
        catalogue(ctx).await
    }
}

/// Enqueues the catalogue when there is anything for it to do: an `_Arcade`
/// directory, or MRA titles that may need retiring. Returns the job, if queued.
///
/// # Errors
///
/// [`Error::Db`] when the job cannot be recorded.
pub async fn enqueue_if_relevant(app: &Arc<AppState>) -> Result<Option<JobId>> {
    let dir = app.config().paths.root.join(ARCADE_DIR);
    let known = app
        .db
        .read(|c| Ok(!rows::check_states(c, PLATFORM)?.is_empty()))
        .await?;
    if !dir.is_dir() && !known {
        return Ok(None);
    }
    Scheduler::enqueue(app, Arc::new(ArcadeCatalog))
        .await
        .map(Some)
}

/// One parsed MRA file.
#[derive(Debug, Clone)]
struct Entry {
    rel: String,
    name: String,
    mra: Mra,
    /// Size and mtime of the MRA file.
    stamp: String,
}

/// A zip an entry names, found or not.
#[derive(Debug, Clone)]
struct Zip {
    path: ZipPath,
    md5: Option<String>,
    on_disk: Option<PathBuf>,
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Error::Task(e.to_string()))
}

async fn catalogue(ctx: &JobContext) -> Result<()> {
    let config = ctx.app.config();
    let (dir, games) = (config.paths.root.join(ARCADE_DIR), config.paths.games);
    ctx.checkpoint().await?;
    let found = blocking(move || list_mras(&dir)).await?;
    let mut entries: Vec<Entry> = Vec::with_capacity(found.len());
    for batch in found.chunks(BATCH) {
        ctx.checkpoint().await?;
        let batch = batch.to_vec();
        entries.extend(
            blocking(move || batch.iter().filter_map(read_entry).collect::<Vec<_>>()).await?,
        );
    }
    let mut seen: HashSet<String> = HashSet::with_capacity(entries.len());
    entries.retain(|e| {
        let fresh = seen.insert(e.name.to_lowercase());
        if !fresh {
            tracing::debug!(mra = %e.rel, "another MRA already has this name; skipped");
        }
        fresh
    });
    let index = blocking({
        let dirs: Vec<String> = entries
            .iter()
            .flat_map(|e| e.mra.zip_paths().into_iter().map(|z| z.dir))
            .collect();
        move || Arc::new(ZipIndex::build(&games, dirs))
    })
    .await?;
    let zips: Vec<Vec<Zip>> = entries.iter().map(|e| zips_of(&e.mra, &index)).collect();

    ctx.checkpoint().await?;
    let prefs = prefs(&config.prefs);
    let stored = {
        let entries = entries.clone();
        let zips = zips.clone();
        ctx.app
            .db
            .write(move |c| store(c, &entries, &zips, &prefs))
            .await?
    };
    let retired = stored.retired;
    let states = ctx.app.db.read(|c| rows::check_states(c, PLATFORM)).await?;
    let mut checked = 0u64;
    for (entry, (zips, id)) in entries.iter().zip(zips.iter().zip(&stored.ids)) {
        let prior = states
            .iter()
            .find(|s| s.id == *id)
            .and_then(|s| s.stamp.clone());
        let stamp = check_stamp(entry, zips);
        if stamp == prior {
            continue;
        }
        ctx.checkpoint().await?;
        let outcome = if stamp.is_some() {
            let (mra, index) = (entry.mra.clone(), Arc::clone(&index));
            checked += 1;
            blocking(move || verify(&mra, &index)).await?
        } else {
            None
        };
        let id = *id;
        ctx.app
            .db
            .write(move |c| {
                let (check, detail) = outcome.map_or((None, None), |(c, d)| (Some(c), d));
                let stamp = check.and(stamp);
                rows::set_check(c, id, check, detail.as_deref(), stamp.as_deref())
            })
            .await?;
    }
    ctx.progress(json!({
        "mras": entries.len(),
        "retired": retired,
        "checked": checked,
    }))
    .await
}

/// What [`store`] wrote.
struct Stored {
    ids: Vec<TitleId>,
    retired: usize,
}

fn store(
    conn: &mut rusqlite::Connection,
    entries: &[Entry],
    zips: &[Vec<Zip>],
    prefs: &mistarr_core::select::Prefs,
) -> Result<Stored> {
    let tx = conn.transaction()?;
    let version = rows::mra_version(&tx, PLATFORM, crate::unix_now())?;
    rows::begin_load(&tx, PLATFORM)?;
    let mut ids = Vec::with_capacity(entries.len());
    for (e, zips) in entries.iter().zip(zips) {
        let parsed = parse_name(&e.name);
        let regions: Vec<String> = parsed.regions.iter().map(|r| r.name().to_owned()).collect();
        let flags = parsed.flag_labels();
        let key = format!("mra:{}", group_key(&parsed));
        let base = if parsed.base_name.is_empty() {
            e.name.as_str()
        } else {
            parsed.base_name.as_str()
        };
        let title = MraTitle {
            name: &e.name,
            base_name: base,
            group_key: &key,
            regions: &regions,
            languages: &parsed.languages,
            revision: parsed.revision.as_ref().map(|r| r.label.as_str()),
            flags: &flags,
            setname: e.mra.setname.as_deref().filter(|s| !s.is_empty()),
            rbf: e.mra.rbf.as_deref().filter(|s| !s.is_empty()),
            mra_path: &e.rel,
        };
        let rom_zips: Vec<MraZip<'_>> = zips
            .iter()
            .map(|z| MraZip {
                name: &z.path.file,
                zip_dir: &z.path.dir,
                md5: z.md5.as_deref(),
                present: z.on_disk.is_some(),
            })
            .collect();
        ids.push(rows::upsert_title(
            &tx, PLATFORM, version, &title, &rom_zips,
        )?);
    }
    let retired = rows::retire_absent(&tx, PLATFORM)?;
    crate::db::dats::set_game_count(&tx, version, entries.len() as u64)?;
    titles::recompute_platform(&tx, PLATFORM, prefs)?;
    tx.commit()?;
    Ok(Stored { ids, retired })
}

/// Every `.mra` under `dir`, relative path first, shallowest and then alphabetical first.
/// Symlinked directories are not followed.
fn list_mras(dir: &Path) -> Vec<(String, PathBuf)> {
    fn walk(dir: &Path, rel: &str, depth: usize, out: &mut Vec<(String, PathBuf)>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            let path = entry.path();
            if kind.is_dir() {
                if depth < MAX_DEPTH {
                    walk(&path, &rel, depth + 1, out);
                }
            } else if Path::new(&name)
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("mra"))
            {
                out.push((rel, path));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, "", 0, &mut out);
    out.sort_by(|a, b| {
        let depth = |s: &str| s.matches('/').count();
        depth(&a.0).cmp(&depth(&b.0)).then_with(|| a.0.cmp(&b.0))
    });
    out
}

/// Size and modification time in nanoseconds, so a rewrite within one second still counts.
fn file_stamp(path: &Path) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    Some(format!("{}:{mtime}", meta.len()))
}

fn read_entry((rel, path): &(String, PathBuf)) -> Option<Entry> {
    let mra = match mra::read(path) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(mra = %rel, error = %e, "cannot read MRA; skipped");
            return None;
        }
    };
    let stem = Path::new(rel)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = mra.name.clone().filter(|n| !n.is_empty()).unwrap_or(stem);
    if name.is_empty() {
        return None;
    }
    Some(Entry {
        rel: rel.clone(),
        name,
        stamp: file_stamp(path).unwrap_or_default(),
        mra,
    })
}

/// The zip files present in each directory under `games/`, keyed by lowercase name,
/// since exFAT, where MiSTer keeps them, is case-insensitive.
#[derive(Debug, Clone, Default)]
struct ZipIndex {
    dirs: HashMap<String, HashMap<String, PathBuf>>,
}

impl ZipIndex {
    fn build(games: &Path, dirs: Vec<String>) -> Self {
        let mut index = Self {
            dirs: HashMap::new(),
        };
        for dir in dirs {
            if index.dirs.contains_key(&dir) {
                continue;
            }
            let mut files = HashMap::new();
            if let Ok(rd) = fs::read_dir(games.join(&dir)) {
                for e in rd.flatten() {
                    if e.path().is_file() {
                        let name = e.file_name().to_string_lossy().to_lowercase();
                        files.insert(name, e.path());
                    }
                }
            }
            index.dirs.insert(dir, files);
        }
        index
    }

    fn find(&self, zip: &ZipPath) -> Option<&PathBuf> {
        self.dirs.get(&zip.dir)?.get(&zip.file.to_lowercase())
    }
}

/// The zips an MRA names with the md5 of the first `<rom>` that names each.
fn zips_of(mra: &Mra, index: &ZipIndex) -> Vec<Zip> {
    mra.zip_paths()
        .into_iter()
        .map(|path| {
            let md5 = mra
                .roms
                .iter()
                .filter(|r| r.md5.is_some())
                .find(|r| {
                    r.zips
                        .iter()
                        .any(|z| zip_location(z).as_ref() == Some(&path))
                })
                .and_then(|r| r.md5.clone());
            let on_disk = index.find(&path).cloned();
            Zip { path, md5, on_disk }
        })
        .collect()
}

/// What the md5 check of an entry depends on, or `None` when it cannot run:
/// no `<rom>` carries an md5, or a zip is missing.
fn check_stamp(entry: &Entry, zips: &[Zip]) -> Option<String> {
    let any_md5 = entry
        .mra
        .roms
        .iter()
        .any(|r| r.md5.is_some() && !r.zips.is_empty());
    if !any_md5 || zips.is_empty() {
        return None;
    }
    let mut stamp = entry.stamp.clone();
    for z in zips {
        let path = z.on_disk.as_ref()?;
        stamp.push(';');
        stamp.push_str(&z.path.rel_path());
        stamp.push(':');
        stamp.push_str(&file_stamp(path).unwrap_or_default());
    }
    Some(stamp)
}

/// Reads parts from the zips found under `games/`.
struct ZipSource<'a> {
    index: &'a ZipIndex,
    open: HashMap<PathBuf, zip::ZipArchive<File>>,
}

impl PartSource for ZipSource<'_> {
    fn open(
        &mut self,
        zip: &str,
        name: &str,
        crc: Option<u32>,
    ) -> io::Result<Option<Box<dyn Read + '_>>> {
        let Some(path) = zip_location(zip).and_then(|z| self.index.find(&z).cloned()) else {
            return Ok(None);
        };
        if !self.open.contains_key(&path) {
            let archive = zip::ZipArchive::new(File::open(&path)?).map_err(io::Error::other)?;
            self.open.insert(path.clone(), archive);
        }
        let Some(archive) = self.open.get_mut(&path) else {
            return Ok(None);
        };
        let Some(i) = member_index(archive, name, crc) else {
            return Ok(None);
        };
        let file = archive.by_index(i).map_err(io::Error::other)?;
        Ok(Some(Box::new(file)))
    }
}

/// The member named `name`, compared exactly then case-insensitively, else the one with CRC32 `crc`.
fn member_index(
    archive: &mut zip::ZipArchive<File>,
    name: &str,
    crc: Option<u32>,
) -> Option<usize> {
    if let Some(i) = archive.index_for_name(name) {
        return Some(i);
    }
    let lower = name.to_lowercase();
    let mut by_crc = None;
    for i in 0..archive.len() {
        let Ok(f) = archive.by_index_raw(i) else {
            continue;
        };
        if f.name().to_lowercase() == lower {
            return Some(i);
        }
        if by_crc.is_none() && crc == Some(f.crc32()) {
            by_crc = Some(i);
        }
    }
    by_crc
}

/// Outcome of the md5 check, worst first when several `<rom>` indexes disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Check {
    Match,
    Refused,
    MissingPart,
    Mismatch,
}

impl Check {
    fn as_str(self) -> &'static str {
        match self {
            Self::Match => "match",
            Self::Refused => "refused",
            Self::MissingPart => "missing_part",
            Self::Mismatch => "mismatch",
        }
    }
}

/// Checks every `<rom>` of `mra` that carries an md5. A `<rom>` index passes when any of
/// its alternatives matches, as MiSTer keeps the first valid rom 0. `None` when nothing is checkable.
fn verify(mra: &Mra, index: &ZipIndex) -> Option<(&'static str, Option<String>)> {
    let mut src = ZipSource {
        index,
        open: HashMap::new(),
    };
    let mut by_index: Vec<(u32, Check, Option<String>)> = Vec::new();
    for rom in mra.roms.iter().filter(|r| !r.zips.is_empty()) {
        let Some(expected) = &rom.md5 else {
            continue;
        };
        let (check, detail) = match assemble::md5(rom, &mut src) {
            Ok(h) if h == *expected => (Check::Match, None),
            Ok(_) => (
                Check::Mismatch,
                Some(format!("rom {} does not match the MRA's md5", rom.index)),
            ),
            Err(mistarr_mister::Error::MissingPart { part, zips }) => (
                Check::MissingPart,
                Some(format!("rom {}: part {part} is not in {zips}", rom.index)),
            ),
            Err(e) => (Check::Refused, Some(format!("rom {}: {e}", rom.index))),
        };
        match by_index.iter_mut().find(|(i, _, _)| *i == rom.index) {
            Some(slot) if slot.1 != Check::Match && check < slot.1 => {
                *slot = (rom.index, check, detail);
            }
            Some(_) => {}
            None => by_index.push((rom.index, check, detail)),
        }
    }
    let (_, worst, detail) = by_index.into_iter().max_by_key(|(_, c, _)| *c)?;
    Some((worst.as_str(), detail))
}

#[cfg(test)]
mod tests;
