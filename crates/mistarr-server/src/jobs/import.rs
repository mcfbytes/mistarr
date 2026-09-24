//! The importer: hashes a finished download, matches it against the wanted
//! entry, then quarantines or places it; see `docs/ARCHITECTURE.md` "Import"
//! and `docs/VERIFICATION.md`.

mod mra;
pub mod place;
mod rename;
mod support;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use mistarr_clients::{ClientTorrentId, SeedPolicy};
use mistarr_core::hash::HeaderRule;
use mistarr_core::PlatformId;
use mistarr_mister::platforms::{self, Kind, Platform};
use mistarr_mister::{
    adapter_for, CoreAdapter, DatEntry, PlacementPlan, StagedFile, StagedKind, StagedMember, Step,
};
use serde_json::{json, Value};
use tokio::sync::broadcast::error::RecvError;

use self::place::{Partial, PlaceError, Roots};
pub use self::rename::{rename, RenameError};
pub use self::support::parse_header;
use self::support::{
    dat_rom, explain, file_name, hash_item, header_rule, is_zip, leaf, locate, match_members,
    pick_rom, quarantine, read_head, rel_string, report, Hashed,
};
use super::{transfer, Job, JobContext, Lane, Scheduler};
use crate::app::AppState;
use crate::db::downloads::{self, DownloadId, DownloadRow, DownloadState};
use crate::db::files::{self, FileId, FileRow, FileState};
use crate::db::imports::{self, EntryRom, ImportAction, TitleEntry};
use crate::db::jobs as job_rows;
use crate::db::sources::{self, SourceRow};
use crate::db::{downloads_import, titles::TitleId};
use crate::error::{Error, Result};
use crate::events::EventKind;

/// `jobs.kind` of [`ImportJob`].
pub const KIND: &str = "import";

/// Why a BIOS entry is never imported, from `docs/PRINCIPLES.md` section 3.
const BIOS_REFUSED: &str = "BIOS entries are never imported";

const OUTSIDE_STAGING: &str = "the staged file is outside the staging directory";

/// Imports one download in `importing`; a disc entry's tracks are imported together.
pub struct ImportJob {
    /// The download to import.
    pub download_id: DownloadId,
}

impl ImportJob {
    fn payload_of(id: DownloadId) -> Value {
        json!({ "download_id": id.0 })
    }
}

#[async_trait]
impl Job for ImportJob {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn payload(&self) -> Value {
        Self::payload_of(self.download_id)
    }

    fn lane(&self) -> Lane {
        Lane::Heavy
    }

    async fn run(&self, ctx: &JobContext) -> Result<()> {
        import(ctx, self.download_id).await
    }
}

/// Enqueues an import for every download in `importing`, then one whenever
/// `download.changed` reports a download entering `importing`. A download that
/// fails or is cancelled wakes the `importing` tracks of its entry waiting on
/// it. Missed events trigger a full sweep.
pub async fn watch(app: Arc<AppState>) {
    let mut live = app.events.subscribe(None).live;
    sweep(&app).await;
    let mut stop = app.shutdown_signal();
    loop {
        let next = tokio::select! {
            r = live.recv() => r,
            _ = stop.wait_for(|s| *s) => return,
        };
        match next {
            Ok(ev) if ev.kind == EventKind::DownloadChanged => {
                let body: Value = serde_json::from_str(&ev.data).unwrap_or(Value::Null);
                let Some(id) = body["download_id"].as_i64().map(DownloadId) else {
                    continue;
                };
                match body["state"].as_str() {
                    Some("importing") => enqueue(&app, vec![id]).await,
                    Some("failed" | "cancelled") => waiting_siblings(&app, id).await,
                    _ => {}
                }
            }
            Ok(_) => {}
            Err(RecvError::Lagged(_)) => sweep(&app).await,
            Err(RecvError::Closed) => return,
        }
    }
}

/// Enqueues an import per download unless one is queued or running for it.
async fn enqueue(app: &Arc<AppState>, ids: Vec<DownloadId>) {
    for download_id in ids {
        let payload = ImportJob::payload_of(download_id);
        let open = app
            .db
            .read(move |c| job_rows::find_open(c, KIND, &payload))
            .await;
        match open {
            Ok(Some(_)) => continue,
            Ok(None) => {}
            Err(e) => tracing::warn!(error = %e, "cannot read import jobs"),
        }
        if let Err(e) = Scheduler::enqueue(app, Arc::new(ImportJob { download_id })).await {
            tracing::warn!(download = %download_id, error = %e, "cannot enqueue import");
        }
    }
}

async fn sweep(app: &Arc<AppState>) {
    let rows = app
        .db
        .read(|c| downloads::list(c, &[DownloadState::Importing], u32::MAX, 0))
        .await;
    match rows {
        Ok((rows, _)) => {
            // Oldest first, as the downloads were handed over.
            let mut ids: Vec<DownloadId> = rows.into_iter().map(|r| r.id).collect();
            ids.sort_by_key(|id| id.0);
            enqueue(app, ids).await;
        }
        Err(e) => tracing::warn!(error = %e, "cannot list downloads to import"),
    }
}

async fn waiting_siblings(app: &Arc<AppState>, id: DownloadId) {
    let ids = app
        .db
        .read(move |c| {
            let Some(row) = downloads::get(c, id)? else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            for sibling in downloads_import::for_title(c, row.title_id)? {
                if downloads::get(c, sibling)?.is_some_and(|r| r.state == DownloadState::Importing)
                {
                    out.push(sibling);
                }
            }
            Ok(out)
        })
        .await;
    match ids {
        Ok(ids) => enqueue(app, ids).await,
        Err(e) => tracing::warn!(download = %id, error = %e, "cannot read download"),
    }
}

/// Moves downloads out of `importing` and publishes each change.
async fn finish(
    app: &AppState,
    ids: &[DownloadId],
    to: DownloadState,
    reason: Option<&str>,
) -> Result<()> {
    let (list, error) = (ids.to_vec(), reason.map(str::to_owned));
    let moved = app
        .db
        .write(move |c| downloads::move_all(c, &list, to, error.as_deref(), crate::unix_now()))
        .await?;
    for id in moved {
        transfer::publish(app, id, to, 1.0);
    }
    Ok(())
}

async fn fail(app: &AppState, ids: &[DownloadId], reason: &str) -> Result<()> {
    finish(app, ids, DownloadState::Failed, Some(reason)).await
}

/// `(size, mtime)` as `files` stores them.
fn stat(meta: &fs::Metadata) -> (i64, i64) {
    let size = i64::try_from(meta.len()).unwrap_or(i64::MAX);
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    (size, mtime)
}

fn task(e: &tokio::task::JoinError) -> Error {
    Error::Task(e.to_string())
}

async fn import(ctx: &JobContext, id: DownloadId) -> Result<()> {
    let app = &ctx.app;
    let Some(row) = app.db.read(move |c| downloads::get(c, id)).await? else {
        return Ok(());
    };
    if row.state != DownloadState::Importing {
        return Ok(());
    }
    let Some(source_id) = row.source_id else {
        return fail(app, &[id], "the transfer has no source").await;
    };
    let title = row.title_id;
    let (entry, source) = app
        .db
        .read(move |c| Ok((imports::title_entry(c, title)?, sources::get(c, source_id)?)))
        .await?;
    let Some(source) = source else {
        return fail(app, &[id], "the source of this transfer is gone").await;
    };
    let Some(entry) = entry else {
        return fail(app, &[id], "the wanted entry is not in the catalog").await;
    };
    if entry.is_bios() {
        return fail(app, &[id], BIOS_REFUSED).await;
    }
    let (Some(platform), Some(adapter)) = (
        platforms::by_id(&entry.platform_id.0),
        adapter_for(&entry.platform_id),
    ) else {
        return fail(app, &[id], "the entry's platform has no adapter").await;
    };
    let config = app.config();
    let placing = Placing {
        ctx,
        staging: config.paths.staging(),
        games: config.paths.games,
        platform,
        adapter,
        source: &source,
        entry: &entry,
    };
    let outcome = match platform.kind {
        _ if entry.from_mra => placing.mra(&row).await,
        Kind::Disc => placing.disc().await,
        Kind::Romset | Kind::Arcade => placing.romset(&row).await,
        _ => placing.single(&row).await,
    };
    // Any outcome may be the source's last open download, a quarantine included.
    placing.release_torrent().await;
    outcome
}

/// A payload matched to a rom of the wanted entry and bound for `games/`.
#[derive(Debug, Clone)]
struct Piece {
    download: DownloadId,
    /// The staged file or zip it came from.
    source: PathBuf,
    hashed: Hashed,
    rom: EntryRom,
    /// The `files.state` to record, when not the one the rom's hash match gives.
    state: Option<FileState>,
}

/// Why a staged item is quarantined, when it is more than a hash mismatch.
struct Why {
    /// The download's error.
    reason: String,
    /// The opening of the report beside the quarantined file.
    report: String,
    /// Fields added to the `quarantined` log entry.
    detail: Value,
}

impl Piece {
    /// The file name the payload has in staging once any unzip ran.
    fn staged_name(&self) -> String {
        match &self.hashed.member {
            Some(m) => leaf(m).to_owned(),
            None => file_name(&self.source),
        }
    }
}

/// What happens at one `Rename` target.
enum Decision {
    Place,
    Replace(Option<FileRow>),
    Skip(FileId),
}

struct Target {
    from: PathBuf,
    rel: String,
    /// The whole staged zip moves here; `files` then holds one row per member.
    whole_zip: bool,
    decision: Decision,
}

/// How far a placement got.
enum Placed {
    /// Every library step ran; the stats of each landed target by `rel`.
    All(HashMap<String, (i64, i64)>),
    /// The final renames stopped partway.
    Partly(HashMap<String, (i64, i64)>, PlaceError),
}

/// One import in progress: the wanted entry, its platform and the torrent.
struct Placing<'a> {
    ctx: &'a JobContext,
    staging: PathBuf,
    games: PathBuf,
    platform: &'static Platform,
    adapter: &'static dyn CoreAdapter,
    source: &'a SourceRow,
    entry: &'a TitleEntry,
}

impl Placing<'_> {
    fn app(&self) -> &Arc<AppState> {
        &self.ctx.app
    }

    fn pid(&self) -> PlatformId {
        self.platform.platform_id()
    }

    fn rule(&self) -> HeaderRule {
        header_rule(self.platform.header_rule)
    }

    /// The staged file of `row` on disk, or why there is none to import.
    async fn local(&self, row: &DownloadRow) -> Result<std::result::Result<PathBuf, &'static str>> {
        let Some(staged) = row.staged_path.clone() else {
            return Ok(Err("the transfer has no staged file"));
        };
        let (source, index) = (self.source.id, row.file_index);
        let inner = match index {
            Some(i) => {
                self.app()
                    .db
                    .read(move |c| downloads_import::torrent_path(c, source, i))
                    .await?
            }
            None => None,
        };
        let (staging, hash) = (self.staging.clone(), self.source.infohash.clone());
        let found =
            tokio::task::spawn_blocking(move || locate(&staging, &hash, &staged, inner.as_deref()))
                .await
                .map_err(|e| task(&e))?;
        Ok(found.ok_or(OUTSIDE_STAGING))
    }

    /// True when the staged file is gone because an earlier attempt placed it.
    async fn already_placed(&self, local: &Path, rom_id: i64) -> Result<bool> {
        if local.exists() {
            return Ok(false);
        }
        self.app()
            .db
            .read(move |c| files::has_verified(c, rom_id))
            .await
    }

    async fn hash(&self, local: &Path) -> Result<std::result::Result<Vec<Hashed>, String>> {
        let (path, rule) = (local.to_path_buf(), self.rule());
        let hashed = tokio::task::spawn_blocking(move || hash_item(&path, rule))
            .await
            .map_err(|e| task(&e))?;
        Ok(hashed.map_err(|e| format!("cannot read the staged file: {e}")))
    }

    async fn head(&self, path: &Path, member: Option<&str>) -> Result<Vec<u8>> {
        let (path, member) = (path.to_path_buf(), member.map(str::to_owned));
        let head = tokio::task::spawn_blocking(move || read_head(&path, member.as_deref()))
            .await
            .map_err(|e| task(&e))?;
        Ok(head.unwrap_or_default())
    }

    /// Quarantines a staged item that is not the wanted entry and marks its
    /// download `bad`, naming the other entry it matches, if any.
    async fn quarantine(
        &self,
        row: DownloadId,
        rom_id: i64,
        local: &Path,
        actual: &[Hashed],
    ) -> Result<()> {
        self.quarantine_with(row, rom_id, local, actual, None).await
    }

    /// [`Self::quarantine`], with the report, reason and log detail led by `why` when given.
    async fn quarantine_with(
        &self,
        row: DownloadId,
        rom_id: i64,
        local: &Path,
        actual: &[Hashed],
        why: Option<Why>,
    ) -> Result<()> {
        let (pid, list) = (self.pid(), actual.to_vec());
        let (expected, other) = self
            .app()
            .db
            .read(move |c| {
                let mut other = None;
                for a in &list {
                    let h = &a.hashes;
                    let size = i64::try_from(h.size).unwrap_or(i64::MAX);
                    if let Some(m) = files::match_rom(c, &pid, &h.sha1, &h.md5, &h.crc32, size)? {
                        let title = imports::title_entry(c, TitleId(m.title_id))?;
                        other = title.map(|t| (t.id, t.name, m.name));
                        break;
                    }
                }
                Ok((imports::rom(c, rom_id)?, other))
            })
            .await?;
        let named = other
            .as_ref()
            .map(|(_, title, rom)| format!("{title} ({rom})"));
        let text = match &why {
            Some(w) => explain(&w.report, actual),
            None => report(
                expected.as_ref(),
                actual,
                named.as_deref(),
                self.platform.header_rule,
            ),
        };
        let (staging, hash, item) = (
            self.staging.clone(),
            self.source.infohash.clone(),
            local.to_path_buf(),
        );
        let moved = tokio::task::spawn_blocking(move || quarantine(&staging, &hash, &item, &text))
            .await
            .map_err(|e| task(&e))?;
        let dst = moved.unwrap_or_else(|e| {
            tracing::warn!(download = %row, error = %e, "cannot move the file to quarantine");
            local.to_path_buf()
        });
        let actual_json: Vec<Value> = actual
            .iter()
            .map(|a| {
                let h = &a.hashes;
                json!({ "member": a.member, "size": h.size, "crc32": h.crc32, "md5": h.md5, "sha1": h.sha1 })
            })
            .collect();
        let mut detail = json!({
            "path": dst.to_string_lossy(),
            "expected": expected,
            "actual": actual_json,
            "other": other.map(|(id, title, rom)| json!({ "title_id": id.0, "title": title, "rom": rom })),
        });
        let reason = match (why, named) {
            (Some(w), _) => {
                merge(&mut detail, &w.detail);
                w.reason
            }
            (None, Some(n)) => format!("the file is {n}, not the wanted entry; it was quarantined"),
            (None, None) => "the file matches no DAT entry and was quarantined".to_owned(),
        };
        let moved = self
            .app()
            .db
            .write(move |c| {
                let now = crate::unix_now();
                let moved = downloads::move_all(c, &[row], DownloadState::Bad, Some(&reason), now)?;
                if !moved.is_empty() {
                    imports::log(
                        c,
                        now,
                        Some(row.0),
                        None,
                        ImportAction::Quarantined,
                        &detail,
                    )?;
                }
                Ok(moved)
            })
            .await?;
        for id in moved {
            transfer::publish(self.app(), id, DownloadState::Bad, 1.0);
        }
        Ok(())
    }

    /// A cartridge download: one staged file, or one member of a staged zip.
    async fn single(&self, row: &DownloadRow) -> Result<()> {
        let app = self.app();
        let local = match self.local(row).await? {
            Ok(p) => p,
            Err(reason) => return fail(app, &[row.id], reason).await,
        };
        if self.already_placed(&local, row.rom_id).await? {
            return finish(app, &[row.id], DownloadState::Done, None).await;
        }
        self.ctx.checkpoint().await?;
        let candidates = match self.hash(&local).await? {
            Ok(h) => h,
            Err(reason) => return fail(app, &[row.id], &reason).await,
        };
        let hit = candidates.iter().find_map(|c| {
            let name = c.member.as_deref();
            pick_rom(&self.entry.roms, &c.hashes, Some(row.rom_id), name, &[]).map(|r| (c, r))
        });
        let Some((hashed, rom)) = hit else {
            return self
                .quarantine(row.id, row.rom_id, &local, &candidates)
                .await;
        };
        let head = self.head(&local, hashed.member.as_deref()).await?;
        let size = fs::metadata(&local).map_or(0, |m| m.len());
        let staged = match &hashed.member {
            None => StagedFile {
                path: local.clone(),
                size,
                kind: StagedKind::File,
                head,
                members: Vec::new(),
            },
            Some(name) => StagedFile {
                path: local.clone(),
                size,
                kind: StagedKind::Zip,
                head: Vec::new(),
                members: vec![StagedMember {
                    name: name.clone(),
                    size: hashed.raw_size,
                    head,
                }],
            },
        };
        let dat = DatEntry {
            name: self.entry.name.clone(),
            roms: vec![dat_rom(rom)],
        };
        let piece = Piece {
            download: row.id,
            source: local.clone(),
            hashed: hashed.clone(),
            rom: rom.clone(),
            state: None,
        };
        self.place(&dat, &staged, vec![piece], &[row.id], &[local])
            .await
    }

    /// A romset or arcade download: a zip whose every member must be a rom of
    /// the entry and which holds every rom of it.
    async fn romset(&self, row: &DownloadRow) -> Result<()> {
        let app = self.app();
        let title = self.entry.id;
        let rows = app
            .db
            .read(move |c| {
                let mut out = Vec::new();
                for id in downloads_import::for_title(c, title)? {
                    out.extend(downloads::get(c, id)?);
                }
                Ok(out)
            })
            .await?;
        let ids: Vec<DownloadId> = rows
            .iter()
            .filter(|r| {
                r.state == DownloadState::Importing
                    && r.source_id == row.source_id
                    && r.file_index == row.file_index
            })
            .map(|r| r.id)
            .collect();
        let local = match self.local(row).await? {
            Ok(p) => p,
            Err(reason) => return fail(app, &ids, reason).await,
        };
        if self.already_placed(&local, row.rom_id).await? {
            return finish(app, &ids, DownloadState::Done, None).await;
        }
        if !is_zip(&local) {
            return fail(app, &ids, "a romset is imported from a zip").await;
        }
        self.ctx.checkpoint().await?;
        let members = match self.hash(&local).await? {
            Ok(h) => h,
            Err(reason) => return fail(app, &ids, &reason).await,
        };
        let set = match_members(&self.entry.roms, &members);
        if !set.is_exact() {
            self.quarantine(row.id, row.rom_id, &local, &members)
                .await?;
            return fail(app, &ids, "the romset did not verify and was quarantined").await;
        }
        let pieces: Vec<Piece> = set
            .pairs
            .iter()
            .map(|(m, rom)| Piece {
                download: row.id,
                source: local.clone(),
                hashed: (*m).clone(),
                rom: (*rom).clone(),
                state: None,
            })
            .collect();
        let mut staged_members = Vec::with_capacity(members.len());
        for m in &members {
            let name = m.member.clone().unwrap_or_default();
            let head = self.head(&local, Some(&name)).await?;
            staged_members.push(StagedMember {
                name,
                size: m.raw_size,
                head,
            });
        }
        let staged = StagedFile {
            path: local.clone(),
            size: fs::metadata(&local).map_or(0, |m| m.len()),
            kind: StagedKind::Zip,
            head: Vec::new(),
            members: staged_members,
        };
        let dat = DatEntry {
            name: self.entry.name.clone(),
            roms: self.entry.roms.iter().map(dat_rom).collect(),
        };
        self.place(&dat, &staged, pieces, &ids, &[local]).await
    }

    /// A disc download: waits for every track of the entry, then places them
    /// together. Tracks an earlier attempt already placed count as present.
    async fn disc(&self) -> Result<()> {
        let app = self.app();
        let title = self.entry.id;
        let rows = app
            .db
            .read(move |c| {
                let mut out = Vec::new();
                for id in downloads_import::for_title(c, title)? {
                    out.extend(downloads::get(c, id)?);
                }
                Ok(out)
            })
            .await?;
        let staged: Vec<&DownloadRow> = rows
            .iter()
            .filter(|r| r.state == DownloadState::Importing)
            .collect();
        let ids: Vec<DownloadId> = staged.iter().map(|r| r.id).collect();
        let covered = self
            .entry
            .roms
            .iter()
            .filter(|rom| staged.iter().any(|r| r.rom_id == rom.id))
            .count();
        if covered < self.entry.roms.len() || self.entry.roms.is_empty() {
            let in_flight = rows.iter().any(|r| {
                matches!(
                    r.state,
                    DownloadState::Wanted
                        | DownloadState::Queued
                        | DownloadState::Transferring
                        | DownloadState::Checking
                )
            });
            if in_flight {
                return Ok(());
            }
            let reason = format!(
                "only {covered} of the {} tracks of this entry are staged; they stay in staging",
                self.entry.roms.len()
            );
            return fail(app, &ids, &reason).await;
        }
        let mut pieces = Vec::new();
        let mut placed_before = Vec::new();
        let mut used = Vec::new();
        for r in &staged {
            self.ctx.checkpoint().await?;
            let local = match self.local(r).await? {
                Ok(p) => p,
                Err(reason) => return fail(app, &ids, reason).await,
            };
            if self.already_placed(&local, r.rom_id).await? {
                used.push(r.rom_id);
                placed_before.push((r.rom_id, local));
                continue;
            }
            let candidates = match self.hash(&local).await? {
                Ok(h) => h,
                Err(reason) => return fail(app, &ids, &reason).await,
            };
            let hit = match candidates.first() {
                Some(c) if c.member.is_none() => pick_rom(
                    &self.entry.roms,
                    &c.hashes,
                    Some(r.rom_id),
                    Some(&file_name(&local)),
                    &used,
                )
                .map(|rom| (c.clone(), rom.clone())),
                _ => None,
            };
            let Some((hashed, rom)) = hit else {
                self.quarantine(r.id, r.rom_id, &local, &candidates).await?;
                let reason = "a track of this entry did not verify; the others stay in staging";
                return fail(app, &ids, reason).await;
            };
            used.push(rom.id);
            pieces.push(Piece {
                download: r.id,
                source: local,
                hashed,
                rom,
                state: None,
            });
        }
        if pieces.is_empty() {
            return finish(app, &ids, DownloadState::Done, None).await;
        }
        let Some(staged_dir) = self.staged_dir(&pieces, &placed_before).await? else {
            return fail(app, &ids, "the tracks are in different staging directories").await;
        };
        let dat = DatEntry {
            name: self.entry.name.clone(),
            roms: self.entry.roms.iter().map(dat_rom).collect(),
        };
        let originals: Vec<PathBuf> = pieces.iter().map(|p| p.source.clone()).collect();
        self.place(&dat, &staged_dir, pieces, &ids, &originals)
            .await
    }

    /// The directory holding every track, as a staged directory of those
    /// tracks plus, by DAT name, the ones placed before; `None` when the
    /// tracks are not all in one directory.
    async fn staged_dir(
        &self,
        pieces: &[Piece],
        placed_before: &[(i64, PathBuf)],
    ) -> Result<Option<StagedFile>> {
        let parent = pieces
            .first()
            .and_then(|p| p.source.parent())
            .map(Path::to_path_buf);
        let Some(parent) = parent.filter(|d| {
            pieces
                .iter()
                .all(|p| p.source.parent() == Some(d.as_path()))
        }) else {
            return Ok(None);
        };
        let mut members = Vec::with_capacity(pieces.len() + placed_before.len());
        for p in pieces {
            members.push(StagedMember {
                name: file_name(&p.source),
                size: p.hashed.raw_size,
                head: self.head(&p.source, None).await?,
            });
        }
        for (rom_id, _) in placed_before {
            if let Some(rom) = self.entry.roms.iter().find(|r| r.id == *rom_id) {
                members.push(StagedMember {
                    name: leaf(&rom.name).to_owned(),
                    size: rom.size,
                    head: Vec::new(),
                });
            }
        }
        Ok(Some(StagedFile {
            path: parent,
            size: 0,
            kind: StagedKind::Dir,
            head: Vec::new(),
            members,
        }))
    }

    /// Plans and applies a placement, then records it. Staging steps run in a
    /// scratch directory first and are rolled back on failure; the final
    /// renames run last, and the files that landed are recorded even when a
    /// later one fails, so a retry completes the rest.
    async fn place(
        &self,
        dat: &DatEntry,
        staged: &StagedFile,
        pieces: Vec<Piece>,
        ids: &[DownloadId],
        originals: &[PathBuf],
    ) -> Result<()> {
        let plan = match self.adapter.plan_placement(dat, staged) {
            Ok(p) => p,
            Err(e) => return fail(self.app(), ids, &format!("cannot place the file: {e}")).await,
        };
        self.place_plan(&plan, staged, pieces, ids, originals, None)
            .await
            .map(|_| ())
    }

    /// Applies `plan` and records it as [`Self::place`] does, adding `note` to each
    /// `import_log` entry. Returns whether every file landed.
    async fn place_plan(
        &self,
        plan: &PlacementPlan,
        staged: &StagedFile,
        pieces: Vec<Piece>,
        ids: &[DownloadId],
        originals: &[PathBuf],
        note: Option<Value>,
    ) -> Result<bool> {
        let app = self.app();
        let whole =
            (staged.kind == StagedKind::Zip).then(|| PathBuf::from(file_name(&staged.path)));
        let targets = self.targets(plan, whole.as_deref()).await?;
        let steps: Vec<Step> = plan
            .steps
            .iter()
            .filter(|s| match s {
                Step::Rename { from, .. } => !targets
                    .iter()
                    .any(|t| &t.from == from && matches!(t.decision, Decision::Skip(_))),
                _ => true,
            })
            .cloned()
            .collect();
        let scratch = self
            .staging
            .join(".import")
            .join(ids.first().map_or(0, |i| i.0).to_string());
        let (staging, games, item, originals) = (
            self.staging.clone(),
            self.games.clone(),
            staged.path.clone(),
            originals.to_vec(),
        );
        self.ctx.checkpoint().await?;
        let applied = tokio::task::spawn_blocking(move || {
            apply_plan(&staging, &item, &scratch, &games, &steps, &originals)
        })
        .await
        .map_err(|e| task(&e))?;
        match applied {
            Err(e) => {
                let reason = format!("cannot place the file, nothing was changed: {e}");
                fail(app, ids, &reason).await.map(|()| false)
            }
            Ok(Placed::All(stats)) => {
                let done = self
                    .record(targets, &stats, pieces, ids, true, note)
                    .await?;
                self.announce(ids, &done);
                Ok(true)
            }
            Ok(Placed::Partly(stats, error)) => {
                let total = targets.len();
                let landed = stats.len();
                let done = self
                    .record(targets, &stats, pieces, ids, false, note)
                    .await?;
                self.announce(&[], &done);
                let reason = format!(
                    "placement stopped after {landed} of {total} files ({error}); retry to finish"
                );
                fail(app, ids, &reason).await.map(|()| false)
            }
        }
    }

    /// Decides, per `Rename` step, whether to place, replace or keep what is there.
    async fn targets(&self, plan: &PlacementPlan, whole: Option<&Path>) -> Result<Vec<Target>> {
        let mut out = Vec::new();
        for step in &plan.steps {
            let Step::Rename { from, to } = step else {
                continue;
            };
            let rel = rel_string(to);
            let exists = self.games.join(to).exists();
            let whole_zip = whole.is_some_and(|w| w == from);
            let (pid, r) = (self.pid(), rel.clone());
            let rows = self
                .app()
                .db
                .read(move |c| {
                    if whole_zip {
                        files::zip_member_rows(c, &pid, &r)
                    } else {
                        Ok(files::find_by_path(c, &pid, &r)?.into_iter().collect())
                    }
                })
                .await?;
            let verified = !rows.is_empty() && rows.iter().all(|f| f.state == FileState::Verified);
            let decision = match rows.into_iter().next() {
                Some(row) if exists && verified => Decision::Skip(row.id),
                row if exists => Decision::Replace(row),
                _ => Decision::Place,
            };
            out.push(Target {
                from: from.clone(),
                rel,
                whole_zip,
                decision,
            });
        }
        Ok(out)
    }

    fn announce(&self, ids: &[DownloadId], done: &[(FileId, ImportAction)]) {
        let app = self.app();
        for id in ids {
            transfer::publish(app, *id, DownloadState::Done, 1.0);
        }
        for (file_id, action) in done {
            app.events.publish(
                EventKind::ImportDone,
                &json!({ "title_id": self.entry.id.0, "file_id": file_id.0, "action": action.as_str() }),
            );
        }
    }

    /// Writes `files` rows the scanner would write for every landed target,
    /// `import_log`, and, when `complete`, the downloads, in one transaction.
    async fn record(
        &self,
        targets: Vec<Target>,
        stats: &HashMap<String, (i64, i64)>,
        pieces: Vec<Piece>,
        ids: &[DownloadId],
        complete: bool,
        note: Option<Value>,
    ) -> Result<Vec<(FileId, ImportAction)>> {
        let scope = Scope {
            pid: self.pid(),
            rule: self.platform.header_rule,
            title: self.entry.id,
            stats: stats.clone(),
            note,
        };
        let ids = ids.to_vec();
        self.app()
            .db
            .write(move |c| {
                let tx = c.transaction()?;
                let now = crate::unix_now();
                let mut done = Vec::new();
                for t in &targets {
                    record_target(&tx, &scope, t, &pieces, now, &mut done)?;
                }
                if complete {
                    downloads::move_all(&tx, &ids, DownloadState::Done, None, now)?;
                }
                tx.commit()?;
                Ok(done)
            })
            .await
    }

    /// Removes the torrent from the client, keeping its data, once a source
    /// has a `done` download and none still selected and its seed policy is
    /// `none`, then clears empty staging directories.
    async fn release_torrent(&self) {
        let app = self.app();
        let source_id = self.source.id;
        let fresh = app
            .db
            .read(move |c| {
                let busy = downloads::of_source(
                    c,
                    source_id,
                    &[
                        DownloadState::Queued,
                        DownloadState::Transferring,
                        DownloadState::Checking,
                        DownloadState::Importing,
                    ],
                )?;
                let done = downloads::of_source(c, source_id, &[DownloadState::Done])?;
                Ok((
                    busy.is_empty() && !done.is_empty(),
                    sources::get(c, source_id)?,
                ))
            })
            .await;
        let (settled, source) = match fresh {
            Ok((settled, Some(source))) => (settled, source),
            Ok(_) => return,
            Err(e) => {
                tracing::warn!(error = %e, "cannot read the source after an import");
                return;
            }
        };
        if !settled || sources::seed_from_text(&source.seed_policy) != Some(SeedPolicy::None) {
            return;
        }
        if let Some(client_id) = source.client_id.as_deref() {
            let Some(client) = app.client() else {
                return;
            };
            if let Err(e) = client.remove(&ClientTorrentId::new(client_id), false).await {
                tracing::warn!(source = %source.id.0, error = %e, "cannot remove the finished torrent from the client");
                return;
            }
            if let Err(e) = app
                .db
                .write(move |c| sources::set_client_id(c, source_id, None))
                .await
            {
                tracing::warn!(error = %e, "cannot clear the source's client id");
            }
        }
        let dir = self.staging.join(&source.infohash);
        let _ = tokio::task::spawn_blocking(move || place::remove_empty_dirs(&dir)).await;
    }
}

/// What [`record_target`] needs beyond the target itself.
struct Scope {
    pid: PlatformId,
    rule: &'static str,
    title: TitleId,
    stats: HashMap<String, (i64, i64)>,
    /// Fields added to every log entry, such as how an MRA zip was verified.
    note: Option<Value>,
}

/// Copies the fields of the object `extra` into the object `into`.
fn merge(into: &mut Value, extra: &Value) {
    if let (Some(into), Some(extra)) = (into.as_object_mut(), extra.as_object()) {
        for (k, v) in extra {
            into.insert(k.clone(), v.clone());
        }
    }
}

fn log_detail(scope: &Scope, p: &Piece, rel: &str) -> Value {
    let mut detail = json!({
        "rel_path": rel,
        "title_id": scope.title.0,
        "rom_id": p.rom.id,
        "staged": file_name(&p.source),
        "member": p.hashed.member,
    });
    if let Some(note) = &scope.note {
        merge(&mut detail, note);
    }
    detail
}

fn previous(prev: Option<&FileRow>, rel: &str) -> Value {
    json!({
        "rel_path": prev.map_or(rel, |f| f.rel_path.as_str()),
        "state": prev.map(|f| f.state.as_str()),
        "rom_id": prev.and_then(|f| f.rom_id),
        "sha1": prev.and_then(|f| f.sha1.clone()),
    })
}

/// Records one target: a kept file, or the rows of a landed one, one per
/// member for a whole zip as the scanner keeps them.
fn record_target(
    tx: &rusqlite::Connection,
    scope: &Scope,
    t: &Target,
    pieces: &[Piece],
    now: i64,
    done: &mut Vec<(FileId, ImportAction)>,
) -> Result<()> {
    let from_name = t.from.file_name().map(|n| n.to_string_lossy().into_owned());
    let owned: Vec<&Piece> = pieces
        .iter()
        .filter(|p| {
            t.whole_zip || pieces.len() == 1 || from_name.as_deref() == Some(&p.staged_name())
        })
        .collect();
    let Some(first) = owned.first() else {
        return Ok(());
    };
    if let Decision::Skip(id) = t.decision {
        let detail = log_detail(scope, first, &t.rel);
        let action = ImportAction::SkippedExisting;
        imports::log(tx, now, Some(first.download.0), Some(id.0), action, &detail)?;
        done.push((id, action));
        return Ok(());
    }
    let Some(&(size, mtime)) = scope.stats.get(&t.rel) else {
        return Ok(());
    };
    let member_rel = |p: &Piece| {
        let member = p.hashed.member.as_deref().unwrap_or_default();
        format!("{}#{member}", t.rel)
    };
    if t.whole_zip {
        let keep: Vec<String> = owned.iter().map(|p| member_rel(p)).collect();
        for old in files::zip_member_rows(tx, &scope.pid, &t.rel)? {
            if !keep.contains(&old.rel_path) {
                files::delete(tx, old.id)?;
            }
        }
    }
    let rows = if t.whole_zip { owned.len() } else { 1 };
    for p in owned.into_iter().take(rows) {
        let (rel, size) = if t.whole_zip {
            let raw = i64::try_from(p.hashed.raw_size).unwrap_or(i64::MAX);
            (member_rel(p), raw)
        } else {
            (t.rel.clone(), size)
        };
        let h = &p.hashed.hashes;
        let hashed = files::Hashed {
            crc32: Some(&h.crc32),
            md5: Some(&h.md5),
            sha1: Some(&h.sha1),
            header_rule: Some(scope.rule),
        };
        let placed_state = file_state(p, t.whole_zip);
        let rom = Some(p.rom.id);
        let id = files::upsert(
            tx,
            &scope.pid,
            &rel,
            size,
            mtime,
            &hashed,
            rom,
            placed_state,
            now,
        )?;
        let mut detail = log_detail(scope, p, &rel);
        let action = match &t.decision {
            Decision::Replace(prev) => {
                detail["previous"] = previous(prev.as_ref(), &rel);
                ImportAction::Replaced
            }
            _ => ImportAction::Placed,
        };
        imports::log(tx, now, Some(p.download.0), Some(id.0), action, &detail)?;
        done.push((id, action));
    }
    Ok(())
}

/// The state the scanner would give a placed payload.
fn file_state(p: &Piece, whole_zip: bool) -> FileState {
    if let Some(state) = p.state {
        return state;
    }
    let named = p
        .hashed
        .member
        .as_deref()
        .is_none_or(|m| leaf(m) == leaf(&p.rom.name));
    if p.rom.status == "baddump" {
        FileState::Bad
    } else if whole_zip && !named {
        FileState::Misnamed
    } else {
        FileState::Verified
    }
}

/// Runs a plan: staging steps into `scratch`, then the library steps. On
/// success the consumed `originals` and the scratch directory are removed.
fn apply_plan(
    staging: &Path,
    item: &Path,
    scratch: &Path,
    games: &Path,
    steps: &[Step],
    originals: &[PathBuf],
) -> std::result::Result<Placed, PlaceError> {
    let roots = Roots::new(staging, item, scratch, games)?;
    let prepared = roots
        .check_same_filesystem()
        .and_then(|()| place::prepare(steps, &roots));
    if let Err(e) = prepared {
        roots.rollback();
        return Err(e);
    }
    let committed = place::commit(steps, &roots);
    roots.rollback();
    let (landed, error) = match committed {
        Ok(landed) => (landed, None),
        Err(Partial { landed, error }) => (landed, Some(error)),
    };
    let mut stats = HashMap::new();
    for to in &landed {
        let path = roots.library(to)?;
        let meta = fs::metadata(&path).map_err(|source| PlaceError::Io { path, source })?;
        stats.insert(rel_string(to), stat(&meta));
    }
    if let Some(error) = error {
        return Ok(Placed::Partly(stats, error));
    }
    for original in originals {
        if let Err(e) = roots.discard(original) {
            tracing::warn!(error = %e, "cannot remove a consumed staged file");
        }
    }
    Ok(Placed::All(stats))
}
