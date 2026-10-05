//! One group's detail and the want and unwant writes; see `docs/API.md` "Titles".

use super::{RomStatus, Tag, TitleSource};
use crate::db::arcade::MraInfo;
use crate::db::candidates::Availability;
use crate::db::files::FileState;
use crate::db::ids::{DatVersionId, FileId, RomId, TitleId};
use crate::db::sql::{self};
use crate::error::Result;
use mistarr_core::PlatformId;
use rusqlite::{Connection, OptionalExtension, Row};
use serde::Serialize;

/// A rom of a variant with the best file on disk for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RomRow {
    /// `roms.id`.
    pub id: RomId,
    /// File name in the DAT.
    pub name: String,
    /// Size in bytes.
    pub size: u64,
    /// Lowercase hex CRC32.
    pub crc32: Option<String>,
    /// Lowercase hex MD5.
    pub md5: Option<String>,
    /// Lowercase hex SHA1.
    pub sha1: Option<String>,
    /// DAT status.
    pub status: RomStatus,
    /// The file matched to this rom, verified first.
    pub file_id: Option<FileId>,
    /// That file's state.
    pub file_state: Option<FileState>,
    /// That file's path relative to the platform's games directory.
    pub file_path: Option<String>,
}

/// One variant of a clone group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[allow(clippy::struct_excessive_bools)] // One flag per title column is the JSON shape.
pub struct VariantRow {
    /// `titles.id`.
    pub id: TitleId,
    /// Full DAT name.
    pub name: String,
    /// Region names.
    pub regions: Vec<String>,
    /// Language codes.
    pub languages: Vec<String>,
    /// Revision label.
    pub revision: Option<String>,
    /// Flag labels.
    pub flags: Vec<String>,
    /// The group's 1G1R pick.
    pub is_1g1r_pick: bool,
    /// Marked wanted.
    pub wanted: bool,
    /// No longer in the newest DAT version.
    pub retired: bool,
    /// Grouped by name rather than `cloneof`.
    pub inferred: bool,
    /// The DAT version the entry was last read from.
    pub dat_version_id: DatVersionId,
    /// Live roms.
    pub roms: Vec<RomRow>,
    /// Distinct files of bound sources in [`VariantRow::availability`].
    pub torrent_files_available: u64,
    /// Files of bound sources mapped to a live rom or a candidate for one, strongest first.
    pub availability: Vec<Availability>,
    /// Where the title was read from.
    pub source: TitleSource,
    /// MRA details, for an MRA title.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mra: Option<MraInfo>,
    /// Where the romset stands on disk, for a Neo Geo entry; filled by the HTTP layer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub romset: Option<RomsetState>,
}

/// A Neo Geo entry's romset on disk and in the core's `romsets.xml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RomsetState {
    /// Whether `romsets.xml` lists it; `None` when the file is absent.
    pub listed: Option<bool>,
    /// Whether `games/NeoGeo` holds it as a directory or zip.
    pub present: bool,
}

/// A clone group with every variant, retired ones last.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GroupDetail {
    /// Group root.
    pub parent_id: TitleId,
    /// Platform.
    pub platform_id: PlatformId,
    /// The parent's base name.
    pub base_name: String,
    /// The 1G1R pick, if any.
    pub pick_variant_id: Option<TitleId>,
    /// Every variant.
    pub variants: Vec<VariantRow>,
}

fn variant_row(r: &Row<'_>) -> rusqlite::Result<VariantRow> {
    Ok(VariantRow {
        id: r.get(0)?,
        name: r.get(1)?,
        regions: Vec::new(),
        languages: Vec::new(),
        revision: r.get(2)?,
        flags: Vec::new(),
        is_1g1r_pick: r.get(3)?,
        wanted: r.get(4)?,
        retired: r.get(5)?,
        inferred: r.get(6)?,
        dat_version_id: r.get(7)?,
        roms: Vec::new(),
        torrent_files_available: 0,
        availability: Vec::new(),
        source: r.get(8)?,
        mra: None,
        romset: None,
    })
}

/// Fills the regions, languages and flags of the variants of the group rooted at `gid`.
fn fill_lists(conn: &Connection, gid: TitleId, variants: &mut [VariantRow]) -> Result<()> {
    for tag in [Tag::Regions, Tag::Languages, Tag::Flags] {
        let (table, column) = tag.table();
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT x.title_id, x.{column} FROM titles t JOIN {table} x ON x.title_id = t.id
             WHERE t.group_root = ?1 OR (t.id = ?1 AND t.group_root IS NULL)
             ORDER BY x.title_id, x.pos"
        ))?;
        let mut rows = stmt.query([gid])?;
        while let Some(r) = rows.next()? {
            let (title, value): (TitleId, String) = (r.get(0)?, r.get(1)?);
            if let Some(v) = variants.iter_mut().find(|v| v.id == title) {
                match tag {
                    Tag::Regions => v.regions.push(value),
                    Tag::Languages => v.languages.push(value),
                    Tag::Flags => v.flags.push(value),
                }
            }
        }
    }
    Ok(())
}

/// The clone group containing title `id`, or `None` when there is no such title.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::titles::detail::group_detail;
/// use mistarr_server::db::ids::TitleId;
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert!(group_detail(&conn, TitleId(1)).unwrap().is_none());
/// ```
pub fn group_detail(conn: &Connection, id: TitleId) -> Result<Option<GroupDetail>> {
    let Some(gid) = super::browse::group_of(conn, id)? else {
        return Ok(None);
    };
    let Some((platform, base_name)): Option<(String, String)> = conn
        .query_row(
            "SELECT platform_id, base_name FROM titles WHERE id = ?1",
            [gid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
    else {
        return Ok(None);
    };
    let mut stmt = conn.prepare(
        "SELECT t.id, t.name, t.revision, t.is_1g1r_pick,
                t.wanted, t.retired, t.inferred, t.dat_version_id, t.source
         FROM titles t WHERE t.group_root = ?1 OR (t.id = ?1 AND t.group_root IS NULL)
         ORDER BY t.retired, t.is_1g1r_pick DESC, t.name",
    )?;
    let mut variants: Vec<VariantRow> = stmt
        .query_map([gid], variant_row)?
        .collect::<rusqlite::Result<_>>()?;
    fill_lists(conn, gid, &mut variants)?;
    let mut stmt = conn.prepare(
        "SELECT r.title_id, r.id, r.name, r.size, r.crc32, r.md5, r.sha1, r.status,
                f.id, f.state, f.rel_path
         FROM titles t JOIN roms r ON r.title_id = t.id AND r.retired = 0
         LEFT JOIN files f ON f.id = (
           SELECT x.id FROM files x WHERE x.rom_id = r.id
           ORDER BY CASE x.state WHEN 'verified' THEN 0 WHEN 'misnamed' THEN 1
                                 WHEN 'bad' THEN 2 ELSE 3 END, x.id
           LIMIT 1)
         WHERE t.group_root = ?1 OR (t.id = ?1 AND t.group_root IS NULL)
         ORDER BY r.title_id, r.name",
    )?;
    let mut rows = stmt.query([gid])?;
    while let Some(r) = rows.next()? {
        let title: TitleId = r.get(0)?;
        let rom = RomRow {
            id: r.get(1)?,
            name: r.get(2)?,
            size: sql::get_u64(r, 3)?,
            crc32: r.get(4)?,
            md5: r.get(5)?,
            sha1: r.get(6)?,
            status: r.get(7)?,
            file_id: r.get(8)?,
            file_state: r.get(9)?,
            file_path: r.get(10)?,
        };
        if let Some(v) = variants.iter_mut().find(|v| v.id == title) {
            v.roms.push(rom);
        }
    }
    for (title, found) in crate::db::candidates::for_group(conn, gid)? {
        if let Some(v) = variants.iter_mut().find(|v| v.id == title) {
            let seen = |a: &Availability| {
                (a.source_id, a.file_index) == (found.source_id, found.file_index)
            };
            if !v.availability.iter().any(seen) {
                v.torrent_files_available += 1;
            }
            v.availability.push(found);
        }
    }
    for v in variants.iter_mut().filter(|v| v.source == TitleSource::Mra) {
        v.mra = crate::db::arcade::info(conn, v.id)?;
    }
    let pick_variant_id = variants.iter().find(|v| v.is_1g1r_pick).map(|v| v.id);
    Ok(Some(GroupDetail {
        parent_id: gid,
        platform_id: PlatformId(platform),
        base_name,
        pick_variant_id,
        variants,
    }))
}

/// Why a variant could not be marked wanted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WantRefused {
    /// No such title.
    Missing,
    /// The entry is retired.
    Retired,
    /// BIOS entries are never selectable.
    Bios,
}

/// Marks one variant wanted.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::titles::detail::{want, WantRefused};
/// use mistarr_server::db::ids::TitleId;
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert_eq!(want(&conn, TitleId(1)).unwrap(), Err(WantRefused::Missing));
/// ```
pub fn want(conn: &Connection, id: TitleId) -> Result<std::result::Result<(), WantRefused>> {
    let row: Option<(bool, bool)> = conn
        .query_row(
            "SELECT retired, EXISTS (SELECT 1 FROM title_flags f WHERE f.title_id = titles.id AND f.flag = 'bios')
             FROM titles WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match row {
        None => Ok(Err(WantRefused::Missing)),
        Some((true, _)) => Ok(Err(WantRefused::Retired)),
        Some((_, true)) => Ok(Err(WantRefused::Bios)),
        Some(_) => {
            conn.execute("UPDATE titles SET wanted = 1 WHERE id = ?1", [id])?;
            Ok(Ok(()))
        }
    }
}

/// Unmarks every variant of the group rooted at `parent` and cancels its
/// downloads that have not started. Returns how many variants were wanted.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::titles::detail::unwant_group;
/// use mistarr_server::db::ids::TitleId;
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert_eq!(unwant_group(&conn, TitleId(1), 0).unwrap(), 0);
/// ```
pub fn unwant_group(conn: &Connection, parent: TitleId, now: i64) -> Result<usize> {
    let n = conn.execute(
        "UPDATE titles SET wanted = 0 WHERE (group_root = ?1 OR (id = ?1 AND group_root IS NULL)) AND wanted = 1",
        [parent],
    )?;
    crate::db::downloads::cancel_unstarted_in_group(conn, parent, now)?;
    Ok(n)
}
