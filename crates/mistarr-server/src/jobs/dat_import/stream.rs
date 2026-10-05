//! Streaming one DAT of a file into `dat_stage` and applying it in one transaction; the
//! flow is `docs/ARCHITECTURE.md` "DAT import".

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Duration;

use mistarr_core::dat::{
    export_name, export_parents, split_version, DatGame, DatHeader, DatStream, ExportOptions,
};
use mistarr_core::hash::HeaderRule;
use mistarr_core::naming::{group_key, parse_name};

use super::{phase, room, stem, Loaded, Member, Meter, Outcome, Request, YIELD_FOR};
use crate::db::dat_stage::{self, StagedGame, StagedRom};
use crate::db::dats::{self, NewVersion};
use crate::db::titles;
use crate::db::Db;
use crate::error::{Error, Result};
use crate::jobs::progress::CountingReader;
use crate::jobs::Lane;

/// Games read between checks for shutdown.
pub(super) const CANCEL_EVERY: u64 = 500;

/// Games parsed between pauses while a core runs, so the parse never holds a CPU for long.
const YIELD_EVERY: u64 = 200;

/// How often a parse held by a manual pause looks again.
const PAUSED_POLL: Duration = Duration::from_millis(250);

/// Games parsed per write to `dat_stage`; the write lock is free between them.
pub(super) const STAGE_CHUNK: usize = 2000;

/// Opens a member for streaming and imports it, reading a DB export twice so its
/// clones are linked. Runs on a blocking thread.
pub(super) fn import_from(db: &Db, path: &Path, member: Member, req: &Request) -> Result<Outcome> {
    let meter = req.meter.as_ref();
    let parents = match with_member(path, member, ("indexing", meter), |r, name| {
        Ok(export_parents(r).map_err(|e| rejected(name, &e)))
    })? {
        Ok(Ok(p)) => p,
        Ok(Err(reason)) | Err(reason) => return Ok(Outcome::Rejected(reason)),
    };
    let streamed = with_member(path, member, ("reading", meter), |r, name| {
        import_stream(db, r, req, name, parents)
    });
    match streamed {
        Ok(Ok(outcome)) => Ok(outcome),
        Ok(Err(reason)) => Ok(Outcome::Rejected(reason)),
        Err(e) if disk_full(&e) => {
            // The stage may hold most of the DAT; give its space back before failing.
            if let Err(c) = db.write_blocking(|c| dat_stage::clear(c)) {
                tracing::warn!(error = %c, "cannot empty the DAT stage");
            }
            let tmp = std::env::var_os(crate::db::tempdir::SQLITE_TMPDIR).map(PathBuf::from);
            Err(Error::NoRoom(full_message(
                tmp.as_deref(),
                db.path(),
                crate::status::free_bytes,
            )))
        }
        Err(e) => Err(e),
    }
}

/// Whether `e` is SQLite's "database or disk is full".
pub(super) fn disk_full(e: &Error) -> bool {
    matches!(e, Error::Db(rusqlite::Error::SqliteFailure(f, _))
        if f.code == rusqlite::ErrorCode::DiskFull)
}

/// Why a load failed for lack of space, naming SQLite's temporary directory `tmp` when
/// it has less room left than the filesystem holding the database `db`.
pub(super) fn full_message(
    tmp: Option<&Path>,
    db: &Path,
    free: impl Fn(&Path) -> Option<u64>,
) -> String {
    let dir = db.parent().unwrap_or(db);
    match tmp {
        Some(t) if free(t).unwrap_or(0) <= free(dir).unwrap_or(u64::MAX) => format!(
            "{} is full: the staged DAT and SQLite's temporary files did not fit there; \
             the database is unchanged",
            t.display()
        ),
        _ => format!(
            "{} is full: the database could not grow; it is unchanged",
            dir.display()
        ),
    }
}

/// Runs `f` on a fresh buffered reader of `member` and its name in the zip, empty for a
/// plain file, reporting the bytes read in `phase` to the meter; `Err` holds why a zip
/// member cannot be opened.
fn with_member<T>(
    path: &Path,
    member: Member,
    (phase, meter): (&str, Option<&Meter>),
    f: impl FnOnce(&mut dyn BufRead, &str) -> Result<T>,
) -> Result<std::result::Result<T, String>> {
    let file = File::open(path)?;
    let report = |read: u64, total: u64| {
        if let Some(m) = meter {
            m.bytes(phase, read, total);
        }
    };
    match member {
        Member::Plain => {
            let total = file.metadata()?.len();
            let counted = CountingReader::new(file, |n| report(n, total));
            f(&mut BufReader::new(counted), "").map(Ok)
        }
        Member::Zip(index) => {
            let mut archive = match zip::ZipArchive::new(BufReader::new(file)) {
                Ok(a) => a,
                Err(e) => return Ok(Err(format!("invalid zip archive: {e}"))),
            };
            let entry = match archive.by_index(index) {
                Ok(e) => e,
                Err(e) => return Ok(Err(format!("invalid zip archive: {e}"))),
            };
            let name = entry.name().to_owned();
            let total = entry.size();
            let counted = CountingReader::new(entry, |n| report(n, total));
            let out = f(&mut BufReader::new(counted), &name);
            out.map(Ok)
        }
    }
}

/// A rejection reason, naming the zip member it came from.
fn rejected(member: &str, e: &dyn std::fmt::Display) -> String {
    if member.is_empty() {
        e.to_string()
    } else {
        format!("{member}: {e}")
    }
}

/// Streams one Logiqx DAT, or a DB export whose clones stay unlinked; see [`import_stream`].
#[cfg(test)]
pub(super) fn import_member<R: BufRead>(
    db: &Db,
    reader: R,
    req: &Request,
    member: &str,
) -> Result<Outcome> {
    import_stream(db, reader, req, member, None)
}

/// The DAT name and version a member is stored under, and the platform it loads into.
struct Identity {
    name: String,
    version: String,
    platform: Option<String>,
}

/// A DB export's identity: the name from the member's name, else the dropped file's,
/// else the member's or file's stem less a final version group, which becomes the
/// version; a plain file being bound keeps its stored name.
fn export_identity(req: &Request, member: &str) -> Identity {
    let named = [member, req.file_stem.as_str()]
        .into_iter()
        .filter(|n| !n.is_empty())
        .find_map(export_name);
    let (name, version) = match (&req.bind, named) {
        (Some(b), _) if member.is_empty() => (b.dat_name.clone(), b.dat_version.clone()),
        (_, Some(n)) => (n.dat_name, n.version),
        _ => {
            let stem = if member.is_empty() {
                req.file_stem.clone()
            } else {
                stem(Path::new(member))
            };
            let (name, version) = split_version(&stem);
            (name.to_owned(), version.to_owned())
        }
    };
    let platform = platform_for(req, &name);
    Identity {
        name,
        version,
        platform,
    }
}

/// A Logiqx DAT's identity: its header name, else the member's stem, else the file's.
fn logiqx_identity(req: &Request, member: &str, header: &DatHeader) -> Identity {
    let name = match (&req.bind, member) {
        _ if !header.name.trim().is_empty() => header.name.clone(),
        (_, m) if !m.is_empty() => stem(Path::new(m)),
        // A plain file holds one DAT; its stored name survives the rename into loaded/.
        (Some(b), _) => b.dat_name.clone(),
        (None, _) => req.file_stem.clone(),
    };
    let platform = platform_for(req, &name);
    Identity {
        name,
        version: header.version.clone(),
        platform,
    }
}

/// How a DB export's files become roms on `platform`: its header rule, the extensions it
/// loads, and the one it writes, else the first it loads.
pub(super) fn export_options(
    platform: Option<&str>,
    parents: HashMap<String, String>,
) -> ExportOptions {
    let row = platform.and_then(mistarr_mister::platforms::by_id);
    ExportOptions {
        header_rule: row.map_or(HeaderRule::None, |p| p.header_rule),
        extension: row
            .and_then(|p| {
                p.extension_written
                    .or_else(|| p.load_extensions.first().copied())
            })
            .map(str::to_owned),
        load_extensions: row
            .map(|p| p.load_extensions.iter().map(|e| (*e).to_owned()).collect())
            .unwrap_or_default(),
        parents,
    }
}

/// The platform a DAT named `dat_name` loads into: the one being bound, else the table's.
fn platform_for(req: &Request, dat_name: &str) -> Option<String> {
    match &req.bind {
        Some(b) => Some(b.platform.0.clone()),
        None => mistarr_mister::bind_dat_name(dat_name).map(|p| p.id.to_owned()),
    }
}

/// Opens the game stream of one DAT with the identity it is stored under; a DB export
/// is identified first, since its platform decides which files become roms.
fn open_stream<R: BufRead>(
    reader: R,
    req: &Request,
    member: &str,
    parents: Option<HashMap<String, String>>,
) -> std::result::Result<(DatStream<R>, Identity), mistarr_core::dat::DatError> {
    let Some(parents) = parents else {
        let stream = DatStream::new(reader)?;
        let id = logiqx_identity(req, member, stream.header());
        return Ok((stream, id));
    };
    let mut id = export_identity(req, member);
    let options = export_options(id.platform.as_deref(), parents);
    let stream = DatStream::with_options(reader, options)?;
    if !stream.header().version.trim().is_empty() {
        id.version.clone_from(&stream.header().version);
    }
    Ok((stream, id))
}

/// Streams one DAT into `dat_stage` in chunks of [`STAGE_CHUNK`] games, each
/// its own short write, then applies it in one transaction so readers see the
/// old titles or the new ones, never a mix. A parse error becomes
/// [`Outcome::Rejected`] and leaves the catalog untouched. `parents` is a DB
/// export's archive index, `None` for a Logiqx DAT.
pub(super) fn import_stream<R: BufRead>(
    db: &Db,
    reader: R,
    req: &Request,
    member: &str,
    parents: Option<HashMap<String, String>>,
) -> Result<Outcome> {
    let prefix = if member.is_empty() {
        String::new()
    } else {
        format!("{member}: ")
    };
    let (stream, id) = match open_stream(reader, req, member, parents) {
        Ok(opened) => opened,
        Err(e) => return Ok(Outcome::Rejected(format!("{prefix}{e}"))),
    };
    let Identity {
        name: dat_name,
        version,
        platform: bound,
    } = id;
    if let Some(b) = &req.bind {
        if b.dat_name != dat_name || b.dat_version != version {
            return Ok(Outcome::Skipped);
        }
    }
    let new = NewVersion {
        dat_name: &dat_name,
        version: &version,
        source_file: &req.source_file,
        platform: bound.as_deref(),
        now: req.now,
    };
    let newer = || {
        Outcome::Rejected(format!(
            "{prefix}a newer version of {dat_name} is loaded; bind that version instead"
        ))
    };
    let (planned, current) = db.read_blocking(|c| dats::plan_version(c, &new))?;
    if req.bind.is_some() && !current {
        return Ok(newer());
    }
    let staging = planned.is_some() && current;
    if staging {
        db.write_blocking(|c| dat_stage::clear(c))?;
    }
    let mut games = 0u64;
    let mut clone_of = false;
    let mut chunk = Vec::new();
    for game in stream {
        let game = match game {
            Ok(g) => g,
            Err(e) => {
                db.write_blocking(|c| dat_stage::clear(c))?;
                return Ok(Outcome::Rejected(format!("{prefix}{e}")));
            }
        };
        games += 1;
        if let Some(m) = &req.meter {
            m.games.store(games, Ordering::Relaxed);
        }
        pace(req, games)?;
        if staging {
            clone_of |= game.clone_of.is_some();
            chunk.push(staged(&game));
            if chunk.len() >= STAGE_CHUNK {
                db.write_tx_blocking(|tx| dat_stage::append(tx, &chunk))?;
                chunk.clear();
            }
        }
    }
    if !chunk.is_empty() {
        db.write_tx_blocking(|tx| dat_stage::append(tx, &chunk))?;
    }
    // Applying the stage grows the copy and SQLite's temporary files at once.
    room(req)?;
    db.write_bulk_blocking(|c| {
        // Hand-written, not `transact`: the not-current return drops `tx` to roll back
        // the upsert, which `transact` would commit.
        let tx = c.transaction()?;
        let plan = dats::upsert_version(&tx, &new)?;
        if req.bind.is_some() && !plan.current {
            return Ok(newer());
        }
        let platform = plan.platform_id.clone().filter(|_| plan.current && staging);
        if let Some(p) = &platform {
            phase(req, "storing");
            dats::begin_load(&tx, plan.id)?;
            dat_stage::apply(&tx, p, plan.id)?;
        }
        dats::set_game_count(&tx, plan.id, games)?;
        let mut retired = 0;
        if let Some(p) = &platform {
            titles::recompute::link_parents(&tx, plan.id, clone_of)?;
            retired = dats::retire_absent(&tx, plan.id)?;
            phase(req, "picking");
            titles::recompute::recompute_platform(&tx, p, &req.prefs)?;
        }
        dat_stage::clear(&tx)?;
        phase(req, "refreshing");
        crate::db::commit(tx)?;
        Ok(Outcome::Loaded(Loaded {
            version: plan.id,
            platform: plan.platform_id,
            games,
            has_titles: platform.is_some(),
            retired,
        }))
    })
}

/// Stops on shutdown, sleeps briefly while a core runs and waits out a
/// manual pause, which holds the background lane; checked every few games.
pub(super) fn pace(req: &Request, games: u64) -> Result<()> {
    if games.is_multiple_of(CANCEL_EVERY) {
        if *req.stop.borrow() {
            return Err(Error::Cancelled);
        }
        room(req)?;
    }
    if !games.is_multiple_of(YIELD_EVERY) {
        return Ok(());
    }
    if req.gate.borrow().core_running() {
        std::thread::sleep(YIELD_FOR);
    }
    while req.gate.borrow().hold(Lane::Background).is_some() {
        if *req.stop.borrow() {
            return Err(Error::Cancelled);
        }
        if req.abort_on_hold {
            return Err(Error::Paused);
        }
        std::thread::sleep(PAUSED_POLL);
    }
    Ok(())
}

/// Parses a game's name into the row its title is stored from; regions, languages and
/// stage flags the name lacks come from the DAT's own fields.
pub(super) fn staged(game: &DatGame) -> StagedGame {
    let parsed = parse_name(&game.name);
    let mut regions: Vec<String> = parsed.regions.iter().map(|r| r.name().to_owned()).collect();
    if regions.is_empty() {
        regions.clone_from(&game.regions);
    }
    let languages = if parsed.languages.is_empty() {
        game.languages.clone()
    } else {
        parsed.languages.clone()
    };
    let mut flags = parsed.flag_labels();
    for flag in game.status_flags() {
        if !parsed.flags.contains(&flag) {
            flags.push(flag.to_string());
        }
    }
    StagedGame {
        name: game.name.clone(),
        base_name: parsed.base_name.clone(),
        group_key: group_key(&parsed),
        clone_of: game.clone_of.clone(),
        regions,
        languages,
        revision: parsed.revision.as_ref().map(|r| r.label.clone()),
        flags,
        roms: game
            .roms
            .iter()
            .map(|r| StagedRom {
                name: r.name.clone(),
                size: r.size,
                crc32: r.crc32.clone(),
                md5: r.md5.clone(),
                sha1: r.sha1.clone(),
                status: r.status.into(),
                header: r.header.clone(),
            })
            .collect(),
    }
}
