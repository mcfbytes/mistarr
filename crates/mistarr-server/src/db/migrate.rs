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

/// The highest migration this binary embeds.
///
/// ```
/// let latest = mistarr_server::db::migrate::latest();
/// assert_eq!(Some(latest), mistarr_server::db::migrate::MIGRATIONS.last().map(|m| m.version));
/// ```
#[must_use]
pub fn latest() -> u32 {
    MIGRATIONS.last().map_or(0, |m| m.version)
}

/// The recorded schema version, without creating anything: 0 when `schema_version` is absent.
///
/// # Errors
///
/// [`Error::Db`] when the database cannot be read.
///
/// ```
/// let conn = rusqlite::Connection::open_in_memory().unwrap();
/// assert_eq!(mistarr_server::db::migrate::recorded_version(&conn).unwrap(), 0);
/// ```
pub fn recorded_version(conn: &Connection) -> Result<u32> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'schema_version')",
        [],
        |r| r.get(0),
    )?;
    if exists {
        current_version(conn)
    } else {
        Ok(0)
    }
}

/// The recorded and latest versions of the database at `path` when it exists, has
/// been migrated before and has migrations to apply; opened read-only, never created.
///
/// # Errors
///
/// [`Error::Db`] when the file exists but cannot be read.
///
/// ```
/// let dir = tempfile::tempdir().unwrap();
/// let path = dir.path().join("m.db");
/// assert_eq!(mistarr_server::db::migrate::pending(&path).unwrap(), None);
/// drop(mistarr_server::db::Db::open(&path).unwrap());
/// assert_eq!(mistarr_server::db::migrate::pending(&path).unwrap(), None);
/// ```
pub fn pending(path: &std::path::Path) -> Result<Option<(u32, u32)>> {
    if !path.is_file() {
        return Ok(None);
    }
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let found = recorded_version(&conn)?;
    Ok((found > 0 && found < latest()).then_some((found, latest())))
}

/// Refuses a database a newer mistarr migrated, reading only; returns its recorded version.
///
/// # Errors
///
/// [`Error::SchemaTooNew`] when the recorded version exceeds [`latest`], and
/// [`Error::Db`] when it cannot be read.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let v = mistarr_server::db::migrate::check_supported(&conn).unwrap();
/// assert_eq!(v, mistarr_server::db::migrate::latest());
/// ```
pub fn check_supported(conn: &Connection) -> Result<u32> {
    let found = recorded_version(conn)?;
    let supported = latest();
    if found > supported {
        return Err(Error::SchemaTooNew { found, supported });
    }
    Ok(found)
}

/// Applies each migration not yet recorded, in order, one transaction each,
/// and returns the versions applied by this call.
///
/// # Errors
///
/// [`Error::SchemaTooNew`] before any write when a newer mistarr migrated the database;
/// [`Error::Migration`] names the first migration that failed; earlier ones stay applied.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// let applied = mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert_eq!(applied.first(), Some(&1));
/// assert!(mistarr_server::db::migrate::apply(&mut conn).unwrap().is_empty());
/// ```
pub fn apply(conn: &mut Connection) -> Result<Vec<u32>> {
    check_supported(conn)?;
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
        flush_groups(&tx).map_err(|e| match e {
            Error::Db(source) => fail(source),
            other => other,
        })?;
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

/// Builds the `title_groups` rows a migration's writes left dirty, once the table exists.
fn flush_groups(conn: &Connection) -> Result<()> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'title_groups_dirty')",
        [],
        |r| r.get(0),
    )?;
    if exists {
        super::groups::flush(conn)?;
    }
    Ok(())
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
    fn schema_has_every_table_and_only_the_search_source_view() {
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
            "title_groups",
            "title_groups_dirty",
            "titles",
            "torrent_files",
        ] {
            assert!(tables.iter().any(|n| n == t), "missing table {t}");
        }
        assert_eq!(names(&conn, "view"), ["title_search_source"]);
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM title_groups", [], |r| r.get(0))
            .expect("group query");
        assert_eq!(rows, 0);
    }

    #[test]
    fn arcade_scan_cleanup_prunes_scan_noise_but_keeps_import_rows() {
        let mut conn = Connection::open_in_memory().expect("open");
        apply(&mut conn).expect("apply");
        crate::db::platforms::seed(&mut conn, &mistarr_mister::platforms::PLATFORMS).expect("seed");
        // An MRA zip rom, as import and presence rows link `files.rom_id` to.
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
        // Library scan rows that matched no rom: rom_id is NULL.
        insert("mame/exampleset.zip#a.bin", "unverified", None);
        insert("hbmame/otherset.zip#b.bin", "unverified", None);
        // A library scan's row for a zip it could not open: bare path, no rom.
        insert("mame/unreadable.zip", "unverified", None);
        // Import rows: rom_id always set.
        insert("mame/exampleset.zip#c.bin", "unverified", Some(rom_id));
        insert("mame/exampleset.zip#d.bin", "verified", Some(rom_id));
        // Another platform's unmatched file is untouched.
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
    fn arcade_presence_migration_indexes_and_drops_md5_less_member_rows() {
        let mut conn = Connection::open_in_memory().expect("open");
        apply(&mut conn).expect("apply");
        crate::db::platforms::seed(&mut conn, &mistarr_mister::platforms::PLATFORMS).expect("seed");
        let insert = |platform: &str, rel_path: &str, md5: Option<&str>| -> i64 {
            conn.execute(
                "INSERT INTO files (platform_id, rel_path, size, mtime, md5, state, scanned_at)
                 VALUES (?1, ?2, 4, 0, ?3, 'verified', 0)",
                params![platform, rel_path, md5],
            )
            .expect("insert");
            conn.last_insert_rowid()
        };
        let stale = insert("arcade", "mame/a.zip#a.bin", None);
        insert(
            "arcade",
            "mame/a.zip#b.bin",
            Some("0123456789abcdef0123456789abcdef"),
        );
        insert("arcade", "mame/b.zip", None);
        insert("arcade", "mame/a#b.zip", None);
        insert("arcade", "mame/C.ZIP#c.bin", None);
        insert("nes", "NES/c.zip#c.nes", None);
        conn.execute(
            "INSERT INTO import_log (at, file_id, action, detail) VALUES (1, ?1, 'placed', '{}')",
            [stale],
        )
        .expect("log");

        let presence = MIGRATIONS
            .iter()
            .find(|m| m.name == "0013_arcade_presence")
            .expect("migration present");
        let rerun = presence
            .sql
            .replace("CREATE INDEX", "CREATE INDEX IF NOT EXISTS");
        conn.execute_batch(&rerun).expect("migrate");

        let left: Vec<String> = conn
            .prepare("SELECT rel_path FROM files ORDER BY rel_path")
            .expect("prepare")
            .query_map([], |r| r.get(0))
            .expect("query")
            .collect::<rusqlite::Result<_>>()
            .expect("rows");
        assert_eq!(
            left,
            [
                "NES/c.zip#c.nes",
                "mame/a#b.zip",
                "mame/a.zip#b.bin",
                "mame/b.zip"
            ],
            "a presence row whose zip name holds `#` stays; members of any case go"
        );
        let logged: Option<i64> = conn
            .query_row("SELECT file_id FROM import_log", [], |r| r.get(0))
            .expect("log");
        assert_eq!(logged, None, "the log entry outlives the row");
        let plan: String = conn
            .query_row(
                "EXPLAIN QUERY PLAN SELECT id FROM import_log WHERE file_id = 1",
                [],
                |r| r.get(3),
            )
            .expect("plan");
        assert!(plan.contains("import_log_file"), "{plan}");
    }

    #[test]
    fn rom_indexes_a_load_need_not_touch_are_partial_and_the_stage_is_gone() {
        let mut conn = Connection::open_in_memory().expect("open");
        apply(&mut conn).expect("apply");
        let sql = |name: &str| -> String {
            conn.query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = ?1",
                [name],
                |r| r.get(0),
            )
            .expect(name)
        };
        assert!(sql("roms_md5").ends_with("WHERE sha1 IS NULL"));
        assert!(sql("roms_size").ends_with("WHERE match_base IS NOT NULL"));
        assert!(sql("roms_match_base").ends_with("WHERE match_base IS NOT NULL"));
        assert!(!sql("roms_sha1").contains("WHERE"));
        assert!(!names(&conn, "table").iter().any(|n| n == "dat_stage"));
        conn.execute_batch(
            "INSERT INTO platforms (id, name, core_dir, kind) VALUES ('p', 'P', 'P', 'cartridge');
             INSERT INTO dat_versions (platform_id, dat_name, version, source_file, loaded_at, game_count)
               VALUES ('p', 'd', '1', 'd.dat', 0, 1);
             INSERT INTO titles (platform_id, dat_version_id, name, base_name) VALUES ('p', 1, 't', 't');
             INSERT INTO roms (title_id, name, size, md5, sha1) VALUES (1, 'a', 4, 'm1', 's1');
             INSERT INTO roms (title_id, name, size, md5) VALUES (1, 'b', 4, 'm2');",
        )
        .expect("rows");
        let entries: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM roms INDEXED BY roms_md5 WHERE sha1 IS NULL AND md5 > ''",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(entries, 1, "only the sha1-less rom is in the md5 index");
    }

    #[test]
    fn an_older_schema_reports_its_pending_migrations() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("m.db");
        drop(crate::db::Db::open(&path).expect("open"));
        let conn = Connection::open(&path).expect("open");
        conn.execute("DELETE FROM schema_version WHERE version = ?1", [latest()])
            .expect("forget the last migration");
        drop(conn);
        assert_eq!(
            pending(&path).expect("pending"),
            Some((latest() - 1, latest()))
        );
    }

    #[test]
    fn a_newer_schema_is_refused_and_its_contents_unchanged() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("m.db");
        let newer = latest() + 1;
        {
            let mut conn = Connection::open(&path).expect("open");
            apply(&mut conn).expect("apply");
            conn.execute(
                "INSERT INTO schema_version (version, name, applied_at) VALUES (?1, 'future', 0)",
                [newer],
            )
            .expect("record");
        }
        let contents = || -> Vec<String> {
            let conn = Connection::open(&path).expect("open");
            let mut stmt = conn
                .prepare(
                    "SELECT 'v' || version || name FROM schema_version
                     UNION ALL SELECT 's' || type || name || COALESCE(sql, '') FROM sqlite_master
                     ORDER BY 1",
                )
                .expect("prepare");
            stmt.query_map([], |r| r.get(0))
                .expect("query")
                .collect::<rusqlite::Result<_>>()
                .expect("rows")
        };
        let before = contents();
        match crate::db::Db::open(&path) {
            Err(Error::SchemaTooNew { found, supported }) => {
                assert_eq!((found, supported), (newer, latest()));
            }
            Err(other) => panic!("expected SchemaTooNew, got {other:?}"),
            Ok(_) => panic!("a newer schema must not open"),
        }
        assert_eq!(contents(), before);
        let msg = Error::SchemaTooNew {
            found: newer,
            supported: latest(),
        }
        .to_string();
        assert!(msg.contains(&newer.to_string()) && msg.contains("mistarr.db.prev"));
    }

    #[test]
    fn an_equal_schema_opens_normally() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("m.db");
        drop(crate::db::Db::open(&path).expect("first open"));
        let db = crate::db::Db::open(&path).expect("second open");
        let v = db.read_blocking(check_supported).expect("check");
        assert_eq!(v, latest());
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
