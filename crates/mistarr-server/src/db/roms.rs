//! Rom lookups: the hash tiers of `docs/VERIFICATION.md` "Matching order" and the reads of
//! a rom, its title and its zip. `titles` and `arcade` write the rom rows.

use std::collections::HashMap;

use mistarr_core::{Crc32, Hashes, Md5, PlatformId, RomId, Sha1};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::Serialize;

use super::ids::{FileId, TitleId};

use super::sql;
use super::titles::RomStatus;
use crate::error::Result;

/// A rom a hashed file matched, per `docs/VERIFICATION.md` "Matching order".
#[derive(Debug, Clone, PartialEq)]
pub struct RomMatch {
    /// The matched `roms.id`.
    pub rom_id: RomId,
    /// The rom's owning `titles.id`.
    pub title_id: TitleId,
    /// The filename the DAT expects, compared against the file's own name.
    pub name: String,
    /// The rom's dump status.
    pub status: RomStatus,
    /// The owning title's name, which placement names the file after.
    pub game: String,
}

/// Which roms a tiered lookup admits and how many it returns.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// The best rom live or retired: live first, then a version not superseded.
    Best,
    /// The best live rom of a live title.
    BestLive,
    /// Every live rom of the first tier with a match, by rom id.
    AllLive,
}

/// Runs the SHA1, MD5, then CRC32-and-size tiers on one platform's DAT titles and returns
/// what `scope` admits from the first tier with a match.
fn match_tiers(
    conn: &Connection,
    platform_id: &PlatformId,
    (sha1, md5, crc32): (Option<Sha1>, Option<Md5>, Option<Crc32>),
    size: i64,
    scope: Scope,
) -> Result<Vec<RomMatch>> {
    let live = if scope == Scope::Best {
        ""
    } else {
        "r.retired = 0 AND t.retired = 0 AND "
    };
    let order = if scope == Scope::AllLive {
        " ORDER BY r.id"
    } else {
        " ORDER BY (r.retired = 0 AND t.retired = 0) DESC,
         d.superseded_by IS NULL DESC, d.id DESC LIMIT 1"
    };
    let select = format!(
        "SELECT r.id, r.title_id, r.name, r.status, t.name
         FROM roms r JOIN titles t ON t.id = r.title_id
         JOIN dat_versions d ON d.id = t.dat_version_id
         WHERE t.platform_id = ?1 AND t.source = 'dat' AND {live}"
    );
    let tiers: [(&str, Vec<&dyn rusqlite::ToSql>); 3] = [
        ("r.sha1 = ?2", vec![platform_id, &sha1]),
        ("r.sha1 IS NULL AND r.md5 = ?2", vec![platform_id, &md5]),
        (
            "r.sha1 IS NULL AND r.md5 IS NULL AND r.crc32 = ?2 AND r.size = ?3",
            vec![platform_id, &crc32, &size],
        ),
    ];
    for (cond, bound) in &tiers {
        let found: Vec<RomMatch> = conn
            .prepare_cached(&format!("{select}{cond}{order}"))?
            .query_map(bound.as_slice(), rom_match)?
            .collect::<rusqlite::Result<_>>()?;
        if !found.is_empty() {
            return Ok(found);
        }
    }
    Ok(Vec::new())
}

/// `docs/VERIFICATION.md` "Matching order", scoped to one platform and
/// preferring a live rom, then a title whose `dat_version` is not superseded.
/// Only DAT titles match; MRA roms are zips found by the arcade catalogue.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn match_rom(
    conn: &Connection,
    platform_id: &PlatformId,
    sha1: Option<Sha1>,
    md5: Option<Md5>,
    crc32: Option<Crc32>,
    size: i64,
) -> Result<Option<RomMatch>> {
    let found = match_tiers(conn, platform_id, (sha1, md5, crc32), size, Scope::Best)?;
    Ok(found.into_iter().next())
}

/// [`match_rom`] over live roms of live titles only.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let nes = mistarr_core::PlatformId::new("nes");
/// let crc = Some(mistarr_core::Crc32::from_u32(0));
/// assert!(mistarr_server::db::roms::match_live_rom(&conn, &nes, None, None, crc, 1).unwrap().is_none());
/// ```
pub fn match_live_rom(
    conn: &Connection,
    platform_id: &PlatformId,
    sha1: Option<Sha1>,
    md5: Option<Md5>,
    crc32: Option<Crc32>,
    size: i64,
) -> Result<Option<RomMatch>> {
    let found = match_tiers(conn, platform_id, (sha1, md5, crc32), size, Scope::BestLive)?;
    Ok(found.into_iter().next())
}

/// Every live rom of a live DAT title on `platform_id` that `hashes` matches at the first
/// tier with any match: SHA1, then MD5, then CRC32 and size; ordered by rom id.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let psx = mistarr_core::PlatformId::new("psx");
/// let h = mistarr_core::hash::hash_reader(&b"a"[..], Default::default(), None).unwrap();
/// assert!(mistarr_server::db::roms::roms_matching(&conn, &psx, &h).unwrap().is_empty());
/// ```
pub fn roms_matching(
    conn: &Connection,
    platform_id: &PlatformId,
    hashes: &Hashes,
) -> Result<Vec<RomMatch>> {
    let size = sql::to_i64(hashes.size);
    let by = (Some(hashes.sha1), Some(hashes.md5), Some(hashes.crc32));
    match_tiers(conn, platform_id, by, size, Scope::AllLive)
}

fn rom_match(r: &Row<'_>) -> rusqlite::Result<RomMatch> {
    Ok(RomMatch {
        rom_id: r.get(0)?,
        title_id: r.get(1)?,
        name: r.get(2)?,
        status: r.get(3)?,
        game: r.get(4)?,
    })
}

/// The live roms of title `title_id`, ordered by rom id, cue sheets included.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let title = mistarr_server::db::ids::TitleId::new(1);
/// assert!(mistarr_server::db::roms::disc_roms(&conn, title).unwrap().is_empty());
/// ```
pub fn disc_roms(conn: &Connection, title_id: TitleId) -> Result<Vec<RomMatch>> {
    Ok(conn
        .prepare_cached(
            "SELECT r.id, r.title_id, r.name, r.status, t.name FROM roms r JOIN titles t ON t.id = r.title_id
             WHERE r.title_id = ?1 AND r.retired = 0 AND t.retired = 0 ORDER BY r.id",
        )?
        .query_map([title_id], rom_match)?
        .collect::<rusqlite::Result<_>>()?)
}

/// Whether a live DAT rom on `platform_id` is a whole `.chd` file of `size` bytes, so a
/// `.chd` of that size is hashed whole before its header is read.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let psx = mistarr_core::PlatformId::new("psx");
/// assert!(!mistarr_server::db::roms::chd_rom_sized(&conn, &psx, 10).unwrap());
/// ```
pub fn chd_rom_sized(conn: &Connection, platform_id: &PlatformId, size: i64) -> Result<bool> {
    Ok(conn
        .prepare_cached(
            "SELECT EXISTS(
               SELECT 1 FROM roms r INDEXED BY roms_chd_size JOIN titles t ON t.id = r.title_id
               WHERE r.size = ?2 AND t.platform_id = ?1 AND t.source = 'dat'
                 AND r.retired = 0 AND t.retired = 0 AND lower(r.name) LIKE '%.chd')",
        )?
        .query_row(params![platform_id, size], |r| r.get(0))?)
}

/// Whether any rom of this platform has this CRC32 and size, the pre-check
/// that decides whether a zip member is worth decompressing.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn crc_candidate_exists(
    conn: &Connection,
    platform_id: &PlatformId,
    crc32: Crc32,
    size: i64,
) -> Result<bool> {
    Ok(conn
        .prepare_cached(
            "SELECT EXISTS(
           SELECT 1 FROM roms r JOIN titles t ON t.id = r.title_id
           WHERE t.platform_id = ?1 AND t.source = 'dat' AND r.crc32 = ?2 AND r.size = ?3)",
        )?
        .query_row(params![platform_id, crc32, size], |r| r.get(0))?)
}

/// Number of roms belonging to a title, for the disc all-or-nothing rule.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn count_roms_for_title(conn: &Connection, title_id: TitleId) -> Result<i64> {
    Ok(conn
        .prepare_cached("SELECT COUNT(*) FROM roms WHERE title_id = ?1")?
        .query_row([title_id], |r| r.get(0))?)
}

/// A rom with everything placement and the quarantine report need.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EntryRom {
    /// `roms.id`.
    pub id: RomId,
    /// File name in the DAT.
    pub name: String,
    /// Size in bytes.
    pub size: u64,
    /// CRC32, when the DAT lists it.
    pub crc32: Option<Crc32>,
    /// MD5, when the DAT lists it.
    pub md5: Option<Md5>,
    /// SHA1, when the DAT lists it.
    pub sha1: Option<Sha1>,
    /// DAT status.
    pub status: RomStatus,
    /// The DAT's `header` attribute, verbatim.
    pub header: Option<String>,
}

pub(crate) const ROM_COLUMNS: &str = "id, name, size, crc32, md5, sha1, status, header";

pub(crate) fn rom_row(r: &Row<'_>) -> rusqlite::Result<EntryRom> {
    Ok(EntryRom {
        id: r.get(0)?,
        name: r.get(1)?,
        size: sql::get_u64(r, 2)?,
        crc32: r.get(3)?,
        md5: r.get(4)?,
        sha1: r.get(5)?,
        status: r.get(6)?,
        header: r.get(7)?,
    })
}

/// One rom, live or retired.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn rom(conn: &Connection, id: RomId) -> Result<Option<EntryRom>> {
    Ok(conn
        .query_row(
            &format!("SELECT {ROM_COLUMNS} FROM roms WHERE id = ?1"),
            [id],
            rom_row,
        )
        .optional()?)
}

/// Whether rom `id` or its title is retired, as after its DAT was removed; false for none.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let rom = mistarr_core::RomId::new(1);
/// assert!(!mistarr_server::db::roms::rom_retired(&conn, rom).unwrap());
/// ```
pub fn rom_retired(conn: &Connection, id: RomId) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT r.retired = 1 OR t.retired = 1 FROM roms r JOIN titles t ON t.id = r.title_id
             WHERE r.id = ?1",
            [id],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(false))
}

/// The title owning rom `rom_id`.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn title_of_rom(conn: &Connection, rom_id: RomId) -> Result<Option<TitleId>> {
    Ok(conn
        .query_row("SELECT title_id FROM roms WHERE id = ?1", [rom_id], |r| {
            r.get(0)
        })
        .optional()?)
}

/// A live zip rom of an MRA title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredZip {
    /// Zip file name.
    pub name: String,
    /// Directory under `games/` it is read from.
    pub zip_dir: String,
    /// Whether it was on disk when last looked for.
    pub present: bool,
    /// Whether an MRA `<rom>` with an md5 names it.
    pub has_md5: bool,
}

/// The live zips of MRA title `id`, in the order they were first stored.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::{ids::TitleId, roms};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert!(roms::zip_roms(&conn, TitleId::new(1)).unwrap().is_empty());
/// ```
pub fn zip_roms(conn: &Connection, id: TitleId) -> Result<Vec<StoredZip>> {
    let mut stmt = conn.prepare_cached(
        "SELECT name, COALESCE(zip_dir, ''), present, md5 IS NOT NULL FROM roms
         WHERE title_id = ?1 AND retired = 0 ORDER BY id",
    )?;
    let rows = stmt.query_map([id], |r| {
        Ok(StoredZip {
            name: r.get(0)?,
            zip_dir: r.get(1)?,
            present: r.get(2)?,
            has_md5: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// A zip an MRA title names, as the importer places it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZipRom {
    /// The MRA title.
    pub title_id: TitleId,
    /// Zip file name.
    pub name: String,
    /// Directory under `games/` it is read from.
    pub zip_dir: String,
    /// The MRA file relative to `_Arcade`.
    pub mra_path: String,
}

/// Rom `rom_id` as a zip of an MRA title, or `None` when it is not one.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let rom = mistarr_core::RomId::new(1);
/// assert!(mistarr_server::db::roms::zip_rom(&conn, rom).unwrap().is_none());
/// ```
pub fn zip_rom(conn: &Connection, rom_id: RomId) -> Result<Option<ZipRom>> {
    Ok(conn
        .query_row(
            "SELECT t.id, r.name, COALESCE(r.zip_dir, ''), COALESCE(t.mra_path, '')
             FROM roms r JOIN titles t ON t.id = r.title_id
             WHERE r.id = ?1 AND t.source = 'mra'",
            [rom_id],
            |r| {
                Ok(ZipRom {
                    title_id: r.get(0)?,
                    name: r.get(1)?,
                    zip_dir: r.get(2)?,
                    mra_path: r.get(3)?,
                })
            },
        )
        .optional()?)
}

/// Every zip live MRA titles of `platform` name, keyed `{zip_dir}/{name}` in ASCII
/// lowercase (exFAT compares names that way), to every live `roms.id` naming it, ascending.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert!(mistarr_server::db::roms::live_zip_roms(&conn, &mistarr_core::PlatformId::new("arcade")).unwrap().is_empty());
/// ```
pub fn live_zip_roms(
    conn: &Connection,
    platform: &PlatformId,
) -> Result<HashMap<String, Vec<RomId>>> {
    let mut stmt = conn.prepare(
        "SELECT COALESCE(r.zip_dir, ''), r.name, r.id FROM roms r JOIN titles t ON t.id = r.title_id
         WHERE t.platform_id = ?1 AND t.source = 'mra' AND t.retired = 0 AND r.retired = 0
         ORDER BY r.id",
    )?;
    let mut rows = stmt.query([platform])?;
    let mut out: HashMap<String, Vec<RomId>> = HashMap::new();
    while let Some(r) = rows.next()? {
        let (dir, name, id): (String, String, RomId) = (r.get(0)?, r.get(1)?, r.get(2)?);
        out.entry(format!("{dir}/{name}").to_ascii_lowercase())
            .or_default()
            .push(id);
    }
    Ok(out)
}

/// A verified file of `rom`, if any.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn verified_file(conn: &Connection, rom: RomId) -> Result<Option<FileId>> {
    Ok(conn
        .query_row(
            "SELECT id FROM files WHERE rom_id = ?1 AND state = 'verified' ORDER BY id LIMIT 1",
            [rom],
            |r| r.get(0),
        )
        .optional()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::fixtures::{conn, dat, pid};
    use mistarr_core::Hashes;

    fn hashes(size: u64) -> Hashes {
        Hashes {
            size,
            crc32: "352441c2".parse().expect("hex"),
            md5: "900150983cd24fb0d6963f7d28e17f72".parse().expect("hex"),
            sha1: "a9993e364706816aba3e25717850c26c9cd0d89d"
                .parse()
                .expect("hex"),
        }
    }

    #[test]
    fn match_rom_prefers_sha1_then_md5_then_crc() {
        let c = conn();
        let nes = pid("nes");
        let h = hashes(3);
        dat(&nes)
            .title("Example Quest (USA)")
            .rom("Example Quest (USA).nes", &h, RomStatus::Good)
            .write(&c)
            .expect("seed");
        let m = match_rom(&c, &nes, Some(h.sha1), Some(h.md5), Some(h.crc32), 3)
            .expect("match")
            .expect("found");
        assert_eq!(m.name, "Example Quest (USA).nes");
        assert_eq!(m.status, RomStatus::Good);

        let other = dat(&nes)
            .title("Other Quest")
            .rom("Other Quest.nes", &hashes(4), RomStatus::Good)
            .write(&c)
            .expect("seed")
            .first_rom();
        c.execute(
            "UPDATE roms SET sha1 = NULL, md5 = ?2, crc32 = ?3 WHERE id = ?1",
            params![other, "1".repeat(32), "00000001"],
        )
        .expect("clear sha1");
        let wrong = Some(Sha1::from_bytes([0xde; 20]));
        let crc = |n| Some(Crc32::from_u32(n));
        let m = match_rom(
            &c,
            &nes,
            wrong,
            Some(Md5::from_bytes([0x11; 16])),
            crc(!0),
            4,
        )
        .expect("match")
        .expect("a wrong sha1 falls to the md5 of a rom with no sha1");
        assert_eq!(m.rom_id, other);
        c.execute("UPDATE roms SET md5 = NULL WHERE id = ?1", [other])
            .expect("clear md5");
        let m = match_rom(&c, &nes, wrong, None, crc(1), 4)
            .expect("match")
            .expect("crc32 and size match a rom with neither");
        assert_eq!(m.rom_id, other);
        assert!(match_rom(&c, &nes, wrong, None, crc(1), 5)
            .expect("match")
            .is_none());
    }

    #[test]
    fn match_rom_prefers_non_superseded_dat_version() {
        let c = conn();
        let nes = pid("nes");
        let h = hashes(3);
        let old = dat(&nes)
            .title("Old Pick")
            .rom("Old Pick.nes", &h, RomStatus::Good)
            .write(&c)
            .expect("old");
        let new = dat(&nes)
            .title("New Pick")
            .rom("New Pick.nes", &h, RomStatus::Good)
            .write(&c)
            .expect("new");
        c.execute(
            "UPDATE dat_versions SET superseded_by = ?1 WHERE id = ?2",
            [new.dat_version, old.dat_version],
        )
        .expect("supersede");
        let m = match_rom(&c, &nes, Some(h.sha1), Some(h.md5), Some(h.crc32), 3)
            .expect("match")
            .expect("found");
        assert_eq!(m.name, "New Pick.nes");
    }

    #[test]
    fn match_rom_prefers_a_live_rom_and_match_live_rom_takes_only_live_ones() {
        let c = conn();
        let nes = pid("nes");
        let h = hashes(3);
        let gone = dat(&nes)
            .title("Gone Quest")
            .rom("Gone Quest.nes", &h, RomStatus::Good)
            .write(&c)
            .expect("gone");
        c.execute(
            "UPDATE roms SET retired = 1 WHERE title_id = ?1",
            [gone.titles[0]],
        )
        .expect("retire");
        let any =
            || match_rom(&c, &nes, Some(h.sha1), Some(h.md5), Some(h.crc32), 3).expect("match");
        let live = || {
            match_live_rom(&c, &nes, Some(h.sha1), Some(h.md5), Some(h.crc32), 3).expect("match")
        };
        assert_eq!(any().expect("retired still matches").name, "Gone Quest.nes");
        assert!(live().is_none(), "a retired rom is not live");
        dat(&nes)
            .title("Live Quest")
            .rom("Live Quest.nes", &h, RomStatus::Good)
            .write(&c)
            .expect("live");
        assert_eq!(any().expect("live first").name, "Live Quest.nes");
        assert_eq!(live().expect("live").name, "Live Quest.nes");
    }

    #[test]
    fn crc_candidate_exists_needs_the_crc_and_size_of_a_dat_rom() {
        let c = conn();
        let nes = pid("nes");
        let h = hashes(3);
        dat(&nes)
            .title("Example Quest (USA)")
            .rom("a.nes", &h, RomStatus::Good)
            .write(&c)
            .expect("seed");
        assert!(crc_candidate_exists(&c, &nes, h.crc32, 3).expect("exists"));
        assert!(!crc_candidate_exists(&c, &nes, h.crc32, 4).expect("size"));
        assert!(!crc_candidate_exists(&c, &pid("snes"), h.crc32, 3).expect("platform"));
    }

    #[test]
    fn count_roms_for_title_counts_only_that_title() {
        let c = conn();
        let seeded = dat(&pid("psx"))
            .title("Disc Quest")
            .rom("Disc Quest.cue", &hashes(3), RomStatus::Good)
            .rom("Disc Quest (Track 1).bin", &hashes(10), RomStatus::Good)
            .title("Other Disc")
            .rom("Other Disc.cue", &hashes(3), RomStatus::Good)
            .write(&c)
            .expect("seed");
        assert_eq!(
            count_roms_for_title(&c, seeded.titles[0]).expect("count"),
            2
        );
    }

    #[test]
    fn roms_matching_returns_every_live_rom_of_the_first_tier() {
        let c = conn();
        let psx = pid("psx");
        let h = hashes(2352);
        let seeded = dat(&psx)
            .title("Disc A")
            .rom("a.bin", &h, RomStatus::Good)
            .title("Disc B")
            .rom("b.bin", &h, RomStatus::Good)
            .write(&c)
            .expect("seed");
        let (a, b) = (seeded.titles[0], seeded.titles[1]);
        let (ra, rb) = (seeded.roms[0], seeded.roms[1]);
        let ids = |hashes: &Hashes, platform: &PlatformId| -> Vec<RomId> {
            roms_matching(&c, platform, hashes)
                .expect("match")
                .iter()
                .map(|m| m.rom_id)
                .collect()
        };
        assert_eq!(ids(&h, &psx), [ra, rb]);
        c.execute("UPDATE roms SET retired = 1 WHERE id = ?1", [rb])
            .expect("retire");
        assert_eq!(ids(&h, &psx), [ra], "retired roms never match");
        let crc_only = Hashes {
            sha1: "f".repeat(40).parse().expect("hex"),
            md5: "e".repeat(32).parse().expect("hex"),
            ..h
        };
        assert!(ids(&crc_only, &psx).is_empty(), "sha1 roms need sha1");
        assert!(ids(&h, &pid("saturn")).is_empty());

        let names: Vec<String> = disc_roms(&c, a)
            .expect("roms")
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert_eq!(names, ["a.bin"]);
        assert!(
            disc_roms(&c, b).expect("roms").is_empty(),
            "a retired rom is gone"
        );
    }

    #[test]
    fn chd_rom_sized_finds_whole_file_chd_roms_by_size() {
        let c = conn();
        let psx = pid("psx");
        dat(&psx)
            .title("Disc")
            .rom("Disc.CHD", &hashes(4096), RomStatus::Good)
            .rom("Disc.bin", &hashes(8192), RomStatus::Good)
            .write(&c)
            .expect("seed");
        assert!(chd_rom_sized(&c, &psx, 4096).expect("sized"));
        assert!(!chd_rom_sized(&c, &psx, 8192).expect("a bin is not a chd"));
        assert!(!chd_rom_sized(&c, &pid("saturn"), 4096).expect("other platform"));
    }
}
