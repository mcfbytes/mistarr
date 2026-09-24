//! Numbered migrations from `migrations/`, recorded in `schema_version`.

use rusqlite::{params, Connection};

use crate::error::{Error, Result};

/// One file from `migrations/`, named `NNNN_name.sql`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Migration {
    /// The number the file name starts with.
    pub version: u32,
    /// The file stem.
    pub name: &'static str,
    /// The file's SQL.
    pub sql: &'static str,
}

/// Every migration, in ascending version order.
pub const MIGRATIONS: &[Migration] = include!(concat!(env!("OUT_DIR"), "/migrations.rs"));

/// Applies each migration not yet recorded, in order, one transaction each,
/// and returns the versions applied by this call.
///
/// # Errors
///
/// [`Error::Migration`] names the first migration that failed; earlier ones stay applied.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// let applied = mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert_eq!(applied.first(), Some(&1));
/// assert!(mistarr_server::db::migrate::apply(&mut conn).unwrap().is_empty());
/// ```
pub fn apply(conn: &mut Connection) -> Result<Vec<u32>> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_version (
           version    INTEGER PRIMARY KEY,
           name       TEXT NOT NULL,
           applied_at INTEGER NOT NULL
         );",
    )?;
    let current = current_version(conn)?;
    let mut applied = Vec::new();
    for m in MIGRATIONS.iter().filter(|m| m.version > current) {
        let fail = |source| Error::Migration {
            version: m.version,
            source,
        };
        let tx = conn.transaction().map_err(fail)?;
        tx.execute_batch(m.sql).map_err(fail)?;
        tx.execute(
            "INSERT INTO schema_version (version, name, applied_at) VALUES (?1, ?2, ?3)",
            params![m.version, m.name, crate::unix_now()],
        )
        .map_err(fail)?;
        tx.commit().map_err(fail)?;
        tracing::info!(version = m.version, name = m.name, "applied migration");
        applied.push(m.version);
    }
    Ok(applied)
}

/// The highest recorded migration, or 0 for a new database.
///
/// # Errors
///
/// [`Error::Db`] when `schema_version` cannot be read.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert!(mistarr_server::db::migrate::current_version(&conn).unwrap() >= 1);
/// ```
pub fn current_version(conn: &Connection) -> Result<u32> {
    Ok(conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_version",
        [],
        |r| r.get(0),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(conn: &Connection, kind: &str) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = ?1 ORDER BY name")
            .expect("prepare");
        stmt.query_map([kind], |r| r.get(0))
            .expect("query")
            .collect::<rusqlite::Result<_>>()
            .expect("rows")
    }

    #[test]
    fn migrations_are_numbered_in_order() {
        assert!(!MIGRATIONS.is_empty());
        assert_eq!(MIGRATIONS[0].version, 1);
        assert_eq!(MIGRATIONS[0].name, "0001_initial");
        assert!(MIGRATIONS.windows(2).all(|w| w[0].version < w[1].version));
    }

    #[test]
    fn initial_schema_has_every_table_and_the_view() {
        let mut conn = Connection::open_in_memory().expect("open");
        apply(&mut conn).expect("apply");
        let tables = names(&conn, "table");
        for t in [
            "dat_versions",
            "downloads",
            "files",
            "import_log",
            "jobs",
            "platforms",
            "roms",
            "schema_version",
            "settings",
            "sources",
            "titles",
            "torrent_files",
        ] {
            assert!(tables.iter().any(|n| n == t), "missing table {t}");
        }
        assert_eq!(names(&conn, "view"), ["title_groups"]);
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM title_groups", [], |r| r.get(0))
            .expect("view query");
        assert_eq!(rows, 0);
    }

    #[test]
    fn arcade_scan_cleanup_prunes_scan_noise_but_keeps_import_rows() {
        let mut conn = Connection::open_in_memory().expect("open");
        apply(&mut conn).expect("apply");
        crate::db::platforms::seed(&mut conn, &mistarr_mister::platforms::PLATFORMS).expect("seed");
        // A real MRA zip rom, the kind the import path always links `files.rom_id` to.
        let version = crate::db::arcade::mra_version(&conn, "arcade", 1).expect("version");
        let title = crate::db::arcade::upsert_title(
            &conn,
            "arcade",
            version,
            &crate::db::arcade::MraTitle {
                name: "Example Blaster",
                base_name: "Example Blaster",
                group_key: "",
                regions: &[],
                languages: &[],
                revision: None,
                flags: &[],
                setname: Some("exblast"),
                rbf: Some("core"),
                mra_path: "x.mra",
                file_stamp: "1:1",
                run: 1,
            },
            &[crate::db::arcade::MraZip {
                name: "exampleset.zip",
                zip_dir: "mame",
                md5: None,
                present: true,
            }],
        )
        .expect("upsert");
        let rom_id: i64 = conn
            .query_row("SELECT id FROM roms WHERE title_id = ?1", [title.0], |r| {
                r.get(0)
            })
            .expect("rom id");

        let insert = |rel_path: &str, state: &str, rom_id: Option<i64>| {
            conn.execute(
                "INSERT INTO files (platform_id, rel_path, size, mtime, rom_id, state, scanned_at)
                 VALUES ('arcade', ?1, 4, 0, ?2, ?3, 0)",
                params![rel_path, rom_id, state],
            )
            .expect("insert");
        };
        // Noise the generic scan wrote: no rom matched, so rom_id is NULL.
        insert("mame/exampleset.zip#a.bin", "unverified", None);
        insert("hbmame/otherset.zip#b.bin", "unverified", None);
        // A zip the old scan could not open at all: one bare-path row, no `#member`.
        insert("mame/unreadable.zip", "unverified", None);
        // Rows the MRA import path legitimately records: rom_id always set.
        insert("mame/exampleset.zip#c.bin", "unverified", Some(rom_id));
        insert("mame/exampleset.zip#d.bin", "verified", Some(rom_id));
        // A non-arcade platform's stray file must survive untouched.
        conn.execute(
            "INSERT INTO files (platform_id, rel_path, size, mtime, rom_id, state, scanned_at)
             VALUES ('nes', 'NES/stray.bin', 4, 0, NULL, 'unverified', 0)",
            [],
        )
        .expect("insert");
        conn.execute(
            "INSERT INTO scan_progress (platform_id, done_dirs, updated_at)
             VALUES ('arcade', '[]', 0), ('nes', '[]', 0)",
            [],
        )
        .expect("insert progress");

        let cleanup = MIGRATIONS
            .iter()
            .find(|m| m.name == "0012_arcade_scan_cleanup")
            .expect("migration present");
        conn.execute_batch(cleanup.sql).expect("cleanup");

        let mut left: Vec<String> = conn
            .prepare("SELECT rel_path FROM files ORDER BY rel_path")
            .expect("prepare")
            .query_map([], |r| r.get(0))
            .expect("query")
            .collect::<rusqlite::Result<_>>()
            .expect("rows");
        left.sort();
        assert_eq!(
            left,
            [
                "NES/stray.bin",
                "mame/exampleset.zip#c.bin",
                "mame/exampleset.zip#d.bin",
            ]
        );

        let progress: Vec<String> = conn
            .prepare("SELECT platform_id FROM scan_progress ORDER BY platform_id")
            .expect("prepare")
            .query_map([], |r| r.get(0))
            .expect("query")
            .collect::<rusqlite::Result<_>>()
            .expect("rows");
        assert_eq!(progress, ["nes"], "arcade never resumes a scan");
    }

    #[test]
    fn apply_is_idempotent() {
        let mut conn = Connection::open_in_memory().expect("open");
        let first = apply(&mut conn).expect("first");
        assert_eq!(first.len(), MIGRATIONS.len());
        assert!(apply(&mut conn).expect("second").is_empty());
        let recorded: i64 = conn
            .query_row("SELECT COUNT(*) FROM schema_version", [], |r| r.get(0))
            .expect("count");
        assert_eq!(usize::try_from(recorded).ok(), Some(MIGRATIONS.len()));
    }

    #[test]
    fn failed_migration_is_reported_and_rolled_back() {
        let mut conn = Connection::open_in_memory().expect("open");
        conn.execute_batch("CREATE TABLE platforms (x INTEGER);")
            .expect("conflict");
        match apply(&mut conn) {
            Err(Error::Migration { version, .. }) => assert_eq!(version, 1),
            other => panic!("expected migration error, got {other:?}"),
        }
        assert_eq!(current_version(&conn).expect("version"), 0);
        assert!(!names(&conn, "table").iter().any(|n| n == "titles"));
    }
}
