//! The `dat_versions` table and supersession; see `docs/ARCHITECTURE.md` "DAT import".

use std::fmt;

use mistarr_core::dat::family_key;
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
    /// The family key of `dat_name`: versions and forms of one list share it.
    pub family: String,
    /// Why the version is not current, when it is not.
    pub reason: Option<String>,
}

const COLUMNS: &str = "d.id, d.platform_id, d.dat_name, d.version, d.source_file, d.loaded_at,
    d.superseded_by, d.game_count, d.retired, d.family, s.dat_name, s.version, s.loaded_at
    FROM dat_versions d LEFT JOIN dat_versions s ON s.id = d.superseded_by";

fn from_row(r: &Row<'_>) -> rusqlite::Result<DatVersionRow> {
    let dat_name: String = r.get(2)?;
    let loaded_at: i64 = r.get(5)?;
    let retired: bool = r.get(8)?;
    let by: Option<(String, String, i64)> = match r.get::<_, Option<String>>(10)? {
        Some(name) => Some((name, r.get(11)?, r.get(12)?)),
        None => None,
    };
    let reason = if retired {
        Some("Removed; its games are no longer listed".to_owned())
    } else {
        by.map(|(name, version, at)| {
            let other = if name == dat_name {
                format!("version {version}")
            } else {
                format!("{name} version {version}")
            };
            if loaded_at > at {
                format!("Older than {other}, which stays current")
            } else {
                format!("Replaced by {other}")
            }
        })
    };
    Ok(DatVersionRow {
        id: DatVersionId(r.get(0)?),
        platform_id: r.get::<_, Option<String>>(1)?.map(PlatformId),
        dat_name,
        version: r.get(3)?,
        source_file: r.get(4)?,
        loaded_at,
        superseded_by: r.get::<_, Option<i64>>(6)?.map(DatVersionId),
        game_count: unsigned(r.get(7)?),
        retired,
        family: r.get(9)?,
        reason,
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
    /// Platform from binding; `None` inherits the newest bound version of the same family.
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
    /// True when this version is now the newest of its family on its platform; its titles
    /// should be loaded.
    pub current: bool,
}

/// What [`upsert_version`] reads before it writes.
struct Decision {
    family: String,
    existing: Option<i64>,
    inherited: Option<String>,
    platform: Option<String>,
    current: bool,
    superseded_by: Option<i64>,
}

fn decide(conn: &Connection, v: &NewVersion<'_>) -> Result<Decision> {
    let family = family_key(v.dat_name).0;
    let existing: Option<(i64, Option<String>)> = conn
        .query_row(
            "SELECT id, platform_id FROM dat_versions
             WHERE dat_name = ?1 AND version = ?2 AND source = 'dat'",
            params![v.dat_name, v.version],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let inherited: Option<String> = match v.platform {
        Some(p) => Some(p.to_owned()),
        None => conn
            .query_row(
                "SELECT platform_id FROM dat_versions
                 WHERE family = ?1 AND platform_id IS NOT NULL AND source = 'dat'
                 ORDER BY dat_name = ?2 DESC, loaded_at DESC, id DESC LIMIT 1",
                params![family, v.dat_name],
                |r| r.get(0),
            )
            .optional()?,
    };
    let (existing, stored) = existing.map_or((None, None), |(id, p)| (Some(id), p));
    let platform = inherited.clone().or(stored);
    let newest: Option<(i64, String)> = conn
        .query_row(
            "SELECT id, version FROM dat_versions
             WHERE family = ?1 AND (platform_id IS ?2 OR platform_id IS NULL OR ?2 IS NULL)
               AND superseded_by IS NULL AND retired = 0
               AND id IS NOT ?3 AND source = 'dat'
             ORDER BY loaded_at DESC, id DESC LIMIT 1",
            params![family, platform, existing],
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
    Ok(Decision {
        family,
        existing,
        inherited,
        platform,
        current,
        superseded_by,
    })
}

/// What [`upsert_version`] would decide for `v`, without writing: the platform
/// the version would be bound to and whether it would be current.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::dats::{plan_version, NewVersion};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let v = NewVersion { dat_name: "Test Console", version: "1", source_file: "a.dat", platform: None, now: 1 };
/// assert_eq!(plan_version(&conn, &v).unwrap(), (None, true));
/// ```
pub fn plan_version(conn: &Connection, v: &NewVersion<'_>) -> Result<(Option<PlatformId>, bool)> {
    let d = decide(conn, v)?;
    Ok((d.platform.map(PlatformId), d.current))
}

/// Inserts or refreshes the `(dat_name, version)` row and applies supersession within
/// its family on its platform: a version whose string is not below the newest live one
/// becomes current and supersedes the others, whichever form they came in; an older one
/// is stored superseded by the current one.
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
    let Decision {
        family,
        existing,
        inherited,
        current,
        superseded_by,
        ..
    } = decide(conn, v)?;
    let id = if let Some(id) = existing {
        conn.execute(
            "UPDATE dat_versions SET source_file = ?2, loaded_at = ?3,
               platform_id = COALESCE(?4, platform_id), superseded_by = ?5, retired = 0,
               family = ?6
             WHERE id = ?1",
            params![id, v.source_file, v.now, inherited, superseded_by, family],
        )?;
        id
    } else {
        conn.execute(
            "INSERT INTO dat_versions (platform_id, dat_name, version, source_file, loaded_at,
               superseded_by, game_count, family)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7)",
            params![
                inherited,
                v.dat_name,
                v.version,
                v.source_file,
                v.now,
                superseded_by,
                family
            ],
        )?;
        conn.last_insert_rowid()
    };
    let platform_id: Option<String> = conn.query_row(
        "SELECT platform_id FROM dat_versions WHERE id = ?1",
        [id],
        |r| r.get(0),
    )?;
    if current {
        conn.execute(
            "UPDATE dat_versions SET superseded_by = ?1
             WHERE family = ?2 AND (platform_id IS ?3 OR platform_id IS NULL OR ?3 IS NULL)
               AND id != ?1 AND superseded_by IS NULL
               AND source = 'dat'",
            params![id, family, platform_id],
        )?;
    }
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
        "UPDATE titles SET retired = 2 WHERE dat_version_id = ?1 AND retired = 0 AND source = 'dat'",
        [id.0],
    )?)
}

/// Marks retired the titles a load of `id` did not list: those left on other versions
/// of its family on its platform and those still pending from [`begin_load`]. Returns how many.
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
         WHERE retired = 0 AND source = 'dat' AND dat_version_id IN (
           SELECT o.id FROM dat_versions o
           JOIN dat_versions n ON n.family = o.family AND n.platform_id IS o.platform_id
           WHERE n.id = ?1 AND o.id != ?1 AND o.source = 'dat')",
        [id.0],
    )?;
    let pending = conn.execute(
        "UPDATE titles SET retired = 1, is_1g1r_pick = 0 WHERE dat_version_id = ?1 AND retired = 2 AND source = 'dat'",
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
        "UPDATE titles SET retired = 1, is_1g1r_pick = 0 WHERE dat_version_id = ?1 AND source = 'dat'",
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
            &format!("SELECT {COLUMNS} WHERE d.id = ?1 AND d.source = 'dat'"),
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
    let total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM dat_versions WHERE source = 'dat'",
        [],
        |r| r.get(0),
    )?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} WHERE d.source = 'dat' ORDER BY d.loaded_at DESC, d.id DESC
         LIMIT ?1 OFFSET ?2"
    ))?;
    let rows = stmt
        .query_map(params![limit, offset], from_row)?
        .collect::<rusqlite::Result<_>>()?;
    Ok((rows, unsigned(total)))
}

/// Stores the family key of every version whose stored key differs from the current rule;
/// run at open so the key follows [`mistarr_core::dat::FORMAT_MARKERS`]. Returns how many.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::dats;
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// conn.execute("INSERT INTO dat_versions (dat_name, version, source_file, loaded_at, game_count)
///   VALUES ('Example (Headered)', '1', 'a.dat', 1, 0)", []).unwrap();
/// assert_eq!(dats::refresh_families(&conn).unwrap(), 1);
/// assert_eq!(dats::refresh_families(&conn).unwrap(), 0);
/// ```
pub fn refresh_families(conn: &Connection) -> Result<usize> {
    let rows: Vec<(i64, String, String)> = conn
        .prepare("SELECT id, dat_name, family FROM dat_versions")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut stmt = conn.prepare("UPDATE dat_versions SET family = ?2 WHERE id = ?1")?;
    let mut changed = 0;
    for (id, name, stored) in rows {
        let key = family_key(&name).0;
        if key != stored {
            changed += stmt.execute(params![id, key])?;
        }
    }
    Ok(changed)
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

    fn named<'a>(dat_name: &'a str, version: &'a str, now: i64) -> NewVersion<'a> {
        NewVersion {
            dat_name,
            ..new(version, Some("nes"), now)
        }
    }

    fn live(c: &Connection) -> Vec<String> {
        c.prepare(
            "SELECT dat_name || ' ' || version FROM dat_versions
             WHERE superseded_by IS NULL AND retired = 0 ORDER BY id",
        )
        .expect("prepare")
        .query_map([], |r| r.get(0))
        .expect("query")
        .collect::<rusqlite::Result<_>>()
        .expect("rows")
    }

    #[test]
    fn a_family_keeps_one_current_version_per_platform() {
        let c = conn();
        let logiqx = "Example Vendor - Example System (Headered)";
        let export = "Example Vendor - Example System (DB Export)";
        let samples = "Example Samples - Example System (Headered)";
        let first = upsert_version(&c, &named(logiqx, "20260101", 1)).expect("first");
        upsert_version(&c, &named(logiqx, "20260201", 2)).expect("refresh");
        assert_eq!(
            live(&c),
            [format!("{logiqx} 20260201")],
            "a refresh supersedes"
        );
        assert_eq!(
            get(&c, first.id)
                .expect("get")
                .expect("row")
                .reason
                .as_deref(),
            Some("Replaced by version 20260201")
        );
        upsert_version(&c, &named(samples, "1", 3)).expect("add-on");
        let replaced = upsert_version(&c, &named(export, "20260301", 4)).expect("export");
        assert!(replaced.current);
        assert_eq!(
            live(&c),
            [format!("{samples} 1"), format!("{export} 20260301")],
            "the export replaces the Logiqx form and the add-on stays"
        );
        let late = upsert_version(&c, &named(logiqx, "20260215", 5)).expect("late");
        assert!(
            !late.current,
            "an older version never supersedes a newer one"
        );
        let row = get(&c, late.id).expect("get").expect("row");
        assert_eq!(row.superseded_by, Some(replaced.id));
        assert_eq!(
            row.reason.as_deref(),
            Some(format!("Older than {export} version 20260301, which stays current").as_str())
        );
        assert_eq!(row.family, "example vendor - example system");
        let other = upsert_version(
            &c,
            &NewVersion {
                platform: Some("gb"),
                ..named(logiqx, "20260401", 6)
            },
        )
        .expect("other platform");
        assert!(other.current);
        assert!(
            get(&c, replaced.id)
                .expect("get")
                .expect("row")
                .superseded_by
                .is_none(),
            "a family on another platform supersedes nothing here"
        );
        retire(&c, replaced.id).expect("retire");
        assert_eq!(
            get(&c, replaced.id)
                .expect("get")
                .expect("row")
                .reason
                .as_deref(),
            Some("Removed; its games are no longer listed")
        );
        assert!(
            get(&c, late.id)
                .expect("get")
                .expect("row")
                .superseded_by
                .is_some(),
            "removing the current version does not bring back an older one"
        );
    }

    #[test]
    fn families_are_refreshed_from_names() {
        let c = conn();
        c.execute(
            "INSERT INTO dat_versions (dat_name, version, source_file, loaded_at, game_count)
             VALUES ('Example (DB Export)', '1', 'a.xml', 1, 0)",
            [],
        )
        .expect("insert");
        assert_eq!(refresh_families(&c).expect("refresh"), 1);
        let (rows, _) = list(&c, 10, 0).expect("list");
        assert_eq!(rows[0].family, "example");
        assert_eq!(rows[0].reason, None);
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
