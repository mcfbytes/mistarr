//! The `platforms` table, seeded from `mistarr_mister::platforms::PLATFORMS`.

use mistarr_core::{PlatformId, PlatformKey};
use mistarr_mister::Platform;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use super::sql::code_enum;
use crate::error::Result;

code_enum! {
    /// `platforms.kind`, the code of the platform table's `Kind`.
    pub enum KindCode {
        /// One file per game.
        Cartridge = ("cartridge", 0),
        /// A cue with tracks or a single image.
        Disc = ("disc", 1),
        /// A zip or directory per game.
        Romset = ("romset", 2),
        /// MAME zips requested by MRA files.
        Arcade = ("arcade", 3),
    }
}

/// The slugs that have a fixed key, in key order: slug number `n` has key `n + 1`. A
/// platform added to the binary's table is appended here, and no key is ever reused.
const FIXED_KEYS: [&str; 33] = [
    "nes",
    "fds",
    "snes",
    "n64",
    "gb",
    "gbc",
    "gba",
    "megadrive",
    "s32x",
    "sms",
    "gg",
    "sg1000",
    "pce",
    "sgx",
    "atari2600",
    "atari5200",
    "atari7800",
    "lynx",
    "coleco",
    "intv",
    "ws",
    "wsc",
    "ngp",
    "vectrex",
    "pokemini",
    "sv",
    "psx",
    "saturn",
    "megacd",
    "pcecd",
    "neocd",
    "neogeo",
    "arcade",
];

/// The fixed key of `slug`, or `None` for a slug the binary's table does not list.
///
/// ```
/// use mistarr_server::db::platforms::fixed_key;
/// assert_eq!(fixed_key("nes").map(|k| k.get()), Some(1));
/// assert_eq!(fixed_key("arcade").map(|k| k.get()), Some(33));
/// assert!(fixed_key("other").is_none());
/// ```
#[must_use]
pub fn fixed_key(slug: &str) -> Option<PlatformKey> {
    let at = FIXED_KEYS.iter().position(|s| *s == slug)?;
    Some(PlatformKey::new(i64::try_from(at).ok()? + 1))
}

/// A platform as one request holds it: the slug that titles, the API and events use,
/// and the key that converted tables use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformRef {
    /// The slug.
    pub id: PlatformId,
    /// The key.
    pub key: PlatformKey,
}

impl PlatformRef {
    /// The platform `slug` names, or `None` when there is no such row.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Db`] on SQLite failure.
    ///
    /// ```
    /// use mistarr_core::PlatformId;
    /// use mistarr_server::db::platforms::PlatformRef;
    /// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
    /// mistarr_server::db::migrate::apply(&mut conn).unwrap();
    /// assert!(PlatformRef::resolve(&conn, &PlatformId::new("nes")).unwrap().is_none());
    /// ```
    pub fn resolve(conn: &Connection, slug: &PlatformId) -> Result<Option<Self>> {
        Ok(key(conn, slug)?.map(|key| Self {
            id: slug.clone(),
            key,
        }))
    }

    /// The platform with `key`, or `None` when there is no such row.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Db`] on SQLite failure.
    ///
    /// ```
    /// use mistarr_core::PlatformKey;
    /// use mistarr_server::db::platforms::PlatformRef;
    /// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
    /// mistarr_server::db::migrate::apply(&mut conn).unwrap();
    /// assert!(PlatformRef::by_key(&conn, PlatformKey::new(1)).unwrap().is_none());
    /// ```
    pub fn by_key(conn: &Connection, key: PlatformKey) -> Result<Option<Self>> {
        conn.prepare_cached("SELECT slug FROM platforms WHERE id = ?1")?
            .query_row([key], |r| r.get(0))
            .optional()
            .map(|id| id.map(|id| Self { id, key }))
            .map_err(Into::into)
    }
}

/// The key of the platform `slug` names, or `None` when there is no such row. A request
/// resolves its platform once and passes the key to the `db` functions.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_core::PlatformId;
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert!(mistarr_server::db::platforms::key(&conn, &PlatformId::new("nes")).unwrap().is_none());
/// ```
pub fn key(conn: &Connection, slug: &PlatformId) -> Result<Option<PlatformKey>> {
    conn.prepare_cached("SELECT id FROM platforms WHERE slug = ?1")?
        .query_row([slug], |r| r.get(0))
        .optional()
        .map_err(Into::into)
}

/// One `platforms` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlatformRow {
    /// Stable slug.
    pub id: PlatformId,
    /// The integer key converted tables hold.
    #[serde(skip)]
    pub key: PlatformKey,
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
            "INSERT INTO platforms (id, slug, name, core_dir, kind) VALUES (
               COALESCE(?1, MAX(COALESCE((SELECT MAX(id) FROM platforms), 0), 999) + 1),
               ?2, ?3, ?4, ?5)
             ON CONFLICT(slug) DO UPDATE SET
               name = excluded.name, core_dir = excluded.core_dir, kind = excluded.kind",
        )?;
        for p in table {
            let kind = KindCode::parse(p.kind.as_str());
            stmt.execute(params![fixed_key(p.id), p.id, p.name, p.core_dir, kind])?;
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

const COLUMNS: &str = "slug, id, name, core_dir, kind, core_present, enabled";

fn from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<PlatformRow> {
    Ok(PlatformRow {
        id: r.get(0)?,
        key: r.get(1)?,
        name: r.get(2)?,
        core_dir: r.get(3)?,
        kind: r.get::<_, KindCode>(4)?.as_str().to_owned(),
        core_present: r.get(5)?,
        enabled: r.get(6)?,
    })
}

/// Every platform, ordered by slug.
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
    let mut stmt = conn.prepare(&format!("SELECT {COLUMNS} FROM platforms ORDER BY slug"))?;
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
/// assert!(mistarr_server::db::platforms::find(&conn, &PlatformId::new("nes")).unwrap().is_none());
/// ```
pub fn find(conn: &Connection, id: &PlatformId) -> Result<Option<PlatformRow>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM platforms WHERE slug = ?1"),
        [id],
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
/// mistarr_server::db::platforms::set_core_present(&conn, &[PlatformId::new("nes")]).unwrap();
/// ```
pub fn set_core_present(conn: &Connection, present: &[PlatformId]) -> Result<()> {
    conn.execute("UPDATE platforms SET core_present = 0", [])?;
    {
        let mut stmt = conn.prepare("UPDATE platforms SET core_present = 1 WHERE slug = ?1")?;
        for id in present {
            stmt.execute([id])?;
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
/// let nes = mistarr_core::PlatformId::new("nes");
/// assert!(!mistarr_server::db::platforms::set_enabled(&conn, &nes, false).unwrap());
/// ```
pub fn set_enabled(conn: &Connection, id: &PlatformId, enabled: bool) -> Result<bool> {
    let n = conn.execute(
        "UPDATE platforms SET enabled = ?2 WHERE slug = ?1",
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
        let nes = rows.iter().find(|r| r.id.as_str() == "nes").expect("nes");
        assert_eq!(nes.core_dir, "NES");
        assert_eq!(nes.kind, "cartridge");
        assert!(nes.enabled && !nes.core_present);
    }

    #[test]
    fn core_presence_is_replaced_and_survives_reseed() {
        let mut c = conn();
        seed(&mut c, &PLATFORMS).expect("seed");
        set_core_present(&c, &[PlatformId::new("nes")]).expect("set");
        set_core_present(&c, &[PlatformId::new("snes")]).expect("set");
        seed(&mut c, &PLATFORMS).expect("reseed");
        let present: Vec<_> = list(&c)
            .expect("list")
            .into_iter()
            .filter(|r| r.core_present)
            .map(|r| r.id.as_str().to_owned())
            .collect();
        assert_eq!(present, ["snes"]);
    }

    #[test]
    fn enabled_toggles() {
        let mut c = conn();
        seed(&mut c, &PLATFORMS).expect("seed");
        assert!(set_enabled(&c, &PlatformId::new("nes"), false).expect("set"));
        assert!(!set_enabled(&c, &PlatformId::new("nope"), false).expect("set"));
        let nes = find(&c, &PlatformId::new("nes"))
            .expect("find")
            .expect("row");
        assert!(!nes.enabled);
    }

    #[test]
    fn find_returns_the_row_or_none() {
        let mut c = conn();
        seed(&mut c, &PLATFORMS).expect("seed");
        let nes = find(&c, &PlatformId::new("nes"))
            .expect("find")
            .expect("row");
        assert_eq!(nes.core_dir, "NES");
        assert!(find(&c, &PlatformId::new("no-such-platform"))
            .expect("find")
            .is_none());
    }

    #[test]
    fn fixed_keys_follow_the_binary_table_in_order() {
        assert_eq!(FIXED_KEYS.len(), PLATFORMS.len());
        for (at, p) in PLATFORMS.iter().enumerate() {
            assert_eq!(FIXED_KEYS[at], p.id, "slot {at}");
            let key = fixed_key(p.id).expect("fixed");
            assert_eq!(key.get(), i64::try_from(at).expect("slot") + 1);
            assert!(KindCode::parse(p.kind.as_str()).is_some(), "{}", p.id);
        }
        assert!(fixed_key("nowhere").is_none());
    }

    #[test]
    fn seed_inserts_a_platform_with_its_fixed_key() {
        let mut c = conn();
        seed(&mut c, &PLATFORMS).expect("seed");
        for row in list(&c).expect("list") {
            assert_eq!(Some(row.key), fixed_key(row.id.as_str()), "{}", row.id);
            assert_eq!(key(&c, &row.id).expect("key"), Some(row.key));
        }
        assert!(key(&c, &PlatformId::new("nowhere")).expect("key").is_none());
        let psx = find(&c, &PlatformId::new("psx"))
            .expect("find")
            .expect("row");
        assert_eq!(psx.kind, "disc");
    }

    #[test]
    fn a_platform_ref_resolves_both_ways() {
        let mut c = conn();
        seed(&mut c, &PLATFORMS).expect("seed");
        let nes = PlatformRef::resolve(&c, &PlatformId::new("nes"))
            .expect("resolve")
            .expect("row");
        assert_eq!(nes.key.get(), 1);
        assert_eq!(PlatformRef::by_key(&c, nes.key).expect("by key"), Some(nes));
        assert!(PlatformRef::resolve(&c, &PlatformId::new("nowhere"))
            .expect("resolve")
            .is_none());
        assert!(PlatformRef::by_key(&c, PlatformKey::new(500))
            .expect("by key")
            .is_none());
    }

    #[test]
    fn a_platform_without_a_fixed_key_takes_the_next_key_from_1000() {
        let mut c = conn();
        let odd = Platform {
            id: "odd",
            ..PLATFORMS[0]
        };
        let other = Platform {
            id: "odder",
            ..PLATFORMS[0]
        };
        seed(&mut c, &PLATFORMS).expect("seed");
        seed(&mut c, &[odd, other]).expect("odd");
        let keys = |slug: &str| {
            key(&c, &PlatformId::new(slug.to_owned()))
                .expect("key")
                .map(PlatformKey::get)
        };
        assert_eq!((keys("odd"), keys("odder")), (Some(1000), Some(1001)));
    }
}
