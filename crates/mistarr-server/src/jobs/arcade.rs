//! Arcade catalogue: the MRA files under `_Arcade` become titles of the arcade platform;
//! see `docs/ARCHITECTURE.md` "Arcade catalogue" and `docs/PLATFORMS.md` "MRA catalogue".

mod presence;

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use mistarr_core::naming::{group_key, parse_name};
use mistarr_core::PlatformId;
use mistarr_mister::adapter::arcade::assemble::{self, PartSource};
use mistarr_mister::adapter::arcade::mra::{self, zip_location, Mra, MraRom, ZipPath};
use serde_json::json;

use super::dat_import::prefs;
use super::{Job, JobContext, Lane, Scheduler};
use crate::app::AppState;
use crate::db::arcade::{self as rows, MraTitle, MraZip, StoredMra, StoredZip};
use crate::db::dats::DatVersionId;
use crate::db::jobs::JobId;
use crate::db::titles::{self, TitleId};
use crate::db::Db;
use crate::error::{Error, Result};

/// `jobs.kind` of [`ArcadeCatalog`].
pub const KIND: &str = "arcade_catalog";

/// The platform MRA titles belong to.
pub const PLATFORM: &str = "arcade";

/// Directory under the SD root holding the MRA files.
pub const ARCADE_DIR: &str = "_Arcade";

/// Deepest subfolder of `_Arcade` searched for MRA files.
const MAX_DEPTH: usize = 4;

/// Folders under `_Arcade` never searched, lowercase: the Arcade Organizer's tree holds
/// only links to MRA files found elsewhere.
const SKIPPED_DIRS: &[&str] = &["_organized"];

/// MRA files handled per blocking task and write transaction.
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
    let known = app.db.read(|c| rows::has_titles(c, PLATFORM)).await?;
    if !dir.is_dir() && !known {
        return Ok(None);
    }
    Scheduler::enqueue(app, Arc::new(ArcadeCatalog))
        .await
        .map(Some)
}

/// One `.mra` file found under `_Arcade`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Listed {
    /// Path relative to `_Arcade`, `/`-separated.
    rel: String,
    /// Parser version, size and mtime, as [`mra_stamp`] writes them.
    stamp: String,
}

/// One parsed MRA file.
#[derive(Debug)]
struct Entry {
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

/// An md5 check outcome as stored: the check and its detail, `None` when none applies.
type Outcome = Option<(&'static str, Option<String>)>;

/// What a batch writes for one MRA file.
enum Title {
    /// A new or changed MRA, stored in full.
    Stored {
        rel: String,
        stamp: String,
        name: String,
        setname: Option<String>,
        rbf: Option<String>,
        zips: Vec<Zip>,
    },
    /// An unchanged MRA: its title is stamped and the presence of each zip that changed updated.
    Kept {
        id: TitleId,
        present: Vec<(String, String, bool)>,
    },
}

/// The md5 check an item still needs.
enum Pending {
    /// The stored check still holds.
    Keep,
    /// Store this outcome and stamp; `(None, None)` clears the check.
    Set(Outcome, Option<String>),
    /// Check against `stamp`, with the MRA parsed already or read again from `rel`.
    Run {
        mra: Option<Mra>,
        rel: String,
        stamp: String,
    },
}

struct Item {
    title: Title,
    check: Pending,
}

/// What a run carries between batches: the zips found so far and the names taken.
#[derive(Default)]
struct Pass {
    index: ZipIndex,
    claimed: HashSet<String>,
    parsed: u64,
    checked: u64,
}

impl Pass {
    /// Takes `name` for this run; false when a shallower MRA already has it.
    fn claim(&mut self, name: &str, rel: &str) -> bool {
        let fresh = self.claimed.insert(name.to_lowercase());
        if !fresh {
            tracing::debug!(mra = %rel, "another MRA already has this name; skipped");
        }
        fresh
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Error::Task(e.to_string()))
}

/// Lists the MRA files, then per batch reads the new and changed ones, runs the md5 checks
/// whose inputs moved and stores the batch, so memory follows the batch, not the catalogue.
async fn catalogue(ctx: &JobContext) -> Result<()> {
    let config = ctx.app.config();
    let arcade = config.paths.root.join(ARCADE_DIR);
    let games = config.paths.games;
    ctx.checkpoint().await?;
    let listed = blocking({
        let arcade = arcade.clone();
        move || list_mras(&arcade)
    })
    .await?;
    let total = listed.len();
    let (version, run) = ctx
        .app
        .db
        .write(|c| {
            let version = rows::mra_version(c, PLATFORM, crate::unix_now())?;
            Ok((version, rows::next_run(c, PLATFORM)?))
        })
        .await?;
    let mut pass = Pass::default();
    for (n, batch) in listed.chunks(BATCH).enumerate() {
        ctx.checkpoint().await?;
        let (db, arcade2, games2, batch) = (
            ctx.app.db.clone(),
            arcade.clone(),
            games.clone(),
            batch.to_vec(),
        );
        let (back, items) = blocking(move || {
            let items = scan_batch(&db, &arcade2, &games2, &batch, &mut pass);
            (pass, items)
        })
        .await?;
        pass = back;
        let mut items = items?;
        for item in &mut items {
            let Pending::Run { mra, rel, stamp } = &mut item.check else {
                continue;
            };
            ctx.checkpoint().await?;
            let (mra, rel, stamp, arcade2) =
                (mra.take(), rel.clone(), stamp.clone(), arcade.clone());
            let (back, done) = blocking(move || {
                let done = run_check(&arcade2, &mut pass, mra, &rel, stamp);
                (pass, done)
            })
            .await?;
            pass = back;
            item.check = done;
        }
        ctx.app
            .db
            .write(move |c| store_batch(c, version, run, items))
            .await?;
        ctx.progress(json!({
            "done": ((n + 1) * BATCH).min(total),
            "total": total,
            "parsed": pass.parsed,
            "checked": pass.checked,
        }))
        .await?;
    }
    ctx.checkpoint().await?;
    let prefs = prefs(&config.prefs);
    let (retired, live) = ctx
        .app
        .db
        .write(move |c| {
            let tx = c.transaction()?;
            let retired = rows::retire_unseen(&tx, PLATFORM, run)?;
            let live = rows::live_count(&tx, PLATFORM)?;
            crate::db::dats::set_game_count(&tx, version, live)?;
            if retired > 0 || rows::recompute_pending(&tx, PLATFORM)? {
                titles::recompute_platform(&tx, PLATFORM, &prefs)?;
                rows::set_recompute_pending(&tx, PLATFORM, false)?;
            }
            tx.commit()?;
            Ok((retired, live))
        })
        .await?;
    super::remap::enqueue(&ctx.app, Some(vec![PlatformId(PLATFORM.into())])).await;
    // Runs after titles are committed, so a zip an MRA newly names this run is
    // already visible to the presence pass's live-MRA lookup.
    let stats = presence::run(ctx).await?;
    ctx.progress(json!({
        "done": total,
        "total": total,
        "mras": live,
        "parsed": pass.parsed,
        "retired": retired,
        "checked": pass.checked,
        "presence_zips": stats.zips,
        "presence_pruned": stats.pruned,
    }))
    .await
}

/// Reads the MRA files of `batch` that are new or changed and decides the md5 check of each.
fn scan_batch(
    db: &Db,
    arcade: &Path,
    games: &Path,
    batch: &[Listed],
    pass: &mut Pass,
) -> Result<Vec<Item>> {
    let mut items = Vec::with_capacity(batch.len());
    for listed in batch {
        let stored = db.read_blocking(|c| rows::stored_mra(c, PLATFORM, &listed.rel))?;
        if let Some(s) = stored
            .as_ref()
            .filter(|s| s.file_stamp.as_deref() == Some(listed.stamp.as_str()))
        {
            if pass.claim(&s.name, &listed.rel) {
                let zips = db.read_blocking(|c| rows::zip_roms(c, s.id))?;
                items.push(kept(s, listed, &zips, games, &mut pass.index));
            }
            continue;
        }
        pass.parsed += 1;
        let Some(entry) = read_entry(&listed.rel, &arcade.join(&listed.rel)) else {
            // A stored title whose MRA cannot be read now stays as it was until it can.
            if let Some(s) = stored.filter(|s| pass.claim(&s.name, &listed.rel)) {
                items.push(Item {
                    title: Title::Kept {
                        id: s.id,
                        present: Vec::new(),
                    },
                    check: Pending::Keep,
                });
            }
            continue;
        };
        if !pass.claim(&entry.name, &listed.rel) {
            continue;
        }
        let dirs = entry.mra.zip_paths().into_iter().map(|z| z.dir);
        pass.index.extend(games, dirs);
        let zips = zips_of(&entry.mra, &pass.index);
        let stamp = check_stamp(&listed.stamp, &entry.mra, &zips);
        let Entry { name, mra, .. } = entry;
        let (setname, rbf) = (mra.setname.clone(), mra.rbf.clone());
        let check = match stamp {
            Some(stamp) => Pending::Run {
                mra: Some(mra),
                rel: listed.rel.clone(),
                stamp,
            },
            None => Pending::Set(None, None),
        };
        items.push(Item {
            title: Title::Stored {
                rel: listed.rel.clone(),
                stamp: listed.stamp.clone(),
                name,
                setname,
                rbf,
                zips,
            },
            check,
        });
    }
    Ok(items)
}

/// An unchanged MRA's title, its zips looked up again and its md5 check redone when one moved.
fn kept(
    s: &StoredMra,
    listed: &Listed,
    zips: &[StoredZip],
    games: &Path,
    index: &mut ZipIndex,
) -> Item {
    index.extend(games, zips.iter().map(|z| z.zip_dir.clone()));
    let found: Vec<(ZipPath, Option<PathBuf>)> = zips
        .iter()
        .map(|z| {
            let path = ZipPath {
                dir: z.zip_dir.clone(),
                file: z.name.clone(),
            };
            let on_disk = index.find(&path);
            (path, on_disk)
        })
        .collect();
    let present = zips
        .iter()
        .zip(&found)
        .filter(|(z, (_, on_disk))| z.present != on_disk.is_some())
        .map(|(z, (_, on_disk))| (z.name.clone(), z.zip_dir.clone(), on_disk.is_some()))
        .collect();
    let stamp = if zips.iter().any(|z| z.has_md5) {
        joined_stamp(&listed.stamp, &found)
    } else {
        None
    };
    let check = match stamp {
        s2 if s2 == s.check_stamp => Pending::Keep,
        Some(stamp) => Pending::Run {
            mra: None,
            rel: listed.rel.clone(),
            stamp,
        },
        None => Pending::Set(None, None),
    };
    Item {
        title: Title::Kept { id: s.id, present },
        check,
    }
}

/// Runs one md5 check, reading the MRA again when it was not parsed in this batch.
fn run_check(
    arcade: &Path,
    pass: &mut Pass,
    mra: Option<Mra>,
    rel: &str,
    stamp: String,
) -> Pending {
    let mra = if let Some(m) = mra {
        m
    } else {
        pass.parsed += 1;
        match read_entry(rel, &arcade.join(rel)) {
            Some(e) => e.mra,
            // An MRA that cannot be read now keeps its last check until it can.
            None => return Pending::Keep,
        }
    };
    pass.checked += 1;
    let outcome = verify(&mra, &pass.index);
    let stamp = outcome.as_ref().and(Some(stamp));
    Pending::Set(outcome, stamp)
}

/// Writes one batch in one transaction, marking the picks stale when it stores a title.
fn store_batch(
    conn: &mut rusqlite::Connection,
    version: DatVersionId,
    run: i64,
    items: Vec<Item>,
) -> Result<()> {
    let tx = conn.transaction()?;
    if items
        .iter()
        .any(|i| matches!(i.title, Title::Stored { .. }))
    {
        rows::set_recompute_pending(&tx, PLATFORM, true)?;
    }
    for Item { title, check } in items {
        let id = match title {
            Title::Stored {
                rel,
                stamp,
                name,
                setname,
                rbf,
                zips,
            } => {
                let parsed = parse_name(&name);
                let regions: Vec<String> =
                    parsed.regions.iter().map(|r| r.name().to_owned()).collect();
                let flags = parsed.flag_labels();
                let key = format!("mra:{}", group_key(&parsed));
                let base = if parsed.base_name.is_empty() {
                    name.as_str()
                } else {
                    parsed.base_name.as_str()
                };
                let title = MraTitle {
                    name: &name,
                    base_name: base,
                    group_key: &key,
                    regions: &regions,
                    languages: &parsed.languages,
                    revision: parsed.revision.as_ref().map(|r| r.label.as_str()),
                    flags: &flags,
                    setname: setname.as_deref().filter(|s| !s.is_empty()),
                    rbf: rbf.as_deref().filter(|s| !s.is_empty()),
                    mra_path: &rel,
                    file_stamp: &stamp,
                    run,
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
                rows::upsert_title(&tx, PLATFORM, version, &title, &rom_zips)?
            }
            Title::Kept { id, present } => {
                rows::touch(&tx, id, run)?;
                for (file, dir, on_disk) in &present {
                    rows::set_zip_present(&tx, id, file, dir, *on_disk)?;
                }
                id
            }
        };
        if let Pending::Set(outcome, stamp) = check {
            let (check, detail) = outcome.map_or((None, None), |(c, d)| (Some(c), d));
            rows::set_check(&tx, id, check, detail.as_deref(), stamp.as_deref())?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Every `.mra` under `dir`, shallowest and then alphabetical first, with its stamp. Links to
/// files are followed; linked folders, folders in [`SKIPPED_DIRS`] and second names of one file are left out.
fn list_mras(dir: &Path) -> Vec<Listed> {
    fn walk(dir: &Path, rel: &str, depth: usize, out: &mut Vec<(Listed, (u64, u64))>) {
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
            if kind.is_dir() {
                let skipped = SKIPPED_DIRS.contains(&name.to_lowercase().as_str());
                if depth < MAX_DEPTH && !skipped {
                    walk(&entry.path(), &rel, depth + 1, out);
                }
            } else if (kind.is_file() || kind.is_symlink())
                && Path::new(&name)
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("mra"))
            {
                // Follows a link to its file, so the inode dedupe sees the target.
                let Ok(meta) = fs::metadata(entry.path()) else {
                    continue;
                };
                if meta.is_file() {
                    let stamp = mra_stamp(&meta);
                    out.push((Listed { rel, stamp }, (meta.dev(), meta.ino())));
                }
            }
        }
    }
    let mut found = Vec::new();
    walk(dir, "", 0, &mut found);
    found.sort_by(|a, b| {
        let depth = |s: &str| s.matches('/').count();
        depth(&a.0.rel)
            .cmp(&depth(&b.0.rel))
            .then_with(|| a.0.rel.cmp(&b.0.rel))
    });
    let mut files = HashSet::with_capacity(found.len());
    found
        .into_iter()
        .filter(|(_, id)| files.insert(*id))
        .map(|(l, _)| l)
        .collect()
}

/// Size and modification time in nanoseconds, so a rewrite within one second still counts.
fn stamp_of(meta: &fs::Metadata) -> String {
    let mtime = meta
        .modified()
        .ok()
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    format!("{}:{mtime}", meta.len())
}

/// [`stamp_of`] the file at `path`.
fn file_stamp(path: &Path) -> Option<String> {
    fs::metadata(path).ok().map(|m| stamp_of(&m))
}

/// [`stamp_of`] an MRA file behind the parser version, so a parser change rereads every MRA.
fn mra_stamp(meta: &fs::Metadata) -> String {
    format!("p{}:{}", mra::PARSER_VERSION, stamp_of(meta))
}

/// Parses the MRA at `path`, named by its `<name>` or else by the stem of `rel`.
fn read_entry(rel: &str, path: &Path) -> Option<Entry> {
    let mra = match mra::read(path) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(mra = %rel, error = %e, "cannot read MRA; skipped");
            return None;
        }
    };
    let stem = Path::new(rel)
        .file_stem()
        .map(|s| collapse(&s.to_string_lossy()))
        .unwrap_or_default();
    let name = mra
        .name
        .as_deref()
        .map(collapse)
        .filter(|n| !n.is_empty())
        .unwrap_or(stem);
    if name.is_empty() {
        return None;
    }
    Some(Entry {
        name,
        stamp: fs::metadata(path)
            .map(|m| mra_stamp(&m))
            .unwrap_or_default(),
        mra,
    })
}

/// `text` trimmed, with each run of whitespace inside it made one space.
fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `dir` (`/`-separated, relative to `games`) on disk, each component matched exactly
/// or else case-insensitively, as exFAT would.
fn resolve_dir(games: &Path, dir: &str) -> Option<PathBuf> {
    let mut path = games.to_path_buf();
    for part in dir.split('/').filter(|c| !c.is_empty()) {
        let exact = path.join(part);
        if exact.is_dir() {
            path = exact;
            continue;
        }
        let want = part.to_lowercase();
        path = fs::read_dir(&path)
            .ok()?
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().to_lowercase() == want)
            .map(|e| e.path())
            .find(|p| p.is_dir())?;
    }
    Some(path)
}

/// A directory on disk and its file names keyed by lowercase name.
type Listing = (PathBuf, HashMap<String, String>);

/// The files of each directory under `games/` an MRA reads zips from, directories and
/// files keyed by lowercase name, since exFAT, where MiSTer keeps them, is case-insensitive.
#[derive(Debug, Clone, Default)]
pub(super) struct ZipIndex {
    /// Per lowercase directory, where it is and its file names by lowercase name.
    dirs: HashMap<String, Option<Listing>>,
}

impl ZipIndex {
    /// An index of `dirs` under `games`.
    pub(super) fn build(games: &Path, dirs: Vec<String>) -> Self {
        let mut index = Self::default();
        index.extend(games, dirs);
        index
    }

    /// Lists each of `dirs` not listed yet.
    pub(super) fn extend(&mut self, games: &Path, dirs: impl IntoIterator<Item = String>) {
        for dir in dirs {
            let key = dir.to_lowercase();
            if self.dirs.contains_key(&key) {
                continue;
            }
            let listed = resolve_dir(games, &dir).map(|path| {
                let files = fs::read_dir(&path)
                    .map(|rd| {
                        rd.flatten()
                            .filter(|e| {
                                e.file_type().is_ok_and(|t| t.is_file()) || e.path().is_file()
                            })
                            .map(|e| {
                                let name = e.file_name().to_string_lossy().into_owned();
                                (name.to_lowercase(), name)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                (path, files)
            });
            self.dirs.insert(key, listed);
        }
    }

    /// Where `zip` is on disk, if listed and present.
    pub(super) fn find(&self, zip: &ZipPath) -> Option<PathBuf> {
        let (dir, files) = self.dirs.get(&zip.dir.to_lowercase())?.as_ref()?;
        files
            .get(&zip.file.to_lowercase())
            .map(|name| dir.join(name))
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
            let on_disk = index.find(&path);
            Zip { path, md5, on_disk }
        })
        .collect()
}

/// What the md5 check of an MRA stamped `mra_stamp` depends on, or `None` when it cannot
/// run: no `<rom>` carries an md5, or a zip is missing.
fn check_stamp(mra_stamp: &str, mra: &Mra, zips: &[Zip]) -> Option<String> {
    let any_md5 = mra
        .roms
        .iter()
        .any(|r| r.md5.is_some() && !r.zips.is_empty());
    if !any_md5 {
        return None;
    }
    let found: Vec<(ZipPath, Option<PathBuf>)> = zips
        .iter()
        .map(|z| (z.path.clone(), z.on_disk.clone()))
        .collect();
    joined_stamp(mra_stamp, &found)
}

/// `mra_stamp` followed by each zip's place and stamp in place order, so the order the MRA or
/// the database lists zips in never counts; `None` without zips or with one missing.
fn joined_stamp(mra_stamp: &str, zips: &[(ZipPath, Option<PathBuf>)]) -> Option<String> {
    if zips.is_empty() {
        return None;
    }
    let mut placed = zips
        .iter()
        .map(|(place, on_disk)| Some((place.rel_path(), on_disk.as_ref()?)))
        .collect::<Option<Vec<_>>>()?;
    placed.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    let mut stamp = mra_stamp.to_owned();
    for (place, path) in placed {
        stamp.push(';');
        stamp.push_str(&place);
        stamp.push(':');
        stamp.push_str(&file_stamp(path).unwrap_or_default());
    }
    Some(stamp)
}

/// Reads parts from the zips found under `games/`, or from a staged zip standing in for one
/// of them, and remembers every member it opened.
pub(super) struct ZipSource<'a> {
    index: &'a ZipIndex,
    staged: Option<(ZipPath, PathBuf)>,
    open: HashMap<PathBuf, zip::ZipArchive<File>>,
    /// Every member opened, as the zip's path and the member's name in the archive.
    pub(super) read: Vec<(PathBuf, String)>,
}

impl<'a> ZipSource<'a> {
    /// A source over `index`, reading `staged.0` from the file `staged.1` instead.
    pub(super) fn new(index: &'a ZipIndex, staged: Option<(ZipPath, PathBuf)>) -> Self {
        Self {
            index,
            staged,
            open: HashMap::new(),
            read: Vec::new(),
        }
    }

    /// Where the MRA zip name `zip` is read from, if anywhere.
    pub(super) fn locate(&self, zip: &str) -> Option<PathBuf> {
        let z = zip_location(zip)?;
        match &self.staged {
            Some((s, path)) if same_zip(s, &z) => Some(path.clone()),
            _ => self.index.find(&z),
        }
    }
}

/// Whether two zip places are the same on a case-insensitive filesystem.
pub(super) fn same_zip(a: &ZipPath, b: &ZipPath) -> bool {
    a.dir.eq_ignore_ascii_case(&b.dir) && a.file.eq_ignore_ascii_case(&b.file)
}

impl PartSource for ZipSource<'_> {
    fn open(
        &mut self,
        zip: &str,
        name: &str,
        crc: Option<u32>,
    ) -> io::Result<Option<Box<dyn Read + '_>>> {
        let Some(path) = self.locate(zip) else {
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
        self.read.push((path, file.name().to_owned()));
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
pub(super) enum Check {
    Match,
    Refused,
    MissingPart,
    Mismatch,
}

impl Check {
    pub(super) fn as_str(self) -> &'static str {
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
    let mut src = ZipSource::new(index, None);
    let roms = mra.roms.iter().filter(|r| !r.zips.is_empty());
    verify_roms(roms, &mut src).map(|(c, d)| (c.as_str(), d))
}

/// The md5 check of one `<rom>` against `expected`, with why it did not match.
pub(super) fn check_rom(
    rom: &MraRom,
    expected: &str,
    src: &mut dyn PartSource,
) -> (Check, Option<String>) {
    match assemble::md5(rom, src) {
        Ok(h) if h == expected => (Check::Match, None),
        Ok(_) => (
            Check::Mismatch,
            Some(format!("rom {} does not match the MRA's md5", rom.index)),
        ),
        Err(e @ mistarr_mister::Error::MissingPart { .. }) => {
            (Check::MissingPart, Some(format!("rom {}: {e}", rom.index)))
        }
        Err(e) => (Check::Refused, Some(format!("rom {}: {e}", rom.index))),
    }
}

/// Checks each of `roms` that carries an md5, one match per index being enough; the worst
/// index decides. `None` when none carries an md5.
pub(super) fn verify_roms<'r>(
    roms: impl IntoIterator<Item = &'r MraRom>,
    src: &mut dyn PartSource,
) -> Option<(Check, Option<String>)> {
    let mut by_index: Vec<(u32, Check, Option<String>)> = Vec::new();
    for rom in roms {
        let Some(expected) = &rom.md5 else {
            continue;
        };
        let (check, detail) = check_rom(rom, expected, src);
        match by_index.iter_mut().find(|(i, _, _)| *i == rom.index) {
            Some(slot) if slot.1 != Check::Match && check < slot.1 => {
                *slot = (rom.index, check, detail);
            }
            Some(_) => {}
            None => by_index.push((rom.index, check, detail)),
        }
    }
    let (_, worst, detail) = by_index.into_iter().max_by_key(|(_, c, _)| *c)?;
    Some((worst, detail))
}

/// One MRA title re-read after a zip it names was placed.
#[derive(Debug, Clone)]
pub(super) struct Refreshed {
    id: TitleId,
    /// Each zip it names, `(file, dir, present)`.
    zips: Vec<(String, String, bool)>,
    check: Option<(&'static str, Option<String>)>,
    stamp: Option<String>,
}

/// Re-reads each `(title, MRA path)` under `arcade_dir` and redoes its zip presence and md5
/// check as the catalogue does. Titles whose MRA cannot be read are left out.
pub(super) fn refresh(
    arcade_dir: &Path,
    games: &Path,
    titles: &[(TitleId, String)],
) -> Vec<Refreshed> {
    let mut out = Vec::with_capacity(titles.len());
    for (id, rel) in titles {
        let Some(entry) = read_entry(rel, &arcade_dir.join(rel)) else {
            continue;
        };
        let dirs = entry.mra.zip_paths().into_iter().map(|z| z.dir).collect();
        let index = ZipIndex::build(games, dirs);
        let zips = zips_of(&entry.mra, &index);
        let stamp = check_stamp(&entry.stamp, &entry.mra, &zips);
        let check = stamp.as_ref().and_then(|_| verify(&entry.mra, &index));
        out.push(Refreshed {
            id: *id,
            zips: zips
                .iter()
                .map(|z| (z.path.file.clone(), z.path.dir.clone(), z.on_disk.is_some()))
                .collect(),
            stamp: check.as_ref().and(stamp),
            check,
        });
    }
    out
}

/// Records what [`refresh`] found.
pub(super) fn store_refreshed(conn: &mut rusqlite::Connection, found: &[Refreshed]) -> Result<()> {
    let tx = conn.transaction()?;
    for r in found {
        for (file, dir, present) in &r.zips {
            rows::set_zip_present(&tx, r.id, file, dir, *present)?;
        }
        let (check, detail) = r.check.clone().map_or((None, None), |(c, d)| (Some(c), d));
        rows::set_check(&tx, r.id, check, detail.as_deref(), r.stamp.as_deref())?;
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests;
