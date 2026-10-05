//! The `platforms` table, seeded from `mistarr_mister::platforms::PLATFORMS`.

use mistarr_core::PlatformId;
use mistarr_mister::Platform;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use crate::error::Result;

/// One `platforms` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlatformRow {
    /// Stable slug.
    pub id: PlatformId,
    /// Display name.
    pub name: String,
    /// Directory under `games/`.
    pub core_dir: String,
    /// `cartridge`, `disc`, `romset` or `arcade`.
    pub kind: String,
    /// Whether an installed `.rbf` loads this platform.
    pub core_present: bool,
    /// Whether the user has the platform switched on.
    pub enabled: bool,
}

/// Inserts every row of `table`, refreshing name, directory and kind of rows
/// that exist and leaving `enabled` and `core_present` alone. Returns rows inserted.
///
/// # Errors
///
/// [`crate::Error::Db`] on any SQLite failure; nothing is written then.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let table = &mistarr_mister::platforms::PLATFORMS;
/// assert_eq!(mistarr_server::db::platforms::seed(&mut conn, table).unwrap(), table.len());
/// ```
pub fn seed(conn: &mut Connection, table: &[Platform]) -> Result<usize> {
    super::transact(conn, |tx| {
        let before = count(tx)?;
        let mut stmt = tx.prepare(
            "INSERT INTO platforms (id, name, core_dir, kind) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET
               name = excluded.name, core_dir = excluded.core_dir, kind = excluded.kind",
        )?;
        for p in table {
            stmt.execute(params![p.id, p.name, p.core_dir, p.kind.as_str()])?;
        }
        drop(stmt);
        Ok(count(tx)?.saturating_sub(before))
    })
}

/// Number of platform rows.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert_eq!(mistarr_server::db::platforms::count(&conn).unwrap(), 0);
/// ```
pub fn count(conn: &Connection) -> Result<usize> {
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM platforms", [], |r| r.get(0))?;
    Ok(usize::try_from(n).unwrap_or(0))
}

const COLUMNS: &str = "id, name, core_dir, kind, core_present, enabled";

fn from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<PlatformRow> {
    Ok(PlatformRow {
        id: PlatformId(r.get(0)?),
        name: r.get(1)?,
        core_dir: r.get(2)?,
        kind: r.get(3)?,
        core_present: r.get(4)?,
        enabled: r.get(5)?,
    })
}

/// Every platform, ordered by id.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert!(mistarr_server::db::platforms::list(&conn).unwrap().is_empty());
/// ```
pub fn list(conn: &Connection) -> Result<Vec<PlatformRow>> {
    let mut stmt = conn.prepare(&format!("SELECT {COLUMNS} FROM platforms ORDER BY id"))?;
    let rows = stmt.query_map([], from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// One platform by id, or `None` if it does not exist.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_core::PlatformId;
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert!(mistarr_server::db::platforms::find(&conn, &PlatformId("nes".into())).unwrap().is_none());
/// ```
pub fn find(conn: &Connection, id: &PlatformId) -> Result<Option<PlatformRow>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM platforms WHERE id = ?1"),
        [&id.0],
        from_row,
    )
    .optional()
    .map_err(Into::into)
}

/// Sets `core_present` to 1 for `present` and 0 for every other platform.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure. Run it in the caller's transaction, such as
/// [`crate::db::Db::write_tx`], so that a failure part way writes nothing.
///
/// ```
/// use mistarr_core::PlatformId;
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// mistarr_server::db::platforms::set_core_present(&conn, &[PlatformId("nes".into())]).unwrap();
/// ```
pub fn set_core_present(conn: &Connection, present: &[PlatformId]) -> Result<()> {
    conn.execute("UPDATE platforms SET core_present = 0", [])?;
    {
        let mut stmt = conn.prepare("UPDATE platforms SET core_present = 1 WHERE id = ?1")?;
        for id in present {
            stmt.execute([&id.0])?;
        }
    }
    Ok(())
}

/// Switches a platform on or off; false when there is no such platform.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert!(!mistarr_server::db::platforms::set_enabled(&conn, "nes", false).unwrap());
/// ```
pub fn set_enabled(conn: &Connection, id: &str, enabled: bool) -> Result<bool> {
    let n = conn.execute(
        "UPDATE platforms SET enabled = ?2 WHERE id = ?1",
        params![id, enabled],
    )?;
    Ok(n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mistarr_mister::platforms::PLATFORMS;

    fn conn() -> Connection {
        let mut c = Connection::open_in_memory().expect("open");
        crate::db::migrate::apply(&mut c).expect("migrate");
        c
    }

    #[test]
    fn seed_inserts_every_row_once() {
        let mut c = conn();
        assert_eq!(seed(&mut c, &PLATFORMS).expect("seed"), PLATFORMS.len());
        assert_eq!(seed(&mut c, &PLATFORMS).expect("reseed"), 0);
        let rows = list(&c).expect("list");
        assert_eq!(rows.len(), PLATFORMS.len());
        assert_eq!(count(&c).expect("count"), PLATFORMS.len());
        let nes = rows.iter().find(|r| r.id.0 == "nes").expect("nes");
        assert_eq!(nes.core_dir, "NES");
        assert_eq!(nes.kind, "cartridge");
        assert!(nes.enabled && !nes.core_present);
    }

    #[test]
    fn core_presence_is_replaced_and_survives_reseed() {
        let mut c = conn();
        seed(&mut c, &PLATFORMS).expect("seed");
        set_core_present(&c, &[PlatformId("nes".into())]).expect("set");
        set_core_present(&c, &[PlatformId("snes".into())]).expect("set");
        seed(&mut c, &PLATFORMS).expect("reseed");
        let present: Vec<_> = list(&c)
            .expect("list")
            .into_iter()
            .filter(|r| r.core_present)
            .map(|r| r.id.0)
            .collect();
        assert_eq!(present, ["snes"]);
    }

    #[test]
    fn enabled_toggles() {
        let mut c = conn();
        seed(&mut c, &PLATFORMS).expect("seed");
        assert!(set_enabled(&c, "nes", false).expect("set"));
        assert!(!set_enabled(&c, "nope", false).expect("set"));
        let nes = find(&c, &PlatformId("nes".into()))
            .expect("find")
            .expect("row");
        assert!(!nes.enabled);
    }

    #[test]
    fn find_returns_the_row_or_none() {
        let mut c = conn();
        seed(&mut c, &PLATFORMS).expect("seed");
        let nes = find(&c, &PlatformId("nes".into()))
            .expect("find")
            .expect("row");
        assert_eq!(nes.core_dir, "NES");
        assert!(find(&c, &PlatformId("no-such-platform".into()))
            .expect("find")
            .is_none());
    }
}
