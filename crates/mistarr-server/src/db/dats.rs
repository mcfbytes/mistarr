//! The `dat_versions` table and supersession; see `docs/ARCHITECTURE.md` "DAT import".

use std::fmt;

use mistarr_core::PlatformId;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use crate::error::Result;

/// A `dat_versions.id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DatVersionId(pub i64);

impl fmt::Display for DatVersionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// One `dat_versions` row as the API returns it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DatVersionRow {
    /// Row id.
    pub id: DatVersionId,
    /// Bound platform, `None` while unbound.
    pub platform_id: Option<PlatformId>,
    /// `<header><name>`, verbatim.
    pub dat_name: String,
    /// `<header><version>`, verbatim.
    pub version: String,
    /// Basename of the file under `dats/loaded/` this version was read from.
    pub source_file: String,
    /// Unix seconds.
    pub loaded_at: i64,
    /// The version that replaced this one.
    pub superseded_by: Option<DatVersionId>,
    /// Number of games in the DAT.
    pub game_count: u64,
    /// Retired through `DELETE /dats/{id}`.
    pub retired: bool,
}

const COLUMNS: &str =
    "id, platform_id, dat_name, version, source_file, loaded_at, superseded_by, game_count, retired";

fn from_row(r: &Row<'_>) -> rusqlite::Result<DatVersionRow> {
    Ok(DatVersionRow {
        id: DatVersionId(r.get(0)?),
        platform_id: r.get::<_, Option<String>>(1)?.map(PlatformId),
        dat_name: r.get(2)?,
        version: r.get(3)?,
        source_file: r.get(4)?,
        loaded_at: r.get(5)?,
        superseded_by: r.get::<_, Option<i64>>(6)?.map(DatVersionId),
        game_count: unsigned(r.get(7)?),
        retired: r.get(8)?,
    })
}

/// A DAT header about to be stored.
#[derive(Debug, Clone, Copy)]
pub struct NewVersion<'a> {
    /// `<header><name>`.
    pub dat_name: &'a str,
    /// `<header><version>`.
    pub version: &'a str,
    /// Basename under `dats/loaded/`.
    pub source_file: &'a str,
    /// Platform from binding; `None` inherits the newest bound version of the same name.
    pub platform: Option<&'a str>,
    /// Unix seconds.
    pub now: i64,
}

/// What [`upsert_version`] decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionPlan {
    /// The stored row.
    pub id: DatVersionId,
    /// The platform the row is bound to after the upsert.
    pub platform_id: Option<PlatformId>,
    /// True when this version is now the newest of its name; its titles should be loaded.
    pub current: bool,
}

/// Inserts or refreshes the `(dat_name, version)` row and applies supersession:
/// a version whose string is not below the newest live one of the same name
/// becomes current and supersedes the others; an older one is stored superseded.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::dats::{upsert_version, NewVersion};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let v = NewVersion { dat_name: "Test Console", version: "1", source_file: "a.dat", platform: None, now: 1 };
/// assert!(upsert_version(&conn, &v).unwrap().current);
/// ```
pub fn upsert_version(conn: &Connection, v: &NewVersion<'_>) -> Result<VersionPlan> {
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM dat_versions WHERE dat_name = ?1 AND version = ?2",
            params![v.dat_name, v.version],
            |r| r.get(0),
        )
        .optional()?;
    let inherited: Option<String> = match v.platform {
        Some(p) => Some(p.to_owned()),
        None => conn
            .query_row(
                "SELECT platform_id FROM dat_versions
                 WHERE dat_name = ?1 AND platform_id IS NOT NULL
                 ORDER BY loaded_at DESC, id DESC LIMIT 1",
                [v.dat_name],
                |r| r.get(0),
            )
            .optional()?,
    };
    let newest: Option<(i64, String)> = conn
        .query_row(
            "SELECT id, version FROM dat_versions
             WHERE dat_name = ?1 AND superseded_by IS NULL AND retired = 0 AND id IS NOT ?2
             ORDER BY loaded_at DESC, id DESC LIMIT 1",
            params![v.dat_name, existing],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let current = newest
        .as_ref()
        .is_none_or(|(_, version)| v.version >= version.as_str());
    let superseded_by = if current {
        None
    } else {
        newest.map(|(id, _)| id)
    };
    let id = if let Some(id) = existing {
        conn.execute(
            "UPDATE dat_versions SET source_file = ?2, loaded_at = ?3,
               platform_id = COALESCE(?4, platform_id), superseded_by = ?5, retired = 0
             WHERE id = ?1",
            params![id, v.source_file, v.now, inherited, superseded_by],
        )?;
        id
    } else {
        conn.execute(
            "INSERT INTO dat_versions
               (platform_id, dat_name, version, source_file, loaded_at, superseded_by, game_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)",
            params![
                inherited,
                v.dat_name,
                v.version,
                v.source_file,
                v.now,
                superseded_by
            ],
        )?;
        conn.last_insert_rowid()
    };
    if current {
        conn.execute(
            "UPDATE dat_versions SET superseded_by = ?1
             WHERE dat_name = ?2 AND id != ?1 AND superseded_by IS NULL",
            params![id, v.dat_name],
        )?;
    }
    let platform_id: Option<String> = conn.query_row(
        "SELECT platform_id FROM dat_versions WHERE id = ?1",
        [id],
        |r| r.get(0),
    )?;
    Ok(VersionPlan {
        id: DatVersionId(id),
        platform_id: platform_id.map(PlatformId),
        current,
    })
}

/// Records the number of games read from a version.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::dats::{self, NewVersion};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let v = NewVersion { dat_name: "Test Console", version: "1", source_file: "a.dat", platform: None, now: 1 };
/// let id = dats::upsert_version(&conn, &v).unwrap().id;
/// dats::set_game_count(&conn, id, 7).unwrap();
/// assert_eq!(dats::get(&conn, id).unwrap().unwrap().game_count, 7);
/// ```
pub fn set_game_count(conn: &Connection, id: DatVersionId, count: u64) -> Result<()> {
    conn.execute(
        "UPDATE dat_versions SET game_count = ?2 WHERE id = ?1",
        params![id.0, i64::try_from(count).unwrap_or(i64::MAX)],
    )?;
    Ok(())
}

/// Marks the live titles of version `id` pending before it is loaded again, so
/// [`retire_absent`] can tell the entries a reload of the same version dropped.
/// Pending titles the load stores become live again. Run both in one transaction.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::dats::{self, DatVersionId};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert_eq!(dats::begin_load(&conn, DatVersionId(1)).unwrap(), 0);
/// ```
pub fn begin_load(conn: &Connection, id: DatVersionId) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE titles SET retired = 2 WHERE dat_version_id = ?1 AND retired = 0",
        [id.0],
    )?)
}

/// Marks retired the titles a load of `id` did not list: those left on other
/// versions of its DAT name and those still pending from [`begin_load`]. Returns how many.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::dats::{self, NewVersion};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let v = NewVersion { dat_name: "Test Console", version: "1", source_file: "a.dat", platform: None, now: 1 };
/// let id = dats::upsert_version(&conn, &v).unwrap().id;
/// assert_eq!(dats::retire_absent(&conn, id).unwrap(), 0);
/// ```
pub fn retire_absent(conn: &Connection, id: DatVersionId) -> Result<usize> {
    let others = conn.execute(
        "UPDATE titles SET retired = 1, is_1g1r_pick = 0
         WHERE retired = 0 AND dat_version_id IN (
           SELECT o.id FROM dat_versions o JOIN dat_versions n ON n.dat_name = o.dat_name
           WHERE n.id = ?1 AND o.id != ?1)",
        [id.0],
    )?;
    let pending = conn.execute(
        "UPDATE titles SET retired = 1, is_1g1r_pick = 0 WHERE dat_version_id = ?1 AND retired = 2",
        [id.0],
    )?;
    Ok(others + pending)
}

/// Retires a version and every title still attached to it. Returns the row
/// as it was, or `None` when there is no such version.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::dats::{self, DatVersionId};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert!(dats::retire(&conn, DatVersionId(1)).unwrap().is_none());
/// ```
pub fn retire(conn: &Connection, id: DatVersionId) -> Result<Option<DatVersionRow>> {
    let Some(row) = get(conn, id)? else {
        return Ok(None);
    };
    conn.execute("UPDATE dat_versions SET retired = 1 WHERE id = ?1", [id.0])?;
    conn.execute(
        "UPDATE titles SET retired = 1, is_1g1r_pick = 0 WHERE dat_version_id = ?1",
        [id.0],
    )?;
    Ok(Some(row))
}

/// Reads one version.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::dats::{self, DatVersionId};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert!(dats::get(&conn, DatVersionId(1)).unwrap().is_none());
/// ```
pub fn get(conn: &Connection, id: DatVersionId) -> Result<Option<DatVersionRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM dat_versions WHERE id = ?1"),
            [id.0],
            from_row,
        )
        .optional()?)
}

/// Every version, newest first, with the total before paging.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let (items, total) = mistarr_server::db::dats::list(&conn, 10, 0).unwrap();
/// assert!(items.is_empty() && total == 0);
/// ```
pub fn list(conn: &Connection, limit: u32, offset: u32) -> Result<(Vec<DatVersionRow>, u64)> {
    let total: i64 = conn.query_row("SELECT COUNT(*) FROM dat_versions", [], |r| r.get(0))?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM dat_versions ORDER BY loaded_at DESC, id DESC LIMIT ?1 OFFSET ?2"
    ))?;
    let rows = stmt
        .query_map(params![limit, offset], from_row)?
        .collect::<rusqlite::Result<_>>()?;
    Ok((rows, unsigned(total)))
}

/// A non-negative SQLite integer as `u64`.
fn unsigned(n: i64) -> u64 {
    u64::try_from(n).unwrap_or(0)
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

    fn new<'a>(version: &'a str, platform: Option<&'a str>, now: i64) -> NewVersion<'a> {
        NewVersion {
            dat_name: "Maker - Game Boy",
            version,
            source_file: "gb.dat",
            platform,
            now,
        }
    }

    fn title(c: &Connection, version: DatVersionId, name: &str) -> i64 {
        c.execute(
            "INSERT INTO titles (platform_id, dat_version_id, name, base_name, regions, languages, flags)
             VALUES ('gb', ?1, ?2, ?2, '[]', '[]', '[]')",
            params![version.0, name],
        )
        .expect("insert");
        c.last_insert_rowid()
    }

    #[test]
    fn newer_versions_supersede_and_older_ones_arrive_superseded() {
        let c = conn();
        let v1 = upsert_version(&c, &new("2", Some("gb"), 1)).expect("v1");
        assert!(v1.current);
        assert_eq!(v1.platform_id, Some(PlatformId("gb".into())));
        let v2 = upsert_version(&c, &new("3", None, 2)).expect("v2");
        assert!(v2.current);
        assert_eq!(v2.platform_id, v1.platform_id, "binding is inherited");
        assert_eq!(
            get(&c, v1.id).expect("get").expect("row").superseded_by,
            Some(v2.id)
        );
        let old = upsert_version(&c, &new("1", None, 3)).expect("old");
        assert!(!old.current);
        assert_eq!(
            get(&c, old.id).expect("get").expect("row").superseded_by,
            Some(v2.id)
        );
        let again = upsert_version(&c, &new("3", None, 4)).expect("reload");
        assert_eq!(again.id, v2.id);
        assert!(again.current);
        let (rows, total) = list(&c, 2, 0).expect("list");
        assert_eq!(total, 3);
        assert_eq!(rows[0].id, v2.id, "newest first");
    }

    #[test]
    fn unbound_names_stay_unbound() {
        let c = conn();
        let v = upsert_version(
            &c,
            &NewVersion {
                dat_name: "Test Console",
                ..new("1", None, 1)
            },
        )
        .expect("v");
        assert_eq!(v.platform_id, None);
        set_game_count(&c, v.id, 3).expect("count");
        let row = get(&c, v.id).expect("get").expect("row");
        assert_eq!((row.game_count, row.platform_id), (3, None));
    }

    #[test]
    fn absent_entries_are_retired_not_deleted() {
        let c = conn();
        let v1 = upsert_version(&c, &new("1", Some("gb"), 1)).expect("v1").id;
        let kept = title(&c, v1, "Kept");
        let gone = title(&c, v1, "Gone");
        let v2 = upsert_version(&c, &new("2", None, 2)).expect("v2").id;
        c.execute(
            "UPDATE titles SET dat_version_id = ?1 WHERE id = ?2",
            params![v2.0, kept],
        )
        .expect("move");
        assert_eq!(retire_absent(&c, v2).expect("retire"), 1);
        let retired: Vec<(i64, bool)> = c
            .prepare("SELECT id, retired FROM titles ORDER BY id")
            .expect("prepare")
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .expect("query")
            .collect::<rusqlite::Result<_>>()
            .expect("rows");
        assert_eq!(retired, [(kept, false), (gone, true)]);
    }

    #[test]
    fn reloading_the_same_version_retires_untouched_titles() {
        let c = conn();
        let v = upsert_version(&c, &new("1", Some("gb"), 1)).expect("v").id;
        let kept = title(&c, v, "Kept");
        let gone = title(&c, v, "Gone");
        assert_eq!(begin_load(&c, v).expect("begin"), 2);
        c.execute("UPDATE titles SET retired = 0 WHERE id = ?1", [kept])
            .expect("touch");
        assert_eq!(retire_absent(&c, v).expect("retire"), 1);
        let retired: Vec<(i64, i64)> = c
            .prepare("SELECT id, retired FROM titles ORDER BY id")
            .expect("prepare")
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .expect("query")
            .collect::<rusqlite::Result<_>>()
            .expect("rows");
        assert_eq!(retired, [(kept, 0), (gone, 1)]);
    }

    #[test]
    fn retire_marks_the_version_and_its_titles() {
        let c = conn();
        let v = upsert_version(&c, &new("1", Some("gb"), 1)).expect("v").id;
        title(&c, v, "One");
        let before = retire(&c, v).expect("retire").expect("row");
        assert!(!before.retired);
        assert!(get(&c, v).expect("get").expect("row").retired);
        let live: i64 = c
            .query_row("SELECT COUNT(*) FROM titles WHERE retired = 0", [], |r| {
                r.get(0)
            })
            .expect("count");
        assert_eq!(live, 0);
        let v2 = upsert_version(&c, &new("0", None, 2)).expect("v2");
        assert!(v2.current, "a retired version does not supersede");
        assert_eq!(DatVersionId(4).to_string(), "4");
    }
}
