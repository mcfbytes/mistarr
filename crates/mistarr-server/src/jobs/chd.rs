//! CHD disc images: the scan's header-only path, the `chd_tracks` job that decodes them, and
//! their classification by tracks. See `docs/VERIFICATION.md` "CHD images" and `docs/CHD.md`.

use std::collections::BTreeSet;
use std::fs::File;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mistarr_core::chd::{self as core, ChdError, ChdId, Decoder, Header, Layout, Step};
use mistarr_core::hash::{hash_reader, HeaderRule};
use mistarr_core::{HashSet, PlatformId};
use mistarr_mister::launch::split_chd_member;
use rusqlite::Connection;
use serde_json::json;
use tokio::time::Instant;

use super::scan::{self, Track};
use super::{Job, JobContext, Lane, Scheduler};
use crate::app::AppState;
use crate::db::chd::{self as rows, Unidentified};
use crate::db::files::{self, FileId, FileRow, FileState, NewFile, RomMatch};
use crate::db::settings::{self, keys};
use crate::error::{Error, Result};
use crate::threads::{self, label};

/// `jobs.kind` of [`ChdTracks`].
pub const KIND: &str = "chd_tracks";

/// Hunks decoded between two checkpoints: about 600 KiB, a fraction of a second on the board.
const SLICE_HUNKS: u32 = 32;

/// Waiting rows read at a time.
const PAGE: u32 = 16;

/// Least time between two live progress reports.
const PROGRESS_EVERY: Duration = Duration::from_secs(2);

/// `files.header_rule` of a CHD's container and member rows.
const HEADER_RULE: &str = "chd";

/// What the scan records for one `.chd` file.
pub(crate) struct ScannedChd {
    /// The container row, or the member rows of an identified image.
    pub(crate) rows: Vec<NewFile>,
    /// Every path the rows cover, kept from the prune.
    pub(crate) seen: Vec<String>,
    /// The file hashed whole, when a loaded DAT lists it as a `.chd` rom.
    pub(crate) whole: Option<Track>,
}

/// Records one `.chd` file for the scan. A file of the size of a whole-file `.chd` rom is
/// hashed whole first; otherwise only its 124-byte header is read, and its rows come from
/// the track cache, a stored failure, or a container row waiting to be decoded.
///
/// # Errors
///
/// [`Error::Db`] on SQLite failure, [`Error::Task`] when a blocking task fails.
pub(crate) async fn scan_file(
    ctx: &JobContext,
    platform: &PlatformId,
    rel_path: &str,
    path: &Path,
) -> Result<ScannedChd> {
    let bare = |rows| ScannedChd {
        rows,
        seen: vec![rel_path.to_owned()],
        whole: None,
    };
    let (size, mtime) = match scan::file_meta(path) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "cannot read metadata; marking unidentified");
            return Ok(bare(vec![container(
                rel_path,
                0,
                0,
                Unidentified::Unreadable,
            )]));
        }
    };
    let pid = platform.clone();
    if ctx
        .app
        .db
        .read(move |c| files::chd_rom_sized(c, &pid, size))
        .await?
    {
        if let Some(track) = whole_file(ctx, platform, rel_path, path, size, mtime).await? {
            return Ok(ScannedChd {
                rows: Vec::new(),
                seen: vec![rel_path.to_owned()],
                whole: Some(track),
            });
        }
    }
    let owned = path.to_path_buf();
    let read = threads::blocking(label::CHD_HEADER, move || {
        File::open(&owned)
            .map_err(ChdError::from)
            .and_then(core::read_header)
    })
    .await
    .map_err(|e| Error::Task(e.to_string()))?;
    let header = match read {
        Ok(h) => h,
        Err(e) => {
            tracing::debug!(path = %path.display(), error = %e, "CHD header not usable");
            let reason = e
                .reason()
                .map_or(Unidentified::Unreadable, Unidentified::Chd);
            return Ok(bare(vec![container(rel_path, size, mtime, reason)]));
        }
    };
    let id = header.id(u64::try_from(size).unwrap_or(0));
    let (pid, rel) = (platform.clone(), rel_path.to_owned());
    let rows = ctx
        .app
        .db
        .read(move |c| known_rows(c, &pid, &rel, size, mtime, &id))
        .await?;
    let seen = rows.iter().map(|r| r.rel_path.clone()).collect();
    Ok(ScannedChd {
        rows,
        seen,
        whole: None,
    })
}

/// The rows of a CHD whose header was read: its members from the cache, its stored failure,
/// or a container row `pending`, which [`settle`] resolves against the setting when written.
fn known_rows(
    conn: &Connection,
    platform: &PlatformId,
    rel_path: &str,
    size: i64,
    mtime: i64,
    id: &ChdId,
) -> Result<Vec<NewFile>> {
    if let Some(tracks) = rows::cached_tracks(conn, id)? {
        let m = ChdMembers {
            container: rel_path,
            size,
            mtime,
            tracks: &tracks,
        };
        return classify_chd(conn, platform, &m);
    }
    if let Some((reason, decoder)) = rows::failure(conn, id)? {
        if decoder >= core::DECODER_VERSION {
            let row = container(rel_path, size, mtime, Unidentified::Chd(reason));
            return Ok(vec![row]);
        }
    }
    // A layout no DAT had is checked again after a DAT load, not on every scan.
    let unchanged_no_layout = files::find_by_path(conn, platform, rel_path)?.is_some_and(|r| {
        r.size == size
            && r.mtime == mtime
            && r.state == FileState::Unidentified
            && r.reason.as_deref() == Some(Unidentified::NoLayout.as_str())
    });
    let reason = if unchanged_no_layout {
        Unidentified::NoLayout
    } else {
        Unidentified::Pending
    };
    Ok(vec![container(rel_path, size, mtime, reason)])
}

/// Hashes a `.chd` whole, as a DAT listing `.chd` roms expects; `None` when the hash
/// matches no rom or the file cannot be read.
async fn whole_file(
    ctx: &JobContext,
    platform: &PlatformId,
    rel_path: &str,
    path: &Path,
    size: i64,
    mtime: i64,
) -> Result<Option<Track>> {
    let (pid, rel) = (platform.clone(), rel_path.to_owned());
    let cached = ctx
        .app
        .db
        .read(move |c| scan::cached_hashes(c, &pid, &rel, size, mtime))
        .await?;
    let hashes = if let Some(h) = cached {
        h
    } else {
        let (owned, hint) = (path.to_path_buf(), u64::try_from(size).unwrap_or(0));
        let hashed = threads::blocking(label::HASH, move || {
            File::open(&owned).and_then(|f| hash_reader(f, HeaderRule::None, Some(hint)))
        })
        .await
        .map_err(|e| Error::Task(e.to_string()))?;
        match hashed {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "cannot hash CHD whole");
                return Ok(None);
            }
        }
    };
    let (pid, h) = (platform.clone(), hashes.clone());
    let matched = ctx
        .app
        .db
        .read(move |c| {
            let size = i64::try_from(h.size).unwrap_or(i64::MAX);
            files::match_rom(c, &pid, &h.sha1, &h.md5, &h.crc32, size)
        })
        .await?;
    Ok(matched.map(|m| Track {
        rel_path: rel_path.to_owned(),
        name: files::basename(rel_path).to_owned(),
        size,
        mtime,
        hashes: Some(hashes),
        matched: Some(m),
    }))
}

/// The container row of a CHD that is not identified.
fn container(rel_path: &str, size: i64, mtime: i64, reason: Unidentified) -> NewFile {
    NewFile {
        rel_path: rel_path.to_owned(),
        size,
        mtime,
        crc32: None,
        md5: None,
        sha1: None,
        header_rule: Some(HEADER_RULE.to_owned()),
        rom_id: None,
        state: FileState::Unidentified,
        reason: Some(reason.as_str().to_owned()),
    }
}

/// The reason a container row is written with while the setting is `on`: `pending` and
/// `off` follow it, and nothing waits while it is off.
///
/// ```
/// use mistarr_server::jobs::chd::settle;
/// assert_eq!(settle("off", true), "pending");
/// assert_eq!(settle("no_layout", false), "off");
/// assert_eq!(settle("cooked", false), "cooked");
/// ```
#[must_use]
pub fn settle(reason: &str, on: bool) -> &str {
    match Unidentified::parse(reason) {
        Some(Unidentified::Pending | Unidentified::Off) if on => Unidentified::Pending.as_str(),
        Some(Unidentified::Pending | Unidentified::Off | Unidentified::NoLayout) if !on => {
            Unidentified::Off.as_str()
        }
        _ => reason,
    }
}

/// An identified CHD: its container path, size and mtime, and its tracks' hashes in order.
pub(crate) struct ChdMembers<'a> {
    pub(crate) container: &'a str,
    pub(crate) size: i64,
    pub(crate) mtime: i64,
    pub(crate) tracks: &'a [HashSet],
}

/// Whether `name` is a cue sheet rom.
fn is_cue(name: &str) -> bool {
    files::basename(name).to_ascii_lowercase().ends_with(".cue")
}

/// The member rows of an identified CHD: `#NN` per track, plus `#cue`, `#cue2`… when one DAT
/// entry has a distinct rom for every track. A bad dump among them makes those tracks `bad`
/// and the rest `unverified`; with no such entry every track is `unverified`.
pub(crate) fn classify_chd(
    conn: &Connection,
    platform: &PlatformId,
    m: &ChdMembers<'_>,
) -> Result<Vec<NewFile>> {
    let mut cands = Vec::with_capacity(m.tracks.len());
    for t in m.tracks {
        cands.push(files::roms_matching(conn, platform, t)?);
    }
    let mut out = Vec::with_capacity(m.tracks.len() + 1);
    match winner(conn, &cands)? {
        Some((assigned, cues)) => {
            let bad = assigned.iter().any(|r| r.status == "baddump");
            for (i, (t, r)) in m.tracks.iter().zip(&assigned).enumerate() {
                let state = match (bad, r.status == "baddump") {
                    (false, _) => FileState::Verified,
                    (true, true) => FileState::Bad,
                    (true, false) => FileState::Unverified,
                };
                out.push(member(m, i, t, Some(r.rom_id), state));
            }
            if !bad {
                for (n, cue) in cues.iter().enumerate() {
                    out.push(cue_row(m, n, cue.rom_id));
                }
            }
        }
        None => {
            for (i, (t, c)) in m.tracks.iter().zip(&cands).enumerate() {
                let rom = c.first().map(|r| r.rom_id);
                out.push(member(m, i, t, rom, FileState::Unverified));
            }
        }
    }
    Ok(out)
}

/// The first title, by id, whose non-cue roms number the tracks and give each track a
/// distinct candidate, lowest rom id first; its track roms in track order and its cues.
fn winner(
    conn: &Connection,
    cands: &[Vec<RomMatch>],
) -> Result<Option<(Vec<RomMatch>, Vec<RomMatch>)>> {
    let Some(first) = cands.first() else {
        return Ok(None);
    };
    let mut titles: Vec<i64> = first.iter().map(|r| r.title_id).collect();
    titles.sort_unstable();
    titles.dedup();
    titles.retain(|t| cands.iter().all(|c| c.iter().any(|r| r.title_id == *t)));
    for title in titles {
        let (cues, tracks): (Vec<RomMatch>, Vec<RomMatch>) = files::disc_roms(conn, title)?
            .into_iter()
            .partition(|r| is_cue(&r.name));
        if tracks.len() != cands.len() {
            continue;
        }
        let mut assigned: Vec<RomMatch> = Vec::with_capacity(cands.len());
        for c in cands {
            let pick = c
                .iter()
                .filter(|r| {
                    tracks.iter().any(|t| t.rom_id == r.rom_id)
                        && !assigned.iter().any(|a| a.rom_id == r.rom_id)
                })
                .min_by_key(|r| r.rom_id);
            match pick {
                Some(r) => assigned.push(r.clone()),
                None => break,
            }
        }
        if assigned.len() == cands.len() {
            return Ok(Some((assigned, cues)));
        }
    }
    Ok(None)
}

fn member(
    m: &ChdMembers<'_>,
    i: usize,
    t: &HashSet,
    rom: Option<i64>,
    state: FileState,
) -> NewFile {
    NewFile {
        rel_path: format!("{}#{:02}", m.container, i + 1),
        size: i64::try_from(t.size).unwrap_or(i64::MAX),
        mtime: m.mtime,
        crc32: Some(t.crc32.clone()),
        md5: Some(t.md5.clone()),
        sha1: Some(t.sha1.clone()),
        header_rule: Some(HEADER_RULE.to_owned()),
        rom_id: rom,
        state,
        reason: None,
    }
}

fn cue_row(m: &ChdMembers<'_>, n: usize, rom: i64) -> NewFile {
    let suffix = if n == 0 {
        "cue".to_owned()
    } else {
        format!("cue{}", n + 1)
    };
    NewFile {
        rel_path: format!("{}#{suffix}", m.container),
        size: m.size,
        mtime: m.mtime,
        crc32: None,
        md5: None,
        sha1: None,
        header_rule: Some(HEADER_RULE.to_owned()),
        rom_id: Some(rom),
        state: FileState::Verified,
        reason: None,
    }
}

/// The track number of a CHD member row, `None` for a cue row or any other path.
///
/// ```
/// use mistarr_server::jobs::chd::track_number;
/// assert_eq!(track_number("PSX/G/g.chd#02"), Some(2));
/// assert_eq!(track_number("PSX/G/g.chd#cue"), None);
/// assert_eq!(track_number("PSX/G/g.bin"), None);
/// ```
#[must_use]
pub fn track_number(rel_path: &str) -> Option<usize> {
    let tail = split_chd_member(rel_path).1?;
    if tail.is_empty() || !tail.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    tail.parse().ok()
}

/// Splits a directory's rows into those the bin and cue rule classifies and the CHD
/// containers whose members are classified together; `unidentified` rows are in neither.
pub(crate) fn split_disc_rows(rows: Vec<FileRow>) -> (Vec<FileRow>, BTreeSet<String>) {
    let mut plain = Vec::with_capacity(rows.len());
    let mut containers = BTreeSet::new();
    for r in rows {
        if r.state == FileState::Unidentified {
            continue;
        }
        match split_chd_member(&r.rel_path) {
            (c, Some(_)) => {
                containers.insert(c.to_owned());
            }
            (_, None) => plain.push(r),
        }
    }
    (plain, containers)
}

/// Matches the member rows of CHD `container` again from their stored hashes, adding or
/// removing its cue rows; returns how many tracks went from no rom to a rom.
///
/// # Errors
///
/// [`Error::Db`] on SQLite failure.
pub(crate) fn rematch_container(
    conn: &Connection,
    platform: &PlatformId,
    container: &str,
    now: i64,
) -> Result<usize> {
    let members = files::zip_member_rows(conn, platform, container)?;
    let mut tracks: Vec<(usize, &FileRow)> = members
        .iter()
        .filter_map(|r| track_number(&r.rel_path).map(|n| (n, r)))
        .collect();
    tracks.sort_by_key(|(n, _)| *n);
    let numbered = tracks.iter().enumerate().all(|(i, (n, _))| *n == i + 1);
    let hashes: Option<Vec<HashSet>> = tracks.iter().map(|(_, r)| stored(r)).collect();
    let (Some(hashes), true, Some((_, first))) = (hashes, numbered, tracks.first()) else {
        return Ok(0);
    };
    let cue_size = members
        .iter()
        .find(|r| {
            split_chd_member(&r.rel_path)
                .1
                .is_some_and(|m| m.starts_with("cue"))
        })
        .map(|r| r.size);
    let size = match cue_size {
        Some(s) => s,
        None => rows::find_id(conn, &hashes)?.map_or(0, |id| i64::try_from(id.size).unwrap_or(0)),
    };
    let m = ChdMembers {
        container,
        size,
        mtime: first.mtime,
        tracks: &hashes,
    };
    let next = classify_chd(conn, platform, &m)?;
    let matched = tracks
        .iter()
        .zip(&next)
        .filter(|((_, old), new)| old.rom_id.is_none() && new.rom_id.is_some())
        .count();
    rows::replace_container(conn, platform, container, &next, now)?;
    Ok(matched)
}

fn stored(r: &FileRow) -> Option<HashSet> {
    Some(HashSet {
        size: u64::try_from(r.size).ok()?,
        crc32: r.crc32.clone()?,
        md5: r.md5.clone()?,
        sha1: r.sha1.clone()?,
    })
}

/// Moves waiting rows to follow `[scan] chd_tracks`, read inside the write, and queues
/// [`ChdTracks`] when it is on and a row waits; for a settings change and at startup.
///
/// # Errors
///
/// [`Error::Db`] when the rows cannot be updated or the job recorded.
pub async fn apply_setting(app: &Arc<AppState>) -> Result<()> {
    let state = Arc::clone(app);
    let (on, waiting) = app
        .db
        .write(move |c| {
            let on = state.config().scan.chd_tracks;
            rows::set_waiting(c, on)?;
            Ok((on, rows::has_waiting(c, None, &[Unidentified::Pending])?))
        })
        .await?;
    if on && waiting {
        Scheduler::enqueue(app, Arc::new(ChdTracks)).await?;
    }
    Ok(())
}

/// Queues [`ChdTracks`] when the setting is on and `platform` has a `pending` row, after
/// a scan; with `recheck`, after its DATs changed, its `no_layout` rows wait again first.
///
/// # Errors
///
/// [`Error::Db`] when the rows cannot be read or the job recorded.
pub async fn queue_for(app: &Arc<AppState>, platform: &PlatformId, recheck: bool) -> Result<()> {
    let disc = mistarr_mister::platforms::by_id(&platform.0)
        .is_some_and(|p| p.kind == mistarr_mister::Kind::Disc);
    if !disc || !app.config().scan.chd_tracks {
        return Ok(());
    }
    let pid = platform.clone();
    let waiting = app
        .db
        .write(move |c| {
            if recheck {
                rows::recheck_layouts(c, &pid)?;
            }
            rows::has_waiting(c, Some(&pid), &[Unidentified::Pending])
        })
        .await?;
    if waiting {
        Scheduler::enqueue(app, Arc::new(ChdTracks)).await?;
    }
    Ok(())
}

/// Decodes every `pending` CHD on the enabled disc platforms, one at a time, and records
/// its tracks; see `docs/ARCHITECTURE.md` "CHD identification".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChdTracks;

/// How one image ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Verified,
    Unmatched,
    NotIdentified,
    /// Left `pending`: it could not be read this time.
    Skipped,
    /// The setting was turned off.
    Stopped,
}

/// Counts for the final progress.
#[derive(Debug, Default)]
struct Tally {
    done: u64,
    verified: u64,
    unmatched: u64,
    not_identified: u64,
}

#[async_trait]
impl Job for ChdTracks {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn lane(&self) -> Lane {
        Lane::Heavy
    }

    async fn run(&self, ctx: &JobContext) -> Result<()> {
        let total = ctx.app.db.read(rows::waiting_count).await?;
        let mut tally = Tally::default();
        let mut live = Live::new(total);
        let mut after = FileId(0);
        'pages: loop {
            let page = ctx
                .app
                .db
                .read(move |c| rows::waiting(c, after, PAGE))
                .await?;
            if page.is_empty() {
                break;
            }
            for row in page {
                after = row.id;
                ctx.checkpoint().await?;
                if !enabled(&ctx.app) {
                    break 'pages;
                }
                let outcome = identify(ctx, &row, &mut live).await?;
                match outcome {
                    Outcome::Verified => tally.verified += 1,
                    Outcome::Unmatched => tally.unmatched += 1,
                    Outcome::NotIdentified => tally.not_identified += 1,
                    Outcome::Skipped => {}
                    Outcome::Stopped => break 'pages,
                }
                tally.done += 1;
                live.done = tally.done;
                if yield_lane(ctx).await? {
                    break 'pages;
                }
            }
        }
        ctx.progress(json!({
            "done": tally.done,
            "total": total,
            "verified": tally.verified,
            "unmatched": tally.unmatched,
            "not_identified": tally.not_identified,
        }))
        .await
    }
}

fn enabled(app: &AppState) -> bool {
    app.config().scan.chd_tracks
}

/// Hands the heavy lane to a queued job of another kind, queueing a fresh run behind it
/// when images still wait. Returns whether this run should end.
async fn yield_lane(ctx: &JobContext) -> Result<bool> {
    let (other, waiting) = ctx
        .app
        .db
        .read(|c| {
            let other = crate::db::jobs::queued_other_in_lane(c, Lane::Heavy.as_str(), KIND)?;
            Ok((other, rows::waiting_count(c)? > 0))
        })
        .await?;
    if !other {
        return Ok(false);
    }
    if waiting {
        Scheduler::enqueue(&ctx.app, Arc::new(ChdTracks)).await?;
    }
    Ok(true)
}

/// Throttled live progress.
struct Live {
    last: Option<Instant>,
    done: u64,
    total: u64,
}

impl Live {
    fn new(total: u64) -> Self {
        Self {
            last: None,
            done: 0,
            total,
        }
    }

    async fn report(&mut self, ctx: &JobContext, row: &FileRow, bytes: (u64, u64)) -> Result<()> {
        if self.last.is_some_and(|t| t.elapsed() < PROGRESS_EVERY) {
            return Ok(());
        }
        self.last = Some(Instant::now());
        ctx.progress(json!({
            "platform_id": row.platform_id.0,
            "done": self.done,
            "total": self.total,
            "file": files::basename(&row.rel_path),
            "bytes_done": bytes.0,
            "bytes_total": bytes.1,
        }))
        .await
    }
}

/// An image opened for decoding.
struct Ready {
    file: File,
    header: Header,
    layout: Layout,
    id: ChdId,
    size: i64,
    mtime: i64,
}

/// Opens `path` and reads its header and track list; the identity when the header was read.
fn open(path: &Path) -> std::result::Result<Ready, (Option<ChdId>, ChdError)> {
    let mut file = File::open(path).map_err(|e| (None, e.into()))?;
    let (size, mtime) = scan::file_meta(path).map_err(|e| (None, e.into()))?;
    let header = core::read_header(&mut file).map_err(|e| (None, e))?;
    let id = header.id(u64::try_from(size).unwrap_or(0));
    let layout = core::read_layout(&mut file, &header).map_err(|e| (Some(id), e))?;
    Ok(Ready {
        file,
        header,
        layout,
        id,
        size,
        mtime,
    })
}

/// Identifies one waiting image: cache, layout pre-filter, then a decode in slices.
async fn identify(ctx: &JobContext, row: &FileRow, live: &mut Live) -> Result<Outcome> {
    let path = ctx.app.config().paths.games.join(&row.rel_path);
    let opened = threads::blocking(label::CHD_HEADER, move || open(&path))
        .await
        .map_err(|e| Error::Task(e.to_string()))?;
    let ready = match opened {
        Ok(r) => r,
        Err((id, e)) => return failed(ctx, row, id, &e).await,
    };
    let id = ready.id;
    if let Some(tracks) = ctx
        .app
        .db
        .read(move |c| rows::cached_tracks(c, &id))
        .await?
    {
        return record(ctx, row, Opened::from(&ready), tracks, None).await;
    }
    let (pid, sizes) = (row.platform_id.clone(), ready.layout.track_sizes());
    if !ctx
        .app
        .db
        .read(move |c| rows::layout_known(c, &pid, &sizes))
        .await?
    {
        set_reason(ctx, row.id, Unidentified::NoLayout).await?;
        return Ok(Outcome::NotIdentified);
    }
    let Ready {
        file,
        header,
        layout,
        id,
        size,
        mtime,
    } = ready;
    let bytes_total = header.logical_bytes;
    let hunk_bytes = u64::from(header.hunk_bytes);
    let made = threads::blocking(label::CHD_DECODE, move || {
        Decoder::new(file, header, layout)
    })
    .await
    .map_err(|e| Error::Task(e.to_string()))?;
    let mut dec = match made {
        Ok(d) => d,
        Err(e) => return failed(ctx, row, Some(id), &e).await,
    };
    let mut active = Duration::ZERO;
    loop {
        ctx.checkpoint().await?;
        if !enabled(&ctx.app) {
            return Ok(Outcome::Stopped);
        }
        let started = Instant::now();
        let (d, step) = threads::blocking(label::CHD_DECODE, move || {
            let step = dec.step(SLICE_HUNKS);
            (dec, step)
        })
        .await
        .map_err(|e| Error::Task(e.to_string()))?;
        active += started.elapsed();
        dec = d;
        match step {
            Ok(Step::More { done, .. }) => {
                let bytes = (
                    done.saturating_mul(hunk_bytes).min(bytes_total),
                    bytes_total,
                );
                live.report(ctx, row, bytes).await?;
            }
            Ok(Step::Done) => break,
            Err(e) => return failed(ctx, row, Some(id), &e).await,
        }
    }
    let tracks = match dec.finish() {
        Ok(t) => t,
        Err(e) => return failed(ctx, row, Some(id), &e).await,
    };
    let secs = active.as_secs_f64().max(0.001);
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )] // A speed estimate.
    let rate = (bytes_total as f64 / secs) as u64;
    let image = Opened { id, size, mtime };
    record(ctx, row, image, tracks, Some(rate)).await
}

/// The identity, size and mtime of an image whose tracks are known.
struct Opened {
    id: ChdId,
    size: i64,
    mtime: i64,
}

impl From<&Ready> for Opened {
    fn from(r: &Ready) -> Self {
        Self {
            id: r.id,
            size: r.size,
            mtime: r.mtime,
        }
    }
}

/// Caches `tracks`, replaces the container row with its members, and stores the rate.
async fn record(
    ctx: &JobContext,
    row: &FileRow,
    image: Opened,
    tracks: Vec<HashSet>,
    rate: Option<u64>,
) -> Result<Outcome> {
    let Opened { id, size, mtime } = image;
    let (pid, container) = (row.platform_id.clone(), row.rel_path.clone());
    let written = ctx
        .app
        .db
        .write(move |c| {
            let tx = c.transaction()?;
            rows::store_tracks(&tx, &id, &tracks)?;
            let m = ChdMembers {
                container: &container,
                size,
                mtime,
                tracks: &tracks,
            };
            let members = classify_chd(&tx, &pid, &m)?;
            rows::replace_container(&tx, &pid, &container, &members, crate::unix_now())?;
            if let Some(rate) = rate {
                settings::set_json(&tx, keys::CHD_RATE, &rate)?;
            }
            crate::db::commit(tx)?;
            Ok(members)
        })
        .await?;
    Ok(if written.iter().any(|r| r.state == FileState::Verified) {
        Outcome::Verified
    } else {
        Outcome::Unmatched
    })
}

/// Records why an image cannot be identified, or leaves it `pending` after an I/O error.
async fn failed(
    ctx: &JobContext,
    row: &FileRow,
    id: Option<ChdId>,
    e: &ChdError,
) -> Result<Outcome> {
    let Some(reason) = e.reason() else {
        tracing::debug!(path = %row.rel_path, error = %e, "cannot read CHD; left pending");
        return Ok(Outcome::Skipped);
    };
    tracing::debug!(path = %row.rel_path, error = %e, "CHD not identified");
    if let Some(id) = id {
        ctx.app
            .db
            .write(move |c| rows::store_failure(c, &id, reason, crate::unix_now()))
            .await?;
    }
    set_reason(ctx, row.id, Unidentified::Chd(reason)).await?;
    Ok(Outcome::NotIdentified)
}

/// Moves a `pending` row to `to`, only while the setting, read inside the write, is on.
async fn set_reason(ctx: &JobContext, id: FileId, to: Unidentified) -> Result<()> {
    let app = Arc::clone(&ctx.app);
    ctx.app
        .db
        .write(move |c| {
            if enabled(&app) {
                rows::set_reason_if(c, id, Unidentified::Pending, to)?;
            }
            Ok(())
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let mut c = Connection::open_in_memory().expect("open");
        crate::db::migrate::apply(&mut c).expect("migrate");
        crate::db::platforms::seed(&mut c, &mistarr_mister::platforms::PLATFORMS).expect("seed");
        c
    }

    fn psx() -> PlatformId {
        PlatformId("psx".into())
    }

    fn track(n: u8) -> HashSet {
        HashSet {
            size: 2352 * u64::from(n),
            crc32: format!("{n:08x}"),
            md5: format!("{n:032x}"),
            sha1: format!("{n:040x}"),
        }
    }

    /// A title with a cue and one rom per `(name, hashes, status)`; the cue's id comes first.
    fn title(c: &Connection, name: &str, roms: &[(&str, HashSet, &str)]) -> Vec<i64> {
        let t = files::seed_title_fixture(c, &psx(), name).expect("title");
        let cue = HashSet {
            size: 90,
            ..track(200)
        };
        let cue_name = format!("{name}.cue");
        let mut ids =
            vec![files::seed_rom_for_title_fixture(c, t, &cue_name, &cue, "good").expect("cue")];
        for (rom, h, status) in roms {
            ids.push(files::seed_rom_for_title_fixture(c, t, rom, h, status).expect("rom"));
        }
        ids
    }

    fn classify(c: &Connection, tracks: &[HashSet]) -> Vec<NewFile> {
        let m = ChdMembers {
            container: "PSX/G/g.chd",
            size: 1000,
            mtime: 7,
            tracks,
        };
        classify_chd(c, &psx(), &m).expect("classify")
    }

    fn summary(rows: &[NewFile]) -> Vec<(String, Option<i64>, FileState)> {
        rows.iter()
            .map(|r| {
                let tail = r.rel_path.rsplit('#').next().unwrap_or_default();
                (tail.to_owned(), r.rom_id, r.state)
            })
            .collect()
    }

    #[test]
    fn a_complete_set_verifies_every_track_and_its_cue() {
        let c = conn();
        let ids = title(
            &c,
            "G",
            &[("g1.bin", track(1), "good"), ("g2.bin", track(2), "good")],
        );
        let rows = classify(&c, &[track(1), track(2)]);
        assert_eq!(
            summary(&rows),
            [
                ("01".to_owned(), Some(ids[1]), FileState::Verified),
                ("02".to_owned(), Some(ids[2]), FileState::Verified),
                ("cue".to_owned(), Some(ids[0]), FileState::Verified),
            ]
        );
        assert_eq!(rows[2].size, 1000, "the cue row carries the image's size");
        assert_eq!(rows[0].size, 2352);
        assert!(rows
            .iter()
            .all(|r| r.mtime == 7 && r.header_rule.as_deref() == Some("chd")));
    }

    #[test]
    fn identical_tracks_take_distinct_roms() {
        let c = conn();
        let ids = title(
            &c,
            "G",
            &[("g1.bin", track(3), "good"), ("g2.bin", track(3), "good")],
        );
        let rows = classify(&c, &[track(3), track(3)]);
        let rom_ids: Vec<_> = rows.iter().map(|r| r.rom_id).collect();
        assert_eq!(rom_ids, [Some(ids[1]), Some(ids[2]), Some(ids[0])]);
        assert!(rows.iter().all(|r| r.state == FileState::Verified));
    }

    #[test]
    fn a_bad_dump_leaves_the_set_without_a_cue() {
        let c = conn();
        title(
            &c,
            "G",
            &[
                ("g1.bin", track(1), "good"),
                ("g2.bin", track(2), "baddump"),
            ],
        );
        let rows = classify(&c, &[track(1), track(2)]);
        let states: Vec<_> = rows.iter().map(|r| r.state).collect();
        assert_eq!(states, [FileState::Unverified, FileState::Bad]);
    }

    #[test]
    fn an_incomplete_or_mismatched_set_stays_unverified() {
        let c = conn();
        let listed = [
            ("g1.bin", track(1), "good"),
            ("g2.bin", track(2), "good"),
            ("g3.bin", track(3), "good"),
        ];
        let ids = title(&c, "G", &listed);
        let rows = classify(&c, &[track(1), track(2)]);
        assert_eq!(
            summary(&rows),
            [
                ("01".to_owned(), Some(ids[1]), FileState::Unverified),
                ("02".to_owned(), Some(ids[2]), FileState::Unverified),
            ]
        );
        let rows = classify(&c, &[track(1), track(2), track(3), track(4)]);
        assert_eq!(rows.len(), 4, "no cue for a count mismatch");
        assert!(rows.iter().all(|r| r.state == FileState::Unverified));
        assert_eq!(rows[3].rom_id, None);
    }

    #[test]
    fn the_title_that_takes_every_track_wins() {
        let c = conn();
        title(
            &c,
            "Short",
            &[("s1.bin", track(1), "good"), ("s2.bin", track(2), "good")],
        );
        let listed = [
            ("l1.bin", track(1), "good"),
            ("l2.bin", track(2), "good"),
            ("l3.bin", track(3), "good"),
        ];
        let long = title(&c, "Long", &listed);
        let rows = classify(&c, &[track(1), track(2), track(3)]);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[2].rom_id, Some(long[3]));
        assert_eq!(rows[3].rom_id, Some(long[0]));
        assert!(rows.iter().all(|r| r.state == FileState::Verified));
    }

    #[test]
    fn split_keeps_chd_members_apart_and_drops_unidentified_rows() {
        let row = |rel: &str, state| FileRow {
            id: FileId(1),
            platform_id: psx(),
            rel_path: rel.to_owned(),
            size: 1,
            mtime: 1,
            crc32: None,
            md5: None,
            sha1: None,
            header_rule: None,
            rom_id: None,
            state,
            scanned_at: 1,
            reason: None,
        };
        let (plain, containers) = split_disc_rows(vec![
            row("PSX/G/g.cue", FileState::Verified),
            row("PSX/G/g.CHD#01", FileState::Unverified),
            row("PSX/G/g.CHD#cue", FileState::Verified),
            row("PSX/G/h.chd", FileState::Unidentified),
        ]);
        assert_eq!(plain.len(), 1);
        assert_eq!(plain[0].rel_path, "PSX/G/g.cue");
        assert_eq!(containers.into_iter().collect::<Vec<_>>(), ["PSX/G/g.CHD"]);
    }

    #[test]
    fn rematch_adds_the_cue_once_the_dat_lists_the_set() {
        let c = conn();
        let tracks = [track(1), track(2)];
        let id = ChdId {
            sha1: core::Sha1Digest([4; 20]),
            size: 1000,
        };
        rows::store_tracks(&c, &id, &tracks).expect("cache");
        let before = classify(&c, &tracks);
        rows::replace_container(&c, &psx(), "PSX/G/g.chd", &before, 1).expect("write");
        assert_eq!(
            rematch_container(&c, &psx(), "PSX/G/g.chd", 2).expect("rematch"),
            0
        );

        let ids = title(
            &c,
            "G",
            &[("g1.bin", track(1), "good"), ("g2.bin", track(2), "good")],
        );
        assert_eq!(
            rematch_container(&c, &psx(), "PSX/G/g.chd", 3).expect("rematch"),
            2
        );
        let cue = files::find_by_path(&c, &psx(), "PSX/G/g.chd#cue")
            .expect("find")
            .expect("cue row");
        assert_eq!(
            (cue.rom_id, cue.state, cue.size),
            (Some(ids[0]), FileState::Verified, 1000)
        );

        c.execute("UPDATE roms SET retired = 1", [])
            .expect("retire");
        rematch_container(&c, &psx(), "PSX/G/g.chd", 4).expect("rematch");
        let cue = files::find_by_path(&c, &psx(), "PSX/G/g.chd#cue").expect("find");
        assert!(cue.is_none(), "a retired set loses its cue row");
        let t1 = files::find_by_path(&c, &psx(), "PSX/G/g.chd#01")
            .expect("find")
            .expect("track");
        assert_eq!((t1.rom_id, t1.state), (None, FileState::Unverified));
    }
}
