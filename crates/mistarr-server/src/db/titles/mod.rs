//! The `titles` and `roms` tables, clone groups, 1G1R picks and the browse
//! query over `title_groups`; see `docs/DATA-MODEL.md` and `docs/VERIFICATION.md`.

use rusqlite::{params, Connection, OptionalExtension};

use super::ids::{DatVersionId, TitleId};
use mistarr_core::PlatformKey;

use super::platforms::PlatformRef;
use super::sql::{self, code_enum};

use crate::error::Result;

code_enum! {
    /// `titles.source`, `dat_versions.source` and `title_groups.source`: where a title was read from.
    pub enum TitleSource {
        /// An entry of a DAT file.
        Dat = ("dat", 0),
        /// An arcade title read from an MRA file.
        Mra = ("mra", 1),
    }
}

code_enum! {
    /// `roms.status`: the DAT's dump status of a rom.
    pub enum RomStatus {
        /// A good dump, the DAT's default.
        Good = ("good", 0),
        /// A known bad dump.
        BadDump = ("baddump", 1),
        /// No known dump; hashes are usually absent.
        NoDump = ("nodump", 2),
        /// A dump the DAT marks verified.
        Verified = ("verified", 3),
    }
}

impl From<mistarr_core::dat::RomStatus> for RomStatus {
    fn from(status: mistarr_core::dat::RomStatus) -> Self {
        use mistarr_core::dat::RomStatus as Dat;
        match status {
            Dat::BadDump => Self::BadDump,
            Dat::NoDump => Self::NoDump,
            Dat::Verified => Self::Verified,
            // Core's enum is non-exhaustive; a status it adds is stored as its default.
            _ => Self::Good,
        }
    }
}

/// One `<game>` with its name already parsed.
#[derive(Debug, Clone, Copy)]
pub struct TitleInput<'a> {
    /// Full DAT game name.
    pub name: &'a str,
    /// Name without tags.
    pub base_name: &'a str,
    /// `naming::group_key` of the name.
    pub group_key: &'a str,
    /// The DAT's `cloneof`, if any.
    pub clone_of: Option<&'a str>,
    /// Region names.
    pub regions: &'a [String],
    /// Language codes.
    pub languages: &'a [String],
    /// Revision label.
    pub revision: Option<&'a str>,
    /// Flag labels.
    pub flags: &'a [String],
}

/// One `<rom>` of a game.
#[derive(Debug, Clone, Copy)]
pub struct RomInput<'a> {
    /// File name in the DAT.
    pub name: &'a str,
    /// Size in bytes.
    pub size: u64,
    /// CRC32, when the DAT lists it.
    pub crc32: Option<mistarr_core::Crc32>,
    /// MD5, when the DAT lists it.
    pub md5: Option<mistarr_core::Md5>,
    /// SHA1, when the DAT lists it.
    pub sha1: Option<mistarr_core::Sha1>,
    /// The DAT's dump status.
    pub status: RomStatus,
    /// The DAT's `header` attribute, verbatim.
    pub header: Option<&'a str>,
}

code_enum! {
    /// `tags.kind`: which of a title's lists a tag belongs to.
    pub enum Tag {
        /// A flag such as `beta` or `bios`.
        Flags = ("flag", 0),
        /// A region name.
        Regions = ("region", 1),
        /// A language code.
        Languages = ("language", 2),
    }
}

impl Tag {
    /// The table linking titles to tags of this kind.
    fn table(self) -> &'static str {
        match self {
            Self::Flags => "title_flags",
            Self::Regions => "title_regions",
            Self::Languages => "title_languages",
        }
    }
}

/// The id of the `tag` named `name`, inserting it when it is new. A known flag or region
/// (a region ignoring ASCII case) carries its summary bit.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub(crate) fn tag_id(conn: &Connection, tag: Tag, name: &str) -> Result<i64> {
    if let Some(id) = conn
        .prepare_cached("SELECT id FROM tags WHERE kind = ?1 AND name = ?2")?
        .query_row(params![tag, name], |r| r.get(0))
        .optional()?
    {
        return Ok(id);
    }
    let bit = match tag {
        Tag::Flags => super::groups::flag_bit(name),
        Tag::Regions => super::groups::region_bit(name),
        Tag::Languages => None,
    };
    conn.prepare_cached("INSERT INTO tags (kind, name, bit) VALUES (?1, ?2, ?3)")?
        .execute(params![tag, name, bit])?;
    Ok(conn.last_insert_rowid())
}

/// Title `id`'s `tag` values in order.
fn tags_of(conn: &Connection, id: TitleId, tag: Tag) -> Result<Vec<String>> {
    let table = tag.table();
    let rows = conn
        .prepare_cached(&format!(
            "SELECT g.name FROM {table} x JOIN tags g ON g.id = x.tag_id
             WHERE x.title_id = ?1 ORDER BY x.pos"
        ))?
        .query_map([id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// Replaces title `id`'s `tag` values with `values`, keeping the first of repeats, and
/// writes nothing when they are unchanged so the title's group stays clean.
fn store_tags(conn: &Connection, id: TitleId, tag: Tag, values: &[String]) -> Result<()> {
    let mut wanted: Vec<&str> = Vec::with_capacity(values.len());
    for v in values {
        if !wanted.contains(&v.as_str()) {
            wanted.push(v);
        }
    }
    if tags_of(conn, id, tag)?
        .iter()
        .map(String::as_str)
        .eq(wanted.iter().copied())
    {
        return Ok(());
    }
    let table = tag.table();
    conn.prepare_cached(&format!("DELETE FROM {table} WHERE title_id = ?1"))?
        .execute([id])?;
    let mut insert = conn.prepare_cached(&format!(
        "INSERT INTO {table} (title_id, pos, tag_id) VALUES (?1, ?2, ?3)"
    ))?;
    for (pos, v) in wanted.iter().enumerate() {
        insert.execute(params![id, sql::to_i64(pos), tag_id(conn, tag, v)?])?;
    }
    Ok(())
}

/// Stores a title's regions, languages and flags in their tables.
pub(crate) fn store_lists(
    conn: &Connection,
    id: TitleId,
    regions: &[String],
    languages: &[String],
    flags: &[String],
) -> Result<()> {
    store_tags(conn, id, Tag::Regions, regions)?;
    store_tags(conn, id, Tag::Languages, languages)?;
    store_tags(conn, id, Tag::Flags, flags)
}

/// Replaces the flags of title `id`, in order.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure, including a title that does not exist.
///
/// ```
/// use mistarr_server::db::titles::set_flags;
/// use mistarr_server::db::ids::TitleId;
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert!(set_flags(&conn, TitleId::new(1), &[]).is_ok());
/// ```
pub fn set_flags(conn: &Connection, id: TitleId, flags: &[String]) -> Result<()> {
    store_tags(conn, id, Tag::Flags, flags)
}

/// The flags of title `id`, in order.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::titles::flags_of;
/// use mistarr_server::db::ids::TitleId;
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert!(flags_of(&conn, TitleId::new(1)).unwrap().is_empty());
/// ```
pub fn flags_of(conn: &Connection, id: TitleId) -> Result<Vec<String>> {
    tags_of(conn, id, Tag::Flags)
}

/// The columns of an MRA title row beyond a DAT title's.
pub(crate) struct MraColumns<'a> {
    pub(crate) setname: Option<&'a str>,
    pub(crate) rbf: Option<&'a str>,
    pub(crate) mra_path: &'a str,
    pub(crate) file_stamp: &'a str,
    pub(crate) run: i64,
}

/// A title row as either writer stores it; `mra` is set for an MRA title only.
pub(crate) struct PutTitle<'a> {
    pub(crate) source: TitleSource,
    pub(crate) name: &'a str,
    pub(crate) base_name: &'a str,
    pub(crate) revision: Option<&'a str>,
    pub(crate) group_key: &'a str,
    pub(crate) clone_of: Option<&'a str>,
    pub(crate) regions: &'a [String],
    pub(crate) languages: &'a [String],
    pub(crate) flags: &'a [String],
    pub(crate) mra: Option<MraColumns<'a>>,
}

/// Inserts the title `put` describes under `version`, or updates the one it names, and
/// stores its lists. A DAT title is reused from any version of the DAT family and an MRA
/// title by name on the platform. An updated title's roms are retired for the caller to
/// store again. Returns the id and whether the title existed.
pub(crate) fn put_title(
    conn: &Connection,
    platform: PlatformKey,
    version: DatVersionId,
    put: &PutTitle<'_>,
) -> Result<(TitleId, bool)> {
    let existing: Option<TitleId> = match put.source {
        TitleSource::Dat => conn
            .prepare_cached(
                // Unary plus keeps the lookup on (dat_version_id, name), not a platform-wide index.
                "SELECT t.id FROM dat_versions d JOIN titles t ON t.dat_version_id = d.id AND t.name = ?1
                 WHERE d.family = (SELECT family FROM dat_versions WHERE id = ?2)
                   AND +t.source = ?4 AND +t.platform = ?3
                 ORDER BY d.id = ?2 DESC, d.loaded_at DESC LIMIT 1",
            )?
            .query_row(
                params![put.name, version, platform, TitleSource::Dat],
                |r| r.get(0),
            )
            .optional()?,
        TitleSource::Mra => conn
            .prepare_cached(
                "SELECT id FROM titles WHERE platform = ?1 AND source = ?3 AND name = ?2",
            )?
            .query_row(
                params![platform, put.name, TitleSource::Mra],
                |r| r.get(0),
            )
            .optional()?,
    };
    let mra = put.mra.as_ref();
    let inferred = mra.map(|_| true);
    let id = if let Some(id) = existing {
        conn.prepare_cached(
            "UPDATE titles SET platform = ?2, dat_version_id = ?3, base_name = ?4, revision = ?5,
               clone_of = ?6, group_key = ?7, setname = ?8, rbf = ?9, mra_path = ?10,
               mra_file_stamp = ?11, mra_seen = ?12, inferred = COALESCE(?13, inferred), retired = 0
             WHERE id = ?1",
        )?
        .execute(params![
            id,
            platform,
            version,
            put.base_name,
            put.revision,
            put.clone_of,
            put.group_key,
            mra.and_then(|m| m.setname),
            mra.and_then(|m| m.rbf),
            mra.map(|m| m.mra_path),
            mra.map(|m| m.file_stamp),
            mra.map(|m| m.run),
            inferred,
        ])?;
        conn.prepare_cached("UPDATE roms SET retired = 1 WHERE title_id = ?1")?
            .execute([id])?;
        id
    } else {
        conn.prepare_cached(
            "INSERT INTO titles (platform, dat_version_id, name, base_name, revision, clone_of,
               group_key, source, setname, rbf, mra_path, mra_file_stamp, mra_seen, inferred)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, COALESCE(?14, 0))",
        )?
        .execute(params![
            platform,
            version,
            put.name,
            put.base_name,
            put.revision,
            put.clone_of,
            put.group_key,
            put.source,
            mra.and_then(|m| m.setname),
            mra.and_then(|m| m.rbf),
            mra.map(|m| m.mra_path),
            mra.map(|m| m.file_stamp),
            mra.map(|m| m.run),
            inferred,
        ])?;
        let id = TitleId::new(conn.last_insert_rowid());
        conn.prepare_cached("UPDATE titles SET parent_id = id WHERE id = ?1")?
            .execute([id])?;
        id
    };
    store_lists(conn, id, put.regions, put.languages, put.flags)?;
    Ok((id, existing.is_some()))
}

/// Stores a game under `version`, reusing the title of the same name from any version
/// of its DAT family on the same platform, so ids, `wanted` and file provenance survive
/// a new version. Roms the game no longer lists are marked retired.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::{dats, titles};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// mistarr_server::db::platforms::seed(&mut conn, &mistarr_mister::platforms::PLATFORMS).unwrap();
/// let v = dats::NewVersion { dat_name: "Maker - Game Boy", version: "1", source_file: "a.dat", platform: None, now: 1 };
/// let version = dats::upsert_version(&conn, &v).unwrap().id;
/// let t = titles::TitleInput { name: "Example Quest (USA)", base_name: "Example Quest",
///     group_key: "example quest", clone_of: None, regions: &[], languages: &[], revision: None, flags: &[] };
/// let gb = mistarr_server::db::platforms::PlatformRef::resolve(&conn, &mistarr_core::PlatformId::new("gb"))
///     .unwrap().unwrap();
/// let id = titles::upsert_title(&conn, &gb, version, &t, &[]).unwrap();
/// assert_eq!(titles::upsert_title(&conn, &gb, version, &t, &[]).unwrap(), id);
/// ```
pub fn upsert_title(
    conn: &Connection,
    platform: &PlatformRef,
    version: DatVersionId,
    t: &TitleInput<'_>,
    roms: &[RomInput<'_>],
) -> Result<TitleId> {
    let put = PutTitle {
        source: TitleSource::Dat,
        name: t.name,
        base_name: t.base_name,
        revision: t.revision,
        group_key: t.group_key,
        clone_of: t.clone_of,
        regions: t.regions,
        languages: t.languages,
        flags: t.flags,
        mra: None,
    };
    let (id, existed) = put_title(conn, platform.key, version, &put)?;
    let arcade = mistarr_mister::platforms::by_id(platform.id.as_str())
        .is_some_and(mistarr_mister::platforms::Platform::is_arcade);
    if existed && !arcade {
        for r in roms {
            let size = sql::to_i64(r.size);
            let listed = (r.crc32, r.md5, r.sha1);
            super::files::unmatch_changed_rom(conn, id, r.name, size, listed)?;
        }
    }
    let mut stmt = conn.prepare_cached(
        "INSERT INTO roms (title_id, own_name, name, size, crc32, md5, sha1, status, header, retired)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0)
         ON CONFLICT(title_id, own_name, name) DO UPDATE SET size = excluded.size, crc32 = excluded.crc32,
           md5 = excluded.md5, sha1 = excluded.sha1, status = excluded.status,
           header = excluded.header, retired = 0",
    )?;
    for r in roms {
        let size = sql::to_i64(r.size);
        let (own, stored) = sql::rom_name_parts(t.name, r.name);
        stmt.execute(params![
            id, own, stored, size, r.crc32, r.md5, r.sha1, r.status, r.header
        ])?;
    }
    Ok(id)
}

pub mod browse;
pub mod detail;
pub mod recompute;

#[cfg(test)]
mod tests;
