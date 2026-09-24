//! MRA titles on the arcade platform: the catalogue the arcade job writes and the
//! md5 check it records; see `docs/DATA-MODEL.md` "MRA titles".

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use super::dats::DatVersionId;
use super::titles::TitleId;
use crate::error::Result;

/// The `dat_versions.dat_name` every MRA title hangs off; the row has `source = 'mra'`.
pub const MRA_DAT_NAME: &str = "_Arcade";

/// The `dat_versions` row MRA titles belong to, created on first use.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// mistarr_server::db::platforms::seed(&mut conn, &mistarr_mister::platforms::PLATFORMS).unwrap();
/// let v = mistarr_server::db::arcade::mra_version(&conn, "arcade", 1).unwrap();
/// assert_eq!(mistarr_server::db::arcade::mra_version(&conn, "arcade", 2).unwrap(), v);
/// ```
pub fn mra_version(conn: &Connection, platform: &str, now: i64) -> Result<DatVersionId> {
    let found: Option<i64> = conn
        .query_row(
            "SELECT id FROM dat_versions WHERE source = 'mra' AND platform_id = ?1",
            [platform],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = found {
        conn.execute(
            "UPDATE dat_versions SET loaded_at = ?2 WHERE id = ?1",
            params![id, now],
        )?;
        return Ok(DatVersionId(id));
    }
    conn.execute(
        "INSERT INTO dat_versions (platform_id, dat_name, version, source_file, loaded_at, game_count, source)
         VALUES (?1, ?2, 'mra', ?2, ?3, 0, 'mra')",
        params![platform, MRA_DAT_NAME, now],
    )?;
    Ok(DatVersionId(conn.last_insert_rowid()))
}

/// One MRA file as a title.
#[derive(Debug, Clone, Copy)]
pub struct MraTitle<'a> {
    /// `<name>`, or the file stem when absent.
    pub name: &'a str,
    /// Name without tags.
    pub base_name: &'a str,
    /// Clone-grouping key; kept apart from DAT keys by the caller.
    pub group_key: &'a str,
    /// Region names parsed from the name.
    pub regions: &'a [String],
    /// Language codes parsed from the name.
    pub languages: &'a [String],
    /// Revision label parsed from the name.
    pub revision: Option<&'a str>,
    /// Flag labels parsed from the name.
    pub flags: &'a [String],
    /// `<setname>`.
    pub setname: Option<&'a str>,
    /// `<rbf>`.
    pub rbf: Option<&'a str>,
    /// The MRA file relative to `_Arcade`.
    pub mra_path: &'a str,
}

/// One zip an MRA names, as a rom of its title.
#[derive(Debug, Clone, Copy)]
pub struct MraZip<'a> {
    /// Zip file name.
    pub name: &'a str,
    /// Directory under `games/` it is read from.
    pub zip_dir: &'a str,
    /// The md5 of the MRA `<rom>` naming this zip, when it carries one.
    pub md5: Option<&'a str>,
    /// Whether the zip is on disk.
    pub present: bool,
}

/// Marks the live MRA titles of `platform` pending before a catalogue run, so
/// [`retire_absent`] retires those whose MRA is gone. Run both in one transaction.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert_eq!(mistarr_server::db::arcade::begin_load(&conn, "arcade").unwrap(), 0);
/// ```
pub fn begin_load(conn: &Connection, platform: &str) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE titles SET retired = 2 WHERE platform_id = ?1 AND source = 'mra' AND retired = 0",
        [platform],
    )?)
}

/// Retires the MRA titles still pending from [`begin_load`]. Returns how many.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert_eq!(mistarr_server::db::arcade::retire_absent(&conn, "arcade").unwrap(), 0);
/// ```
pub fn retire_absent(conn: &Connection, platform: &str) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE titles SET retired = 1, is_1g1r_pick = 0
         WHERE platform_id = ?1 AND source = 'mra' AND retired = 2",
        [platform],
    )?)
}

/// Stores an MRA title and its zips, reusing the MRA title of the same name so
/// ids and `wanted` survive a re-scan. Zips the MRA no longer names are retired.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::arcade::{self, MraTitle, MraZip};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// mistarr_server::db::platforms::seed(&mut conn, &mistarr_mister::platforms::PLATFORMS).unwrap();
/// let v = arcade::mra_version(&conn, "arcade", 1).unwrap();
/// let t = MraTitle { name: "Example Blaster", base_name: "Example Blaster", group_key: "mra:example blaster",
///     regions: &[], languages: &[], revision: None, flags: &[], setname: Some("exblast"),
///     rbf: Some("excore"), mra_path: "Example Blaster.mra" };
/// let zips = [MraZip { name: "exblast.zip", zip_dir: "mame", md5: None, present: false }];
/// let id = arcade::upsert_title(&conn, "arcade", v, &t, &zips).unwrap();
/// assert_eq!(arcade::upsert_title(&conn, "arcade", v, &t, &zips).unwrap(), id);
/// ```
pub fn upsert_title(
    conn: &Connection,
    platform: &str,
    version: DatVersionId,
    t: &MraTitle<'_>,
    zips: &[MraZip<'_>],
) -> Result<TitleId> {
    let json = |xs: &[String]| serde_json::to_string(xs).unwrap_or_else(|_| "[]".to_owned());
    let (regions, languages, flags) = (json(t.regions), json(t.languages), json(t.flags));
    let existing: Option<i64> = conn
        .prepare_cached(
            "SELECT id FROM titles WHERE platform_id = ?1 AND source = 'mra' AND name = ?2",
        )?
        .query_row(params![platform, t.name], |r| r.get(0))
        .optional()?;
    let id = if let Some(id) = existing {
        conn.prepare_cached(
            "UPDATE titles SET dat_version_id = ?2, base_name = ?3, regions = ?4, languages = ?5,
               revision = ?6, flags = ?7, group_key = ?8, setname = ?9, rbf = ?10,
               mra_path = ?11, inferred = 1, retired = 0
             WHERE id = ?1",
        )?
        .execute(params![
            id,
            version.0,
            t.base_name,
            regions,
            languages,
            t.revision,
            flags,
            t.group_key,
            t.setname,
            t.rbf,
            t.mra_path
        ])?;
        conn.prepare_cached("UPDATE roms SET retired = 1 WHERE title_id = ?1")?
            .execute([id])?;
        id
    } else {
        conn.prepare_cached(
            "INSERT INTO titles (platform_id, dat_version_id, name, base_name, regions, languages,
               revision, flags, group_key, inferred, source, setname, rbf, mra_path)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, 'mra', ?10, ?11, ?12)",
        )?
        .execute(params![
            platform,
            version.0,
            t.name,
            t.base_name,
            regions,
            languages,
            t.revision,
            flags,
            t.group_key,
            t.setname,
            t.rbf,
            t.mra_path
        ])?;
        let id = conn.last_insert_rowid();
        conn.prepare_cached("UPDATE titles SET parent_id = id WHERE id = ?1")?
            .execute([id])?;
        id
    };
    let mut stmt = conn.prepare_cached(
        "INSERT INTO roms (title_id, name, size, md5, status, zip_dir, present, retired)
         VALUES (?1, ?2, 0, ?3, 'good', ?4, ?5, 0)
         ON CONFLICT(title_id, name) DO UPDATE SET md5 = excluded.md5, zip_dir = excluded.zip_dir,
           present = excluded.present, retired = 0",
    )?;
    let mut seen: Vec<&str> = Vec::with_capacity(zips.len());
    for z in zips {
        if seen.iter().any(|s| s.eq_ignore_ascii_case(z.name)) {
            continue;
        }
        seen.push(z.name);
        stmt.execute(params![id, z.name, z.md5, z.zip_dir, z.present])?;
    }
    Ok(TitleId(id))
}

/// A live MRA title with the stamp its md5 check last ran against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckState {
    /// The title.
    pub id: TitleId,
    /// Its MRA file relative to `_Arcade`.
    pub mra_path: String,
    /// Stamp of the last check, `None` when none ran.
    pub stamp: Option<String>,
}

/// Every live MRA title of `platform` with its last check stamp.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert!(mistarr_server::db::arcade::check_states(&conn, "arcade").unwrap().is_empty());
/// ```
pub fn check_states(conn: &Connection, platform: &str) -> Result<Vec<CheckState>> {
    let mut stmt = conn.prepare(
        "SELECT id, COALESCE(mra_path, ''), mra_stamp FROM titles
         WHERE platform_id = ?1 AND source = 'mra' AND retired = 0 ORDER BY id",
    )?;
    let rows = stmt.query_map([platform], |r| {
        Ok(CheckState {
            id: TitleId(r.get(0)?),
            mra_path: r.get(1)?,
            stamp: r.get(2)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Records the md5 check of an MRA title; all `None` clears it.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::{arcade, titles::TitleId};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// arcade::set_check(&conn, TitleId(1), None, None, None).unwrap();
/// ```
pub fn set_check(
    conn: &Connection,
    id: TitleId,
    check: Option<&str>,
    detail: Option<&str>,
    stamp: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE titles SET mra_check = ?2, mra_detail = ?3, mra_stamp = ?4 WHERE id = ?1",
        params![id.0, check, detail, stamp],
    )?;
    Ok(())
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
/// assert!(mistarr_server::db::arcade::zip_rom(&conn, 1).unwrap().is_none());
/// ```
pub fn zip_rom(conn: &Connection, rom_id: i64) -> Result<Option<ZipRom>> {
    Ok(conn
        .query_row(
            "SELECT t.id, r.name, COALESCE(r.zip_dir, ''), COALESCE(t.mra_path, '')
             FROM roms r JOIN titles t ON t.id = r.title_id
             WHERE r.id = ?1 AND t.source = 'mra'",
            [rom_id],
            |r| {
                Ok(ZipRom {
                    title_id: TitleId(r.get(0)?),
                    name: r.get(1)?,
                    zip_dir: r.get(2)?,
                    mra_path: r.get(3)?,
                })
            },
        )
        .optional()?)
}

/// Live MRA titles of `platform` naming zip `name` in `zip_dir`, compared case-insensitively
/// as exFAT does, with their MRA paths.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let found = mistarr_server::db::arcade::titles_naming(&conn, "arcade", "mame", "exblast.zip");
/// assert!(found.unwrap().is_empty());
/// ```
pub fn titles_naming(
    conn: &Connection,
    platform: &str,
    zip_dir: &str,
    name: &str,
) -> Result<Vec<(TitleId, String)>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT t.id, COALESCE(t.mra_path, '') FROM titles t
         JOIN roms r ON r.title_id = t.id AND r.retired = 0
         WHERE t.platform_id = ?1 AND t.source = 'mra' AND t.retired = 0
           AND lower(COALESCE(r.zip_dir, '')) = lower(?2) AND lower(r.name) = lower(?3)
         ORDER BY t.id",
    )?;
    let rows = stmt.query_map(params![platform, zip_dir, name], |r| {
        Ok((TitleId(r.get(0)?), r.get(1)?))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Records whether zip `name` in `zip_dir` of MRA title `title` is on disk.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::{arcade, titles::TitleId};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert_eq!(arcade::set_zip_present(&conn, TitleId(1), "exblast.zip", "mame", true).unwrap(), 0);
/// ```
pub fn set_zip_present(
    conn: &Connection,
    title: TitleId,
    name: &str,
    zip_dir: &str,
    present: bool,
) -> Result<usize> {
    Ok(conn
        .prepare_cached(
            "UPDATE roms SET present = ?4
             WHERE title_id = ?1 AND retired = 0 AND lower(name) = lower(?2)
               AND lower(COALESCE(zip_dir, '')) = lower(?3)",
        )?
        .execute(params![title.0, name, zip_dir, present])?)
}

/// The live DAT entry of `platform` named `set`, as a MAME DAT names the zip `set.zip`:
/// the exact name first, then one differing only in case.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert!(mistarr_server::db::arcade::dat_entry_named(&conn, "arcade", "exblast").unwrap().is_none());
/// ```
pub fn dat_entry_named(conn: &Connection, platform: &str, set: &str) -> Result<Option<TitleId>> {
    Ok(conn
        .query_row(
            "SELECT t.id FROM titles t JOIN dat_versions v ON v.id = t.dat_version_id
             WHERE t.platform_id = ?1 AND t.source = 'dat' AND t.retired = 0
               AND v.superseded_by IS NULL AND v.retired = 0 AND lower(t.name) = lower(?2)
             ORDER BY t.name = ?2 DESC, t.id LIMIT 1",
            params![platform, set],
            |r| r.get(0).map(TitleId),
        )
        .optional()?)
}

/// What title detail shows for an MRA title.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct MraInfo {
    /// `<setname>`.
    pub setname: Option<String>,
    /// `<rbf>`.
    pub rbf: Option<String>,
    /// The MRA file relative to `_Arcade`.
    pub path: Option<String>,
    /// Zips the MRA names that are not on disk, as paths relative to `games/`.
    pub missing_zips: Vec<String>,
    /// The md5 check: `match`, `mismatch`, `missing_part` or `refused`; `None` when not run.
    pub md5_check: Option<String>,
    /// Why the check did not match.
    pub md5_detail: Option<String>,
}

/// [`MraInfo`] of title `id`, or `None` when it is not an MRA title.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::{arcade, titles::TitleId};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert!(arcade::info(&conn, TitleId(1)).unwrap().is_none());
/// ```
pub fn info(conn: &Connection, id: TitleId) -> Result<Option<MraInfo>> {
    let Some(mut info) = conn
        .query_row(
            "SELECT setname, rbf, mra_path, mra_check, mra_detail FROM titles
             WHERE id = ?1 AND source = 'mra'",
            [id.0],
            |r| {
                Ok(MraInfo {
                    setname: r.get(0)?,
                    rbf: r.get(1)?,
                    path: r.get(2)?,
                    missing_zips: Vec::new(),
                    md5_check: r.get(3)?,
                    md5_detail: r.get(4)?,
                })
            },
        )
        .optional()?
    else {
        return Ok(None);
    };
    let mut stmt = conn.prepare_cached(
        "SELECT CASE WHEN COALESCE(zip_dir, '') = '' THEN name ELSE zip_dir || '/' || name END
         FROM roms WHERE title_id = ?1 AND retired = 0 AND present = 0 ORDER BY id",
    )?;
    info.missing_zips = stmt
        .query_map([id.0], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(Some(info))
}

#[cfg(test)]
mod tests;
