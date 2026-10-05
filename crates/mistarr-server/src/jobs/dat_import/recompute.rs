//! The recompute's passes: files of retired roms and unmatched files matched again from
//! their stored hashes, then the 1G1R picks; see `docs/VERIFICATION.md` "Matching stored hashes".

use mistarr_core::select::Prefs;
use mistarr_core::PlatformId;
use mistarr_mister::platforms::{self, Platform};
use rusqlite::Connection;

use crate::db::files;
use crate::db::ids::FileId;
use crate::db::titles;
use crate::db::Db;
use crate::error::Result;
use crate::jobs::matching::set_matches;
use crate::jobs::progress::Progress;

/// Files matched again per transaction by [`rematch_chunk`] and [`match_unmatched_chunk`].
pub(super) const REMATCH_CHUNK: u32 = 256;

/// Matches up to [`REMATCH_CHUNK`] files of retired roms on `platform` again, by their
/// stored hashes against live roms only, and returns how many it took. A file no live
/// rom lists becomes `unverified`; see [`set_matches`] for the states.
///
/// # Errors
///
/// [`Error::Db`] on SQLite failure.
pub(super) fn rematch_chunk(conn: &Connection, platform: &PlatformId) -> Result<usize> {
    let orphans = files::retired_matches(conn, platform, REMATCH_CHUNK)?;
    set_matches(conn, platform, &orphans)?;
    Ok(orphans.len())
}

/// One page of [`match_unmatched_chunk`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct UnmatchedChunk {
    /// Files read; fewer than [`REMATCH_CHUNK`] means the last page.
    pub(super) read: usize,
    /// The highest id read, the cursor for the next page.
    pub(super) last: FileId,
    /// Files that went from no rom to a rom.
    pub(super) matched: usize,
}

/// Matches up to [`REMATCH_CHUNK`] unmatched files on `platform` with an id above
/// `after` against live roms by their stored hashes, without reading the files. Paging
/// by id reads a file that stays unmatched once per run.
///
/// # Errors
///
/// [`Error::Db`] on SQLite failure.
pub(super) fn match_unmatched_chunk(
    conn: &Connection,
    platform: &PlatformId,
    after: FileId,
) -> Result<UnmatchedChunk> {
    let rows = files::unmatched_after(conn, platform, after, REMATCH_CHUNK)?;
    let matched = set_matches(conn, platform, &rows)?;
    Ok(UnmatchedChunk {
        read: rows.len(),
        last: rows.last().map_or(after, |f| f.id),
        matched,
    })
}

/// A step of a recompute: matching files of retired roms, matching unmatched files after
/// an id, which arcade skips, and picking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Pass {
    Retired,
    Unmatched(FileId),
    Picking,
    Done,
}

impl Pass {
    /// The live progress phase of the pass.
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Retired | Self::Unmatched(_) => "matching",
            Self::Picking | Self::Done => "picking",
        }
    }
}

/// What a recompute's passes have done so far.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Tally {
    pub(super) checked: usize,
    pub(super) matched: usize,
    pub(super) picked: titles::recompute::Recomputed,
}

/// Runs one chunk of `pass` over `platform` in the caller's transaction, one per chunk,
/// adding to `tally`, and returns the pass that follows.
pub(super) fn recompute_pass(
    tx: &Connection,
    platform: &PlatformId,
    prefs: &Prefs,
    pass: Pass,
    tally: &mut Tally,
) -> Result<Pass> {
    let next = match pass {
        Pass::Retired => {
            let taken = rematch_chunk(tx, platform)?;
            tally.checked += taken;
            if taken >= REMATCH_CHUNK as usize {
                Pass::Retired
            } else if platforms::by_id(platform.as_str()).is_some_and(Platform::is_arcade) {
                // Arcade files are matched by the arcade catalogue's presence pass.
                Pass::Picking
            } else {
                Pass::Unmatched(FileId::new(0))
            }
        }
        Pass::Unmatched(after) => {
            let chunk = match_unmatched_chunk(tx, platform, after)?;
            tally.checked += chunk.read;
            tally.matched += chunk.matched;
            if chunk.read < REMATCH_CHUNK as usize {
                Pass::Picking
            } else {
                Pass::Unmatched(chunk.last)
            }
        }
        Pass::Picking => {
            tally.picked = titles::recompute::recompute_platform(tx, platform, prefs)?;
            Pass::Done
        }
        Pass::Done => Pass::Done,
    };
    Ok(next)
}

/// [`Recompute`]'s passes over `platform` on the calling thread, with `check` and then
/// `report` before each chunk; the caller queues the re-map.
pub(super) fn recompute_blocking(
    db: &Db,
    platform: &PlatformId,
    prefs: &Prefs,
    check: &dyn Fn() -> Result<()>,
    report: &dyn Fn(Pass, &Tally),
) -> Result<Tally> {
    let mut tally = Tally::default();
    let mut pass = Pass::Retired;
    while pass != Pass::Done {
        check()?;
        report(pass, &tally);
        pass = db.write_tx_blocking(|tx| recompute_pass(tx, platform, prefs, pass, &mut tally))?;
    }
    Ok(tally)
}

/// A recompute's live progress before `pass`.
pub(super) fn pass_progress(pass: Pass, tally: &Tally) -> Progress {
    match pass {
        Pass::Retired | Pass::Unmatched(_) => Progress::phase("matching")
            .with("checked", tally.checked)
            .with("matched", tally.matched),
        Pass::Picking | Pass::Done => Progress::phase("picking").with("matched", tally.matched),
    }
}
