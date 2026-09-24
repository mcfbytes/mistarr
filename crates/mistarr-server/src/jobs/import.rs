//! The importer: hashes a finished download, matches it, then quarantines or
//! places it; see `docs/ARCHITECTURE.md` "Import" and `docs/VERIFICATION.md`.

pub mod place;

use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use mistarr_clients::{ClientTorrentId, SeedPolicy};
use mistarr_core::hash::{hash_reader, hash_zip_member, zip_members, HashError, HeaderRule};
use mistarr_core::{HashSet as Hashes, PlatformId};
use mistarr_mister::platforms::{self, Kind, Platform};
use mistarr_mister::{
    adapter_for, CoreAdapter, DatEntry, DatRom, PlacementPlan, StagedFile, StagedKind,
    StagedMember, Step,
};
use serde_json::{json, Value};
use tokio::sync::broadcast::error::RecvError;

use self::place::{PlaceError, Roots};
use super::{Job, JobContext, Lane, Scheduler};
use crate::app::AppState;
use crate::db::downloads_import::{self as downloads, state, DownloadId, ImportRow};
use crate::db::files::{self, FileId, FileRow, FileState};
use crate::db::imports::{self, EntryRom, ImportAction, TitleEntry};
use crate::db::sources::{self, SourceId, SourceRow};
use crate::db::titles::TitleId;
use crate::error::{Error, Result};
use crate::events::EventKind;

/// `jobs.kind` of [`ImportJob`].
pub const KIND: &str = "import";

/// Bytes of a staged payload handed to the adapter as its head.
const HEAD_LEN: u64 = 16;

/// Imports one download in `importing`; a disc entry's tracks are imported together.
pub struct ImportJob {
    /// The download to import.
    pub download_id: DownloadId,
}

#[async_trait]
impl Job for ImportJob {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn payload(&self) -> Value {
        json!({ "download_id": self.download_id.0 })
    }

    fn lane(&self) -> Lane {
        Lane::Heavy
    }

    async fn run(&self, ctx: &JobContext) -> Result<()> {
        import(ctx, self.download_id).await
    }
}

/// Enqueues an import for every download in `importing`, then one for each
/// `importing` download of a title whenever `download.changed` names one of
/// its downloads. Missed events trigger a full sweep.
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
                let id = serde_json::from_str::<Value>(&ev.data)
                    .ok()
                    .and_then(|v| v["download_id"].as_i64());
                if let Some(id) = id {
                    changed(&app, DownloadId(id)).await;
                }
            }
            Ok(_) => {}
            Err(RecvError::Lagged(_)) => sweep(&app).await,
            Err(RecvError::Closed) => return,
        }
    }
}

async fn enqueue(app: &Arc<AppState>, ids: Vec<DownloadId>) {
    for download_id in ids {
        let job = Arc::new(ImportJob { download_id });
        if let Err(e) = Scheduler::enqueue(app, job).await {
            tracing::warn!(download = %download_id, error = %e, "cannot enqueue import");
        }
    }
}

async fn sweep(app: &Arc<AppState>) {
    match app.db.read(downloads::importing).await {
        Ok(ids) => enqueue(app, ids).await,
        Err(e) => tracing::warn!(error = %e, "cannot list downloads to import"),
    }
}

async fn changed(app: &Arc<AppState>, id: DownloadId) {
    let ids = app
        .db
        .read(move |c| {
            let Some(row) = downloads::get(c, id)? else {
                return Ok(Vec::new());
            };
            Ok(downloads::for_title(c, row.title_id)?
                .into_iter()
                .filter(|r| r.state == state::IMPORTING)
                .map(|r| r.id)
                .collect())
        })
        .await;
    match ids {
        Ok(ids) => enqueue(app, ids).await,
        Err(e) => tracing::warn!(download = %id, error = %e, "cannot read download"),
    }
}

/// The directories one import works with.
struct Env {
    staging: PathBuf,
    games: PathBuf,
}

impl Env {
    fn new(app: &AppState) -> Self {
        let config = app.config();
        Self {
            staging: config.paths.staging(),
            games: config.paths.games,
        }
    }
}

fn no_parent(path: &Path) -> bool {
    path.components()
        .all(|c| !matches!(c, Component::ParentDir))
}

/// The local path of a staged file. `staged` is the local path the poller
/// recorded; a relative one is taken inside `staging/<infohash>/`. When it is
/// missing, `torrent_path` is looked for there and one directory below, since a
/// magnet's display name may differ from the folder the client wrote. `None`
/// when the path is not inside staging.
fn locate(
    staging: &Path,
    infohash: &str,
    staged: &str,
    torrent_path: Option<&str>,
) -> Option<PathBuf> {
    let root = staging.join(infohash);
    let path = Path::new(staged);
    let local = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    if !no_parent(&local) || !local.starts_with(staging) {
        return None;
    }
    let Some(inner) = torrent_path
        .map(Path::new)
        .filter(|p| p.is_relative() && no_parent(p))
    else {
        return Some(local);
    };
    if local.exists() {
        return Some(local);
    }
    let mut dirs: Vec<PathBuf> = fs::read_dir(&root)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default();
    dirs.sort();
    std::iter::once(root.clone())
        .chain(dirs)
        .map(|d| d.join(inner))
        .find(|p| p.is_file())
        .or(Some(local))
}

/// A staged file hashed and matched to a rom of the title being placed.
#[derive(Debug, Clone)]
struct Track {
    download: DownloadId,
    local: PathBuf,
    member: Option<String>,
    hashes: Hashes,
    rom: EntryRom,
}

async fn import(ctx: &JobContext, id: DownloadId) -> Result<()> {
    let app = &ctx.app;
    let Some(row) = app.db.read(move |c| downloads::get(c, id)).await? else {
        return Ok(());
    };
    if row.state != state::IMPORTING {
        return Ok(());
    }
    let Some(source_id) = row.source_id else {
        return finish(app, &[id], state::FAILED, "the transfer has no source").await;
    };
    let title = row.title_id;
    let (entry, source) = app
        .db
        .read(move |c| Ok((imports::title_entry(c, title)?, sources::get(c, source_id)?)))
        .await?;
    let Some(source) = source else {
        return finish(
            app,
            &[id],
            state::FAILED,
            "the source of this transfer is gone",
        )
        .await;
    };
    let Some(entry) = entry else {
        return finish(
            app,
            &[id],
            state::FAILED,
            "the wanted entry is not in the catalog",
        )
        .await;
    };
    if entry.is_bios() {
        return finish(app, &[id], state::FAILED, BIOS_REFUSED).await;
    }
    let Some(platform) = platforms::by_id(&entry.platform_id.0) else {
        return finish(app, &[id], state::FAILED, "the entry's platform is unknown").await;
    };
    let Some(adapter) = adapter_for(&entry.platform_id) else {
        return finish(
            app,
            &[id],
            state::FAILED,
            "the entry's platform has no adapter",
        )
        .await;
    };
    let env = Env::new(app);
    let placing = Placing {
        ctx,
        env: &env,
        platform,
        adapter,
        source: &source,
    };
    if platform.kind == Kind::Disc {
        placing.disc(&entry).await
    } else {
        placing.single(&row).await
    }
}

/// Why a BIOS entry is never imported, from `docs/PRINCIPLES.md` section 3.
const BIOS_REFUSED: &str = "BIOS entries are never imported";

const OUTSIDE_STAGING: &str = "the staged file is outside the staging directory";

/// Moves downloads out of `importing` and publishes each change.
async fn finish(app: &AppState, ids: &[DownloadId], to: &'static str, reason: &str) -> Result<()> {
    let (list, reason) = (ids.to_vec(), reason.to_owned());
    let error = (to != state::DONE).then_some(reason);
    let moved = app
        .db
        .write(move |c| {
            let now = crate::unix_now();
            let mut moved = Vec::new();
            for id in list {
                if downloads::finish(c, id, to, error.as_deref(), now)? {
                    moved.push(id);
                }
            }
            Ok(moved)
        })
        .await?;
    for id in moved {
        publish_download(app, id, to);
    }
    Ok(())
}

fn publish_download(app: &AppState, id: DownloadId, to: &str) {
    app.events.publish(
        EventKind::DownloadChanged,
        &json!({ "download_id": id.0, "state": to, "progress": 1.0 }),
    );
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

fn is_zip(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
}

/// Hashes a staged file, or every member of a staged zip, under `rule`.
fn hash_item(path: &Path, rule: HeaderRule) -> Result<Vec<(Option<String>, Hashes)>, HashError> {
    if !is_zip(path) {
        let size = fs::metadata(path)?.len();
        let hashes = hash_reader(File::open(path)?, rule, Some(size))?;
        return Ok(vec![(None, hashes)]);
    }
    let mut out = Vec::new();
    for m in zip_members(File::open(path)?)? {
        if !m.name.ends_with('/') {
            let hashes = hash_zip_member(File::open(path)?, &m.name, rule)?;
            out.push((Some(m.name), hashes));
        }
    }
    Ok(out)
}

/// Up to [`HEAD_LEN`] leading bytes of a file or of one zip member.
fn read_head(path: &Path, member: Option<&str>) -> Result<Vec<u8>, HashError> {
    let mut buf = Vec::new();
    match member {
        None => {
            File::open(path)?.take(HEAD_LEN).read_to_end(&mut buf)?;
        }
        Some(name) => {
            let mut zip = zip::ZipArchive::new(File::open(path)?)?;
            zip.by_name(name)?.take(HEAD_LEN).read_to_end(&mut buf)?;
        }
    }
    Ok(buf)
}

/// Header bytes from a DAT `header` attribute written as hex, spaces allowed.
///
/// ```
/// use mistarr_server::jobs::import::parse_header;
/// assert_eq!(parse_header("4E 45 53 1a"), Some(b"NES\x1a".to_vec()));
/// assert_eq!(parse_header("no header"), None);
/// ```
#[must_use]
pub fn parse_header(text: &str) -> Option<Vec<u8>> {
    let hex: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if hex.is_empty() || hex.len() % 2 != 0 {
        return None;
    }
    hex.chunks(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).ok()?, 16).ok())
        .collect()
}

fn dat_rom(rom: &EntryRom) -> DatRom {
    DatRom {
        name: rom.name.clone(),
        size: rom.size,
        header: rom.header.as_deref().and_then(parse_header),
    }
}

/// A library path as `files.rel_path` stores it.
fn rel_string(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The quarantine report: the rom the transfer was chosen for and what arrived.
fn report(expected: Option<&EntryRom>, actual: &[(Option<String>, Hashes)], rule: &str) -> String {
    let mut out =
        String::from("This file matches no entry in the loaded DATs and was not placed.\n\n");
    match expected {
        Some(r) => {
            let _ = writeln!(out, "Expected: {} ({} bytes)", r.name, r.size);
            let dash = || "-".to_owned();
            let _ = writeln!(
                out,
                "  crc32 {}  md5 {}  sha1 {}",
                r.crc32.clone().unwrap_or_else(dash),
                r.md5.clone().unwrap_or_else(dash),
                r.sha1.clone().unwrap_or_else(dash)
            );
        }
        None => out.push_str("Expected: unknown rom\n"),
    }
    for (member, h) in actual {
        let label = member.as_deref().map_or(String::new(), |m| format!(" {m}"));
        let _ = writeln!(out, "Actual{label}: {} bytes", h.size);
        let _ = writeln!(out, "  crc32 {}  md5 {}  sha1 {}", h.crc32, h.md5, h.sha1);
    }
    let _ = writeln!(out, "Header rule: {rule}");
    out
}

/// Moves a staged item to `staging/quarantine/<infohash>/` with its report beside it.
fn quarantine(staging: &Path, infohash: &str, item: &Path, report: &str) -> io::Result<PathBuf> {
    let dir = staging.join("quarantine").join(infohash);
    fs::create_dir_all(&dir)?;
    let name = file_name(item);
    let dst = dir.join(&name);
    fs::rename(item, &dst)?;
    fs::write(dir.join(format!("{name}.report.txt")), report)?;
    Ok(dst)
}

/// The directory holding every track, as a staged directory of those tracks;
/// `None` when they are not all in one directory.
async fn staged_dir(tracks: &[Track]) -> Result<Option<StagedFile>> {
    let parent = tracks
        .first()
        .and_then(|t| t.local.parent())
        .map(Path::to_path_buf);
    let Some(parent) =
        parent.filter(|p| tracks.iter().all(|t| t.local.parent() == Some(p.as_path())))
    else {
        return Ok(None);
    };
    let mut members = Vec::with_capacity(tracks.len());
    for t in tracks {
        let path = t.local.clone();
        let head = tokio::task::spawn_blocking(move || read_head(&path, None))
            .await
            .map_err(|e| Error::Task(e.to_string()))?
            .unwrap_or_default();
        members.push(StagedMember {
            name: file_name(&t.local),
            size: t.hashes.size,
            head,
        });
    }
    Ok(Some(StagedFile {
        path: parent,
        size: 0,
        kind: StagedKind::Dir,
        head: Vec::new(),
        members,
    }))
}

/// What happens at one `Rename` target.
enum Decision {
    Place,
    Replace(Option<FileRow>),
    Skip(FileRow),
}

struct Target {
    from: PathBuf,
    rel: String,
    decision: Decision,
}

/// One import in progress: the platform, its adapter and the torrent.
struct Placing<'a> {
    ctx: &'a JobContext,
    env: &'a Env,
    platform: &'static Platform,
    adapter: &'static dyn CoreAdapter,
    source: &'a SourceRow,
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
    async fn local(&self, row: &ImportRow) -> Result<std::result::Result<PathBuf, &'static str>> {
        let Some(staged) = row.staged_path.clone() else {
            return Ok(Err("the transfer has no staged file"));
        };
        let (source, index) = (self.source.id, row.file_index);
        let inner = match index {
            Some(i) => {
                self.app()
                    .db
                    .read(move |c| downloads::torrent_path(c, source, i))
                    .await?
            }
            None => None,
        };
        let (staging, hash) = (self.env.staging.clone(), self.source.infohash.clone());
        let found =
            tokio::task::spawn_blocking(move || locate(&staging, &hash, &staged, inner.as_deref()))
                .await
                .map_err(|e| Error::Task(e.to_string()))?;
        Ok(found.ok_or(OUTSIDE_STAGING))
    }

    async fn hash(
        &self,
        local: &Path,
    ) -> Result<std::result::Result<Vec<(Option<String>, Hashes)>, String>> {
        let (path, rule) = (local.to_path_buf(), self.rule());
        let hashed = tokio::task::spawn_blocking(move || hash_item(&path, rule))
            .await
            .map_err(|e| Error::Task(e.to_string()))?;
        Ok(hashed.map_err(|e| format!("cannot read the staged file: {e}")))
    }

    async fn matched(&self, hashes: &Hashes) -> Result<Option<files::RomMatch>> {
        let (pid, h) = (self.pid(), hashes.clone());
        let size = i64::try_from(h.size).unwrap_or(i64::MAX);
        self.app()
            .db
            .read(move |c| files::match_rom(c, &pid, &h.sha1, &h.md5, &h.crc32, size))
            .await
    }

    /// Quarantines a staged item that matched nothing and marks its download `bad`.
    async fn quarantine(
        &self,
        row: &ImportRow,
        local: &Path,
        actual: &[(Option<String>, Hashes)],
    ) -> Result<()> {
        let rom_id = row.rom_id;
        let expected = self.app().db.read(move |c| imports::rom(c, rom_id)).await?;
        let text = report(expected.as_ref(), actual, self.platform.header_rule);
        let (staging, hash, item) = (
            self.env.staging.clone(),
            self.source.infohash.clone(),
            local.to_path_buf(),
        );
        let moved = tokio::task::spawn_blocking(move || quarantine(&staging, &hash, &item, &text))
            .await
            .map_err(|e| Error::Task(e.to_string()))?;
        let dst = match moved {
            Ok(dst) => dst,
            Err(e) => {
                tracing::warn!(download = %row.id, error = %e, "cannot move the file to quarantine");
                local.to_path_buf()
            }
        };
        let actual_json: Vec<Value> = actual
            .iter()
            .map(|(m, h)| json!({ "member": m, "size": h.size, "crc32": h.crc32, "md5": h.md5, "sha1": h.sha1 }))
            .collect();
        let detail = json!({
            "path": dst.to_string_lossy(),
            "expected": expected,
            "actual": actual_json,
        });
        let id = row.id;
        let logged = self
            .app()
            .db
            .write(move |c| {
                let now = crate::unix_now();
                let moved = downloads::finish(
                    c,
                    id,
                    state::BAD,
                    Some("the file matches no DAT entry and was quarantined"),
                    now,
                )?;
                if moved {
                    imports::log(c, now, Some(id.0), None, ImportAction::Quarantined, &detail)?;
                }
                Ok(moved)
            })
            .await?;
        if logged {
            publish_download(self.app(), id, state::BAD);
        }
        Ok(())
    }

    /// A cartridge, romset or arcade download: one staged file or zip.
    async fn single(&self, row: &ImportRow) -> Result<()> {
        let app = self.app();
        let local = match self.local(row).await? {
            Ok(p) => p,
            Err(reason) => return finish(app, &[row.id], state::FAILED, reason).await,
        };
        self.ctx.checkpoint().await?;
        let candidates = match self.hash(&local).await? {
            Ok(h) => h,
            Err(reason) => return finish(app, &[row.id], state::FAILED, &reason).await,
        };
        let mut found = None;
        for (member, hashes) in &candidates {
            if let Some(m) = self.matched(hashes).await? {
                found = Some((member.clone(), hashes.clone(), m));
                break;
            }
        }
        let Some((member, hashes, m)) = found else {
            return self.quarantine(row, &local, &candidates).await;
        };
        let title = TitleId(m.title_id);
        let entry = app.db.read(move |c| imports::title_entry(c, title)).await?;
        let Some(entry) = entry else {
            return finish(
                app,
                &[row.id],
                state::FAILED,
                "the matched entry is not in the catalog",
            )
            .await;
        };
        if entry.is_bios() {
            return finish(app, &[row.id], state::FAILED, BIOS_REFUSED).await;
        }
        let Some(rom) = entry.roms.iter().find(|r| r.id == m.rom_id).cloned() else {
            return finish(
                app,
                &[row.id],
                state::FAILED,
                "the matched rom is retired from its DAT",
            )
            .await;
        };
        let (path, mem) = (local.clone(), member.clone());
        let head = tokio::task::spawn_blocking(move || read_head(&path, mem.as_deref()))
            .await
            .map_err(|e| Error::Task(e.to_string()))?;
        let head = match head {
            Ok(h) => h,
            Err(e) => {
                let reason = format!("cannot read the staged file: {e}");
                return finish(app, &[row.id], state::FAILED, &reason).await;
            }
        };
        let size = fs::metadata(&local).map_or(0, |m| m.len());
        let staged_file = match &member {
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
                    size: hashes.size,
                    head,
                }],
            },
        };
        let dat = DatEntry {
            name: entry.name.clone(),
            roms: vec![dat_rom(&rom)],
        };
        let track = Track {
            download: row.id,
            local,
            member,
            hashes,
            rom,
        };
        self.place(&entry, &dat, &staged_file, vec![track]).await
    }

    /// A disc download: waits for every track of the entry, then places them together.
    async fn disc(&self, entry: &TitleEntry) -> Result<()> {
        let app = self.app();
        let title = entry.id;
        let rows = app.db.read(move |c| downloads::for_title(c, title)).await?;
        let staged: Vec<&ImportRow> = rows
            .iter()
            .filter(|r| r.state == state::IMPORTING)
            .collect();
        let ids: Vec<DownloadId> = staged.iter().map(|r| r.id).collect();
        let mut chosen: Vec<&ImportRow> = Vec::new();
        for rom in &entry.roms {
            if let Some(r) = staged.iter().find(|r| r.rom_id == rom.id) {
                chosen.push(r);
            }
        }
        if chosen.len() < entry.roms.len() || entry.roms.is_empty() {
            if rows
                .iter()
                .any(|r| state::IN_FLIGHT.contains(&r.state.as_str()))
            {
                return Ok(());
            }
            let reason = format!(
                "only {} of the {} tracks of this entry are staged; they stay in staging",
                chosen.len(),
                entry.roms.len()
            );
            return finish(app, &ids, state::FAILED, &reason).await;
        }
        let mut tracks = Vec::with_capacity(chosen.len());
        for r in chosen {
            self.ctx.checkpoint().await?;
            let local = match self.local(r).await? {
                Ok(p) => p,
                Err(reason) => return finish(app, &ids, state::FAILED, reason).await,
            };
            let candidates = match self.hash(&local).await? {
                Ok(h) => h,
                Err(reason) => return finish(app, &ids, state::FAILED, &reason).await,
            };
            let hit = match candidates.first() {
                Some((None, h)) => self.matched(h).await?.map(|m| (m, h.clone())),
                _ => None,
            };
            let Some((m, hashes)) = hit else {
                self.quarantine(r, &local, &candidates).await?;
                let reason = "a track of this entry did not verify; the others stay in staging";
                return finish(app, &ids, state::FAILED, reason).await;
            };
            let Some(rom) = entry.roms.iter().find(|x| x.id == m.rom_id).cloned() else {
                let reason = "a track matches a different entry; the tracks stay in staging";
                return finish(app, &ids, state::FAILED, reason).await;
            };
            tracks.push(Track {
                download: r.id,
                local,
                member: None,
                hashes,
                rom,
            });
        }
        let mut covered: Vec<i64> = tracks.iter().map(|t| t.rom.id).collect();
        covered.sort_unstable();
        covered.dedup();
        if covered.len() != entry.roms.len() {
            let reason = "two tracks hold the same data; the tracks stay in staging";
            return finish(app, &ids, state::FAILED, reason).await;
        }
        let Some(staged_dir) = staged_dir(&tracks).await? else {
            let reason = "the tracks are in different staging directories";
            return finish(app, &ids, state::FAILED, reason).await;
        };
        let dat = DatEntry {
            name: entry.name.clone(),
            roms: entry.roms.iter().map(dat_rom).collect(),
        };
        self.place(entry, &dat, &staged_dir, tracks).await
    }

    /// Plans, applies and records a placement; any failure leaves the files in staging.
    async fn place(
        &self,
        entry: &TitleEntry,
        dat: &DatEntry,
        staged: &StagedFile,
        tracks: Vec<Track>,
    ) -> Result<()> {
        let app = self.app();
        let ids: Vec<DownloadId> = tracks.iter().map(|t| t.download).collect();
        let plan = match self.adapter.plan_placement(dat, staged) {
            Ok(p) => p,
            Err(e) => {
                let reason = format!("cannot place the file: {e}");
                return finish(app, &ids, state::FAILED, &reason).await;
            }
        };
        let targets = self.targets(&plan).await?;
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
        let skipped: Vec<PathBuf> = targets
            .iter()
            .filter(|t| matches!(t.decision, Decision::Skip(_)))
            .map(|t| t.from.clone())
            .collect();
        let placed: Vec<String> = targets
            .iter()
            .filter(|t| !matches!(t.decision, Decision::Skip(_)))
            .map(|t| t.rel.clone())
            .collect();
        let (staging, games, item, zip_input) = (
            self.env.staging.clone(),
            self.env.games.clone(),
            staged.path.clone(),
            staged.kind == StagedKind::Zip,
        );
        self.ctx.checkpoint().await?;
        let applied = tokio::task::spawn_blocking(
            move || -> std::result::Result<Vec<(i64, i64)>, PlaceError> {
                let roots = Roots::new(&staging, &item, &games)?;
                roots.check_same_filesystem()?;
                place::apply(&steps, &roots)?;
                for from in &skipped {
                    roots.discard(&roots.stage(from)?)?;
                }
                if zip_input {
                    roots.discard(roots.staged())?;
                }
                placed
                    .iter()
                    .map(|rel| {
                        let path = roots.library(Path::new(rel))?;
                        let meta = fs::metadata(&path).map_err(|source| PlaceError::Io {
                            path: path.clone(),
                            source,
                        })?;
                        Ok(stat(&meta))
                    })
                    .collect()
            },
        )
        .await
        .map_err(|e| Error::Task(e.to_string()))?;
        let stats = match applied {
            Ok(s) => s,
            Err(e) => {
                let reason = format!("cannot place the file: {e}");
                return finish(app, &ids, state::FAILED, &reason).await;
            }
        };
        let done = self.record(entry, targets, stats, tracks).await?;
        for id in &ids {
            publish_download(app, *id, state::DONE);
        }
        for (file_id, action) in done {
            app.events.publish(
                EventKind::ImportDone,
                &json!({ "title_id": entry.id.0, "file_id": file_id.0, "action": action.as_str() }),
            );
        }
        self.release_torrent().await;
        Ok(())
    }

    /// Decides, per `Rename` step, whether to place, replace or keep what is there.
    async fn targets(&self, plan: &PlacementPlan) -> Result<Vec<Target>> {
        let mut out = Vec::new();
        for step in &plan.steps {
            let Step::Rename { from, to } = step else {
                continue;
            };
            let rel = rel_string(to);
            let exists = self.env.games.join(to).exists();
            let (pid, r) = (self.pid(), rel.clone());
            let row = self
                .app()
                .db
                .read(move |c| files::find_by_path(c, &pid, &r))
                .await?;
            let decision = match row {
                Some(row) if exists && row.state == FileState::Verified => Decision::Skip(row),
                row if exists => Decision::Replace(row),
                _ => Decision::Place,
            };
            out.push(Target {
                from: from.clone(),
                rel,
                decision,
            });
        }
        Ok(out)
    }

    /// Writes `files`, `import_log` and the downloads in one transaction.
    async fn record(
        &self,
        entry: &TitleEntry,
        targets: Vec<Target>,
        stats: Vec<(i64, i64)>,
        tracks: Vec<Track>,
    ) -> Result<Vec<(FileId, ImportAction)>> {
        let (pid, rule) = (self.pid(), self.platform.header_rule);
        let title = entry.id;
        self.app()
            .db
            .write(move |c| {
                let tx = c.transaction()?;
                let now = crate::unix_now();
                let mut stats = stats.into_iter();
                let mut done = Vec::new();
                for t in targets {
                    let track = tracks
                        .iter()
                        .find(|k| tracks.len() == 1 || t.from.file_name() == k.local.file_name());
                    let Some(track) = track else { continue };
                    let base = json!({
                        "rel_path": t.rel,
                        "title_id": title.0,
                        "rom_id": track.rom.id,
                        "staged": file_name(&track.local),
                        "member": track.member,
                    });
                    let (file_id, action, detail) = match t.decision {
                        Decision::Skip(row) => (row.id, ImportAction::SkippedExisting, base),
                        decision => {
                            let (size, mtime) = stats.next().unwrap_or((0, 0));
                            let file_state = if track.rom.status == "baddump" {
                                FileState::Bad
                            } else {
                                FileState::Verified
                            };
                            let hashed = files::Hashed {
                                crc32: Some(&track.hashes.crc32),
                                md5: Some(&track.hashes.md5),
                                sha1: Some(&track.hashes.sha1),
                                header_rule: Some(rule),
                            };
                            let id = files::upsert(
                                &tx,
                                &pid,
                                &t.rel,
                                size,
                                mtime,
                                &hashed,
                                Some(track.rom.id),
                                file_state,
                                now,
                            )?;
                            match decision {
                                Decision::Replace(prev) => {
                                    let mut detail = base;
                                    detail["previous"] = json!({
                                        "rel_path": t.rel,
                                        "state": prev.as_ref().map(|p| p.state.as_str()),
                                        "rom_id": prev.as_ref().and_then(|p| p.rom_id),
                                        "sha1": prev.and_then(|p| p.sha1),
                                    });
                                    (id, ImportAction::Replaced, detail)
                                }
                                _ => (id, ImportAction::Placed, base),
                            }
                        }
                    };
                    imports::log(
                        &tx,
                        now,
                        Some(track.download.0),
                        Some(file_id.0),
                        action,
                        &detail,
                    )?;
                    done.push((file_id, action));
                }
                for t in &tracks {
                    downloads::finish(&tx, t.download, state::DONE, None, now)?;
                }
                tx.commit()?;
                Ok(done)
            })
            .await
    }

    /// Removes the torrent from the client, keeping its data, once every
    /// download of it is done and its seed policy is `none`, then clears empty staging.
    async fn release_torrent(&self) {
        let app = self.app();
        let source_id: SourceId = self.source.id;
        let fresh = app
            .db
            .read(move |c| {
                Ok((
                    downloads::source_settled(c, source_id)?,
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
        let dir = self.env.staging.join(&source.infohash);
        let _ = tokio::task::spawn_blocking(move || place::remove_empty_dirs(&dir)).await;
    }
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

/// Why a rename request cannot be carried out, for the API to report.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RenameError {
    /// No such file, or it is not in the title's clone group.
    #[error("no such file in this title")]
    NotFound,
    /// The file is not `misnamed`, lives in a zip, or needs more than a rename.
    #[error("{0}")]
    Refused(String),
    /// Another file already has the canonical name.
    #[error("`{0}` already exists")]
    Conflict(String),
    /// A server failure.
    #[error(transparent)]
    Server(#[from] Error),
}

/// The misnamed file `file_id` of group `group` with its entry and rom.
async fn rename_subject(
    app: &AppState,
    group: TitleId,
    file_id: FileId,
) -> std::result::Result<(FileRow, TitleEntry, EntryRom), RenameError> {
    let found = app
        .db
        .read(move |c| {
            let Some(file) = files::get(c, file_id)? else {
                return Ok(None);
            };
            let Some(title) = file
                .rom_id
                .map(|r| imports::title_of_rom(c, r))
                .transpose()?
                .flatten()
            else {
                return Ok(Some((file, None)));
            };
            if crate::db::titles::group_of(c, title)? != Some(group) {
                return Ok(None);
            }
            Ok(Some((file, imports::title_entry(c, title)?)))
        })
        .await?;
    let Some((file, entry)) = found else {
        return Err(RenameError::NotFound);
    };
    if file.state != FileState::Misnamed {
        return Err(RenameError::Refused(format!(
            "the file is {}, not misnamed",
            file.state.as_str()
        )));
    }
    if file.rel_path.contains('#') {
        return Err(RenameError::Refused(
            "a file inside a zip cannot be renamed in place".into(),
        ));
    }
    let entry = entry.ok_or(RenameError::NotFound)?;
    if entry.is_bios() {
        return Err(RenameError::Refused(BIOS_REFUSED.into()));
    }
    let rom = entry
        .roms
        .iter()
        .find(|r| Some(r.id) == file.rom_id)
        .cloned()
        .ok_or_else(|| RenameError::Refused("the file's rom is retired from its DAT".into()))?;
    Ok((file, entry, rom))
}

/// The library path the adapter gives `file`, refusing a plan that needs
/// more than a rename.
async fn canonical_path(
    file: &FileRow,
    entry: &TitleEntry,
    rom: &EntryRom,
    games: &Path,
) -> std::result::Result<PathBuf, RenameError> {
    let adapter = adapter_for(&file.platform_id)
        .ok_or_else(|| RenameError::Refused("the file's platform has no adapter".into()))?;
    let current = games.join(&file.rel_path);
    let path = current.clone();
    let head = tokio::task::spawn_blocking(move || read_head(&path, None))
        .await
        .map_err(|e| Error::Task(e.to_string()))?
        .map_err(|e| RenameError::Refused(format!("cannot read the file: {e}")))?;
    let staged = StagedFile {
        path: current,
        size: u64::try_from(file.size).unwrap_or(0),
        kind: StagedKind::File,
        head,
        members: Vec::new(),
    };
    let dat = DatEntry {
        name: entry.name.clone(),
        roms: vec![dat_rom(rom)],
    };
    let plan = adapter
        .plan_placement(&dat, &staged)
        .map_err(|e| RenameError::Refused(format!("cannot compute the canonical name: {e}")))?;
    let mut target = None;
    for step in &plan.steps {
        match step {
            Step::Rename { to, .. } => target = Some(to.clone()),
            Step::CreateDir { .. } => {}
            _ => {
                return Err(RenameError::Refused(
                    "the file needs more than a new name to load; import it again instead".into(),
                ))
            }
        }
    }
    target.ok_or_else(|| RenameError::Refused("the adapter gave no name".into()))
}

/// Gives a `misnamed` file of title group `group` its canonical name in place,
/// as the platform's adapter computes it, and returns the file's new path.
///
/// # Errors
///
/// [`RenameError`] naming why the file was not renamed.
pub async fn rename(
    app: &AppState,
    group: TitleId,
    file_id: FileId,
) -> std::result::Result<String, RenameError> {
    let (file, entry, rom) = rename_subject(app, group, file_id).await?;
    let games = app.config().paths.games;
    let to = canonical_path(&file, &entry, &rom, &games).await?;
    let (from_rel, to_rel) = (file.rel_path.clone(), rel_string(&to));
    if from_rel == to_rel {
        return Err(RenameError::Refused(
            "the file already has its canonical name".into(),
        ));
    }
    let (g, f, t) = (games.clone(), PathBuf::from(&from_rel), to.clone());
    let moved = tokio::task::spawn_blocking(move || {
        place::rename_in_library(&g, &f, &t).and_then(|dst| {
            fs::metadata(&dst)
                .map(|m| stat(&m))
                .map_err(|source| PlaceError::Io { path: dst, source })
        })
    })
    .await
    .map_err(|e| Error::Task(e.to_string()))?;
    let (_, mtime) = match moved {
        Ok(s) => s,
        Err(PlaceError::Exists(_)) => return Err(RenameError::Conflict(to_rel)),
        Err(e) => return Err(RenameError::Refused(e.to_string())),
    };
    let (to_db, from_db, title) = (to_rel.clone(), from_rel.clone(), entry.id);
    app.db
        .write(move |c| {
            let tx = c.transaction()?;
            let now = crate::unix_now();
            files::move_to(&tx, file_id, &to_db, FileState::Verified, mtime, now)?;
            let detail = json!({ "from": from_db, "rel_path": to_db, "title_id": title.0 });
            imports::log(
                &tx,
                now,
                None,
                Some(file_id.0),
                ImportAction::Renamed,
                &detail,
            )?;
            tx.commit()?;
            Ok(())
        })
        .await?;
    app.events.publish(
        EventKind::ImportDone,
        &json!({ "title_id": entry.id.0, "file_id": file_id.0, "action": ImportAction::Renamed.as_str() }),
    );
    Ok(to_rel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn headers_parse_from_spaced_hex() {
        assert_eq!(parse_header("4e45531a"), Some(b"NES\x1a".to_vec()));
        assert_eq!(parse_header("4e4"), None);
        assert_eq!(parse_header(""), None);
        assert_eq!(parse_header("zz"), None);
    }

    #[test]
    fn staged_paths_stay_in_staging_and_fall_back_to_the_torrent_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let staging = dir.path().join("staging");
        let real = staging.join("ab/Real Name/NES/x.nes");
        fs::create_dir_all(real.parent().expect("parent")).expect("mkdir");
        fs::write(&real, b"x").expect("write");
        let recorded = staging.join("ab/Magnet Name/NES/x.nes");
        let recorded = recorded.to_string_lossy();
        assert_eq!(
            locate(&staging, "ab", &recorded, Some("NES/x.nes")),
            Some(real.clone())
        );
        assert_eq!(
            locate(&staging, "ab", &recorded, None),
            Some(staging.join("ab/Magnet Name/NES/x.nes"))
        );
        assert_eq!(
            locate(&staging, "ab", "Real Name/NES/x.nes", Some("NES/x.nes")),
            Some(real)
        );
        assert_eq!(locate(&staging, "ab", "/elsewhere/x.nes", None), None);
        assert_eq!(locate(&staging, "ab", "../../x.nes", None), None);
    }

    #[test]
    fn report_names_expected_and_actual_hashes() {
        let actual = hash_reader(Cursor::new(b"abc"), HeaderRule::None, None).expect("hash");
        let expected = EntryRom {
            id: 1,
            name: "Example Quest (USA).nes".into(),
            size: 3,
            crc32: Some("00000000".into()),
            md5: None,
            sha1: None,
            status: "good".into(),
            header: None,
        };
        let text = report(Some(&expected), &[(None, actual.clone())], "ines");
        assert!(text.contains("Expected: Example Quest (USA).nes (3 bytes)"));
        assert!(text.contains(&actual.sha1));
        assert!(text.contains("md5 -"));
        assert!(text.ends_with("Header rule: ines\n"));
        let unknown = report(None, &[(Some("a.bin".into()), actual)], "none");
        assert!(unknown.contains("Actual a.bin: 3 bytes"));
    }

    #[test]
    fn helpers_name_paths_and_rules() {
        assert_eq!(
            rel_string(Path::new("NES/Example Quest (USA).nes")),
            "NES/Example Quest (USA).nes"
        );
        assert!(is_zip(Path::new("a.ZIP")));
        assert!(!is_zip(Path::new("a.nes")));
        assert_eq!(header_rule("n64"), HeaderRule::N64);
        assert_eq!(header_rule("none"), HeaderRule::None);
    }

    #[test]
    fn hash_item_and_read_head_cover_files_and_zips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let plain = dir.path().join("a.bin");
        fs::write(&plain, b"abcdefghijklmnopqrstuvwxyz").expect("write");
        let hashed = hash_item(&plain, HeaderRule::None).expect("hash");
        assert_eq!(hashed.len(), 1);
        assert_eq!(hashed[0].1.size, 26);
        assert_eq!(read_head(&plain, None).expect("head").len(), 16);
        let zipped = dir.path().join("a.zip");
        let mut z = zip::ZipWriter::new(File::create(&zipped).expect("create"));
        z.add_directory("d/", zip::write::SimpleFileOptions::default())
            .expect("dir");
        z.start_file("d/a.bin", zip::write::SimpleFileOptions::default())
            .expect("start");
        std::io::Write::write_all(&mut z, b"abc").expect("write");
        z.finish().expect("finish");
        let hashed = hash_item(&zipped, HeaderRule::None).expect("hash");
        assert_eq!(hashed.len(), 1);
        assert_eq!(hashed[0].0.as_deref(), Some("d/a.bin"));
        assert_eq!(read_head(&zipped, Some("d/a.bin")).expect("head"), b"abc");
        let text = "report";
        let staging = dir.path().join("staging");
        let q = quarantine(&staging, "0a0a", &plain, text).expect("quarantine");
        assert_eq!(q, staging.join("quarantine/0a0a/a.bin"));
        assert_eq!(
            fs::read_to_string(staging.join("quarantine/0a0a/a.bin.report.txt")).expect("read"),
            text
        );
    }
}
