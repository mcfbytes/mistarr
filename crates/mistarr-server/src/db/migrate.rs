//! Numbered migrations from `migrations/`, recorded in `schema_version`.

use std::collections::BTreeMap;

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
    if super::has_table(conn, "main", "schema_version")? {
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
    let conn = super::open_read_only(path)?;
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
    apply_list(conn, MIGRATIONS, u32::MAX)
}

/// [`apply`] stopping after migration `last`, so a test can seed the schema of that version.
///
/// # Errors
///
/// As [`apply`].
#[cfg(any(test, feature = "test-support"))]
pub fn apply_through(conn: &mut Connection, last: u32) -> Result<Vec<u32>> {
    apply_list(conn, MIGRATIONS, last)
}

/// [`apply_through`] over the migrations given, so a test can apply migrations of its own.
fn apply_list(conn: &mut Connection, migrations: &[Migration], last: u32) -> Result<Vec<u32>> {
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
    for m in migrations
        .iter()
        .filter(|m| m.version > current && m.version <= last)
    {
        apply_one(conn, m)?;
        tracing::info!(version = m.version, name = m.name, "applied migration");
        applied.push(m.version);
    }
    Ok(applied)
}

/// The marker line that runs a migration with foreign keys switched off.
const FOREIGN_KEYS_OFF: &str = "-- foreign_keys: off";

/// Whether a migration's SQL opens with the [`FOREIGN_KEYS_OFF`] marker line.
fn wants_foreign_keys_off(sql: &str) -> bool {
    sql.split('\n')
        .next()
        .is_some_and(|line| line.trim_end() == FOREIGN_KEYS_OFF)
}

/// Whether the connection enforces foreign keys right now.
fn foreign_keys_on(conn: &Connection) -> rusqlite::Result<bool> {
    conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0))
}

/// A kind of orphan row: the table, the parent table and the foreign key it breaks.
type OrphanKind = (String, String, i64);

/// How many orphan rows of each kind `PRAGMA foreign_key_check` reports.
///
/// A rowid cannot be part of the kind: it is NULL in a `WITHOUT ROWID` table, and a
/// rebuild that moves a table to one keeps every row under a different identity.
fn foreign_key_violations(conn: &Connection) -> rusqlite::Result<BTreeMap<OrphanKind, usize>> {
    let mut stmt = conn.prepare("PRAGMA foreign_key_check")?;
    let mut rows = stmt.query([])?;
    let mut out: BTreeMap<OrphanKind, usize> = BTreeMap::new();
    while let Some(row) = rows.next()? {
        *out.entry((row.get(0)?, row.get(2)?, row.get(3)?))
            .or_default() += 1;
    }
    Ok(out)
}

/// The table of the first kind of orphan `after` holds more of than `before`, if any.
fn added_violation<'m>(
    before: &BTreeMap<OrphanKind, usize>,
    after: &'m BTreeMap<OrphanKind, usize>,
) -> Option<&'m str> {
    after
        .iter()
        .find(|(kind, count)| **count > before.get(*kind).copied().unwrap_or(0))
        .map(|(kind, _)| kind.0.as_str())
}

/// Applies one migration, switching foreign keys off around its transaction when marked.
///
/// SQLite ignores the pragma inside a transaction, so the switch brackets the transaction
/// and restores the connection's earlier value on the success and every failure path.
fn apply_one(conn: &mut Connection, m: &Migration) -> Result<()> {
    let fail = |source| Error::Migration {
        version: m.version,
        source,
    };
    if !wants_foreign_keys_off(m.sql) {
        return apply_in(conn, m, false);
    }
    let before = foreign_keys_on(conn).map_err(fail)?;
    conn.pragma_update(None, "foreign_keys", false)
        .map_err(fail)?;
    let outcome = apply_in(conn, m, true);
    if let Err(source) = conn.pragma_update(None, "foreign_keys", before) {
        if outcome.is_ok() {
            return Err(fail(source));
        }
    }
    outcome
}

/// Applies one migration in its own transaction, checking foreign keys when `check_fk`.
///
/// The check fails only on more orphans of a kind than the database held, since an
/// orphan it already holds is not this migration's to fix.
fn apply_in(conn: &mut Connection, m: &Migration, check_fk: bool) -> Result<()> {
    let fail = |source| Error::Migration {
        version: m.version,
        source,
    };
    let tx = conn.transaction().map_err(fail)?;
    let before = if check_fk {
        foreign_key_violations(&tx).map_err(fail)?
    } else {
        BTreeMap::new()
    };
    tx.execute_batch(m.sql).map_err(fail)?;
    flush_groups(&tx).map_err(|e| match e {
        Error::Db(source) => fail(source),
        other => other,
    })?;
    if check_fk {
        let after = foreign_key_violations(&tx).map_err(fail)?;
        if let Some(table) = added_violation(&before, &after) {
            return Err(Error::ForeignKeyCheck {
                version: m.version,
                table: table.to_owned(),
            });
        }
    }
    tx.execute(
        "INSERT INTO schema_version (version, name, applied_at) VALUES (?1, ?2, ?3)",
        params![m.version, m.name, crate::unix_now()],
    )
    .map_err(fail)?;
    tx.commit().map_err(fail)?;
    Ok(())
}

/// Builds the `title_groups` rows a migration's writes left dirty, once the table exists.
fn flush_groups(conn: &Connection) -> Result<()> {
    if super::has_table(conn, "main", "title_groups_dirty")? {
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
    use crate::db::imports::ImportAction;
    use crate::db::jobs::JobState;
    use crate::jobs::{JobKind, Lane};

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
    fn schema_has_every_table_and_only_the_known_views() {
        let mut conn = Connection::open_in_memory().expect("open");
        apply(&mut conn).expect("apply");
        let tables = names(&conn, "table");
        for t in [
            "chd_failures",
            "chd_tracks",
            "chd_whole",
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
            "torrent_dirs",
            "torrent_files",
        ] {
            assert!(tables.iter().any(|n| n == t), "missing table {t}");
        }
        let views = [
            "file_view",
            "live_rom_view",
            "rom_view",
            "title_search_source",
            "title_view",
            "torrent_file_view",
        ];
        assert_eq!(names(&conn, "view"), views);
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM title_groups", [], |r| r.get(0))
            .expect("group query");
        assert_eq!(rows, 0);
    }

    #[test]
    fn arcade_scan_cleanup_prunes_scan_noise_but_keeps_import_rows() {
        let mut conn = Connection::open_in_memory().expect("open");
        apply_through(&mut conn, 11).expect("older migrations");
        conn.execute_batch(
            "INSERT INTO platforms (id, name, core_dir, kind) VALUES
               ('arcade', 'Arcade', '_Arcade', 'arcade'), ('nes', 'NES', 'NES', 'cartridge');",
        )
        .expect("platforms");
        // An MRA zip rom, as import and presence rows link `files.rom_id` to.
        conn.execute_batch(
            "INSERT INTO dat_versions (platform_id, dat_name, version, source_file, loaded_at, game_count)
               VALUES ('arcade', 'MRA', '1', 'mra', 0, 0);
             INSERT INTO titles (platform_id, dat_version_id, name, base_name, regions, languages, flags, source)
               VALUES ('arcade', 1, 'Example Blaster', 'Example Blaster', '[]', '[]', '[]', 'mra');
             INSERT INTO roms (title_id, name, size, status) VALUES (1, 'exampleset.zip', 0, 'good');",
        )
        .expect("mra title");
        let rom_id = conn.last_insert_rowid();

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

        assert_eq!(apply_through(&mut conn, 12).expect("cleanup"), [12]);

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
    fn a_source_the_user_unbound_keeps_its_choice_and_gets_the_reason() {
        let mut conn = Connection::open_in_memory().expect("open");
        apply_through(&mut conn, 18).expect("older migrations");
        conn.execute_batch(
            "INSERT INTO sources (infohash, display_name, origin_file, state, added_at, user_unbound)
               VALUES ('aa', 'Set aside', 'a.torrent', 'unbound', 0, 1),
                      ('bb', 'Automatic', 'b.torrent', 'unbound', 0, 0);",
        )
        .expect("sources");
        assert_eq!(apply(&mut conn).expect("apply").first(), Some(&19));
        let rows: Vec<(bool, Option<String>, Option<String>)> = conn
            .prepare("SELECT user_binding, reason, bind_pending FROM sources ORDER BY id")
            .expect("prepare")
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .expect("query")
            .collect::<rusqlite::Result<_>>()
            .expect("rows");
        let ignored = r#"{"code":"ignored"}"#.to_owned();
        assert_eq!(
            rows,
            [(true, Some(ignored), None), (false, None, None)],
            "the user's choice survives as user_binding with its reason"
        );
    }

    #[test]
    fn stored_source_reasons_become_codes() {
        use crate::db::sources::SourceReason;
        let mut conn = Connection::open_in_memory().expect("open");
        apply_through(&mut conn, 20).expect("older migrations");
        conn.execute_batch(
            "INSERT INTO platforms (id, name, core_dir, kind, enabled) VALUES
               ('nes', 'NES', 'NES', 'cartridge', 1);",
        )
        .expect("platforms");
        let reasons = [
            ("No download client found. The file list is read once one is detected.", None),
            ("Waiting for the download client to read the file list.", None),
            ("The download client did not accept the source: refused (403).", None),
            ("No platform matched 60% of the files. Pick a platform to bind it.", Some("nes")),
            (
                "No platform matched 75% of the files. Pick a platform to bind it. Its names suggest NES.",
                Some("nes"),
            ),
            ("Looks like NES. No DAT for it is loaded yet; it binds once one loads.", Some("nes")),
            ("Marked as not a game set. It is not bound automatically.", None),
            ("Something else.", None),
        ];
        for (i, (reason, suggested)) in reasons.iter().enumerate() {
            conn.execute(
                "INSERT INTO sources (infohash, display_name, origin_file, state, reason, added_at,
                                      suggested_platform_id)
                 VALUES (?1, 'Set', 'a.torrent', 'unbound', ?2, 0, ?3)",
                params![format!("{i:040x}"), reason, suggested],
            )
            .expect("source");
        }
        assert!(apply(&mut conn).expect("apply").contains(&21));
        let got: Vec<Option<SourceReason>> = conn
            .prepare("SELECT reason FROM sources ORDER BY id")
            .expect("prepare")
            .query_map([], |r| r.get(0))
            .expect("query")
            .collect::<rusqlite::Result<_>>()
            .expect("rows");
        let nes = || mistarr_core::PlatformId::new("nes");
        assert_eq!(
            got,
            [
                Some(SourceReason::NoClient),
                Some(SourceReason::WaitingMetadata),
                Some(SourceReason::ClientRefused {
                    error: "refused (403)".into()
                }),
                Some(SourceReason::NoMatch {
                    percent: 60,
                    suggested: None
                }),
                Some(SourceReason::NoMatch {
                    percent: 75,
                    suggested: Some(nes())
                }),
                Some(SourceReason::AwaitingDat { platform: nes() }),
                Some(SourceReason::Ignored),
                None,
            ]
        );
    }

    #[test]
    fn rows_hashed_without_the_whole_file_queue_a_scan_of_their_platform() {
        let mut conn = Connection::open_in_memory().expect("open");
        let whole = MIGRATIONS
            .iter()
            .find(|m| m.name.ends_with("_whole_hashes"))
            .expect("the whole-hashes migration");
        apply_through(&mut conn, whole.version - 1).expect("older migrations");
        conn.execute_batch(
            "INSERT INTO platforms (id, name, core_dir, kind, enabled) VALUES
               ('nes', 'NES', 'NES', 'cartridge', 1), ('atari7800', 'Atari 7800', 'Atari7800', 'cartridge', 1),
               ('lynx', 'Lynx', 'AtariLynx', 'cartridge', 0), ('snes', 'SNES', 'SNES', 'cartridge', 1),
               ('gba', 'GBA', 'GBA', 'cartridge', 1);",
        )
        .expect("platforms");
        let insert = |platform: &str, rel: &str, rule: Option<&str>, sha1: Option<&str>| {
            conn.execute(
                "INSERT INTO files (platform_id, rel_path, size, mtime, sha1, header_rule, state,
                                    scanned_at)
                 VALUES (?1, ?2, 9, 0, ?3, ?4, 'verified', 0)",
                params![platform, rel, sha1, rule],
            )
            .expect("file");
        };
        insert("nes", "NES/a.zip#a.nes", Some("ines"), Some("aa"));
        insert("atari7800", "Atari7800/b.a78", Some("a78"), Some("bb"));
        insert("lynx", "AtariLynx/c.lnx", Some("lnx"), Some("cc"));
        insert("snes", "SNES/d.sfc", Some("smc"), Some("dd"));
        insert("gba", "GBA/e.zip#e.gba", None, None);
        insert("nes", "NES/f.zip#f.nes", Some("ines"), None);
        assert_eq!(
            apply_through(&mut conn, whole.version).expect("the whole-hashes migration"),
            [whole.version]
        );
        let queued: Vec<String> = conn
            .prepare("SELECT platform_id FROM scan_progress ORDER BY platform_id")
            .expect("prepare")
            .query_map([], |r| r.get(0))
            .expect("query")
            .collect::<rusqlite::Result<_>>()
            .expect("rows");
        assert_eq!(
            queued,
            ["atari7800", "nes"],
            "enabled platforms with stale rows only"
        );
        let whole_sha1: Option<String> = conn
            .query_row(
                "SELECT sha1_whole FROM files WHERE platform_id = 'nes'",
                [],
                |r| r.get(0),
            )
            .expect("column");
        assert_eq!(whole_sha1, None);
    }

    #[test]
    fn arcade_presence_migration_indexes_and_drops_md5_less_member_rows() {
        let mut conn = Connection::open_in_memory().expect("open");
        apply_through(&mut conn, 12).expect("older migrations");
        conn.execute_batch(
            "INSERT INTO platforms (id, name, core_dir, kind) VALUES
               ('arcade', 'Arcade', '_Arcade', 'arcade'), ('nes', 'NES', 'NES', 'cartridge');",
        )
        .expect("platforms");
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
            "INSERT INTO import_log (at, file_id, action, detail) VALUES (1, ?1, 0, '{}')",
            [stale],
        )
        .expect("log");

        assert_eq!(apply_through(&mut conn, 13).expect("migrate"), [13]);

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
        assert!(sql("roms_match_name").ends_with("(match_name, title_id)"));
        assert!(sql("roms_match_base").contains("size, title_id) WHERE"));
        assert!(sql("torrent_files_source_rom").ends_with("(source_id, rom_id)"));
        assert!(sql("roms_id_title").ends_with("(id, title_id)"));
        assert!(sql("titles_id_dat").ends_with("(id, dat_version_id)"));
        assert!(sql("title_groups_counts")
            .ends_with("(platform_id, lean_flags, source, have_verified, wanted)"));
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
        // The wave's numbers may leave a gap below the last file, so what the database
        // then records is the highest migration it still holds.
        let previous = MIGRATIONS[MIGRATIONS.len() - 2].version;
        assert_eq!(pending(&path).expect("pending"), Some((previous, latest())));
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

    #[test]
    fn chd_migration_drops_unmatched_disc_chd_rows_only() {
        let mut conn = Connection::open_in_memory().expect("open");
        let chd = MIGRATIONS
            .iter()
            .find(|m| m.name.ends_with("_chd_tracks"))
            .expect("the CHD migration");
        apply_through(&mut conn, chd.version - 1).expect("older migrations");
        conn.execute_batch(
            "INSERT INTO platforms (id, name, core_dir, kind, enabled) VALUES
               ('psx', 'PSX', 'PSX', 'disc', 1), ('nes', 'NES', 'NES', 'cartridge', 1);",
        )
        .expect("platforms");
        conn.execute_batch(
            "INSERT INTO dat_versions (platform_id, dat_name, version, source_file, loaded_at, game_count)
               VALUES ('psx', 'Discs', '1', 'discs.dat', 0, 1);
             INSERT INTO titles (platform_id, dat_version_id, name, base_name)
               VALUES ('psx', 1, 'Disc', 'Disc');
             INSERT INTO roms (title_id, name, size, status) VALUES (1, 'Disc.chd', 9, 'good');",
        )
        .expect("disc");
        let rom = conn.last_insert_rowid();
        let insert = |platform: &str, rel: &str, rom: Option<i64>| {
            conn.execute(
                "INSERT INTO files (platform_id, rel_path, size, mtime, rom_id, state, scanned_at)
                 VALUES (?1, ?2, 9, 0, ?3, 'unverified', 0)",
                params![platform, rel, rom],
            )
            .expect("file");
            conn.last_insert_rowid()
        };
        let logged = insert("psx", "PSX/G/g.CHD", None);
        insert("psx", "PSX/Disc/Disc.chd", Some(rom));
        insert("psx", "PSX/G/g.bin", None);
        insert("nes", "NES/odd.chd", None);
        conn.execute(
            "INSERT INTO import_log (at, file_id, action, detail) VALUES (0, ?1, 0, '{}')",
            [logged],
        )
        .expect("log");
        assert_eq!(
            apply_through(&mut conn, chd.version).expect("the CHD migration"),
            [chd.version]
        );
        let left: Vec<String> = conn
            .prepare("SELECT rel_path FROM files ORDER BY rel_path")
            .expect("prepare")
            .query_map([], |r| r.get(0))
            .expect("query")
            .collect::<rusqlite::Result<_>>()
            .expect("rows");
        assert_eq!(left, ["NES/odd.chd", "PSX/Disc/Disc.chd", "PSX/G/g.bin"]);
        let file_id: Option<i64> = conn
            .query_row("SELECT file_id FROM import_log", [], |r| r.get(0))
            .expect("log");
        assert_eq!(file_id, None);
    }

    /// Seeds jobs of every kind, state and lane plus three rows outside the value
    /// tables, a log row per action and two settings, and returns the jobs that stay.
    fn seed_small_tables(conn: &Connection) -> Vec<(JobKind, JobState, Lane)> {
        let (states, lanes) = (JobState::ALL, Lane::ALL);
        let mut want = Vec::new();
        for (i, kind) in JobKind::ALL.iter().enumerate() {
            let (state, lane) = (states[i % states.len()], lanes[i % lanes.len()]);
            let at = i64::try_from(i).expect("index");
            conn.execute(
                "INSERT INTO jobs (kind, payload, subject, state, created_at, updated_at, lane)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6)",
                params![
                    kind.as_str(),
                    format!("{{\"i\":{i}}}"),
                    format!("i\u{1f}{i}"),
                    state.as_str(),
                    at,
                    lane.as_str()
                ],
            )
            .expect("job");
            want.push((*kind, state, lane));
        }
        for (kind, state, lane) in [
            ("nope", "queued", "heavy"),
            ("scan", "nope", "heavy"),
            ("scan", "queued", "nope"),
        ] {
            conn.execute(
                "INSERT INTO jobs (kind, payload, subject, state, created_at, updated_at, lane)
                 VALUES (?1, '{}', '', ?2, 0, 0, ?3)",
                params![kind, state, lane],
            )
            .expect("bad job");
        }
        for (i, action) in ImportAction::ALL.iter().enumerate() {
            conn.execute(
                "INSERT INTO import_log (at, download_id, file_id, action, detail)
                 VALUES (?1, NULL, NULL, ?2, ?3)",
                params![
                    i64::try_from(i).expect("index"),
                    action.as_str(),
                    format!("{{\"a\":{i}}}")
                ],
            )
            .expect("log");
        }
        conn.execute_batch(
            "INSERT INTO settings (key, value) VALUES ('a', '1'), ('b', '{\"x\": 2}');",
        )
        .expect("settings");
        want
    }

    #[test]
    fn small_tables_move_to_codes_and_strict_with_every_row_kept() {
        let mut conn = Connection::open_in_memory().expect("open");
        // As the board opens: keys on, so the rebuild runs the way it will there.
        conn.pragma_update(None, "foreign_keys", true)
            .expect("keys");
        // The file this test checks is 0028; the numbers below it are the wave's, and
        // may be a gap while the packages that own them are still in flight.
        apply_through(&mut conn, 27).expect("older migrations");
        let want = seed_small_tables(&conn);

        assert!(apply(&mut conn).expect("apply").contains(&28));

        let jobs: Vec<(JobKind, JobState, Lane, Option<String>, i64)> = conn
            .prepare("SELECT kind, state, lane, progress, created_at FROM jobs ORDER BY id")
            .expect("prepare")
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .expect("query")
            .collect::<rusqlite::Result<_>>()
            .expect("rows");
        let got: Vec<(JobKind, JobState, Lane)> = jobs
            .iter()
            .map(|(k, s, l, p, _)| {
                assert_eq!(*p, None, "progress is kept");
                (*k, *s, *l)
            })
            .collect();
        assert_eq!(got, want, "every job keeps its kind, state and lane");
        assert_eq!(
            jobs.iter().map(|(_, _, _, _, at)| *at).collect::<Vec<_>>(),
            (0..i64::try_from(JobKind::ALL.len()).expect("count")).collect::<Vec<_>>(),
            "the bad rows are gone and no other row moved"
        );

        let log: Vec<(ImportAction, String)> = conn
            .prepare("SELECT action, detail FROM import_log ORDER BY id")
            .expect("prepare")
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .expect("query")
            .collect::<rusqlite::Result<_>>()
            .expect("rows");
        let mut want_log = Vec::new();
        for (i, action) in ImportAction::ALL.iter().enumerate() {
            want_log.push((*action, format!("{{\"a\":{i}}}")));
        }
        assert_eq!(log, want_log, "every log row keeps its action and detail");

        assert_eq!(
            column(
                &conn,
                "SELECT key || '=' || value FROM settings ORDER BY key"
            ),
            ["a=1", "b={\"x\": 2}"],
            "settings rows are kept"
        );

        let integrity: String = conn
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .expect("integrity");
        assert_eq!(integrity, "ok");
        let orphans: i64 = conn
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })
            .expect("foreign keys");
        assert_eq!(orphans, 0);
        let sql = |name: &str| -> String {
            conn.query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [name],
                |r| r.get(0),
            )
            .expect(name)
        };
        for table in ["jobs", "import_log"] {
            assert!(sql(table).contains("STRICT"), "{table} is not STRICT");
        }
        assert!(
            sql("settings").contains("STRICT, WITHOUT ROWID"),
            "{}",
            sql("settings")
        );
        assert_eq!(
            column(
                &conn,
                "SELECT \"table\" FROM pragma_foreign_key_list('import_log') ORDER BY 1"
            ),
            ["downloads", "files"],
            "import_log keeps its foreign keys"
        );
        let indexes = names(&conn, "index");
        for index in ["jobs_subject", "jobs_state", "import_log_file"] {
            assert!(indexes.iter().any(|n| n == index), "missing index {index}");
        }
    }

    /// Platforms, one DAT version with two roms and two sources, for the torrent tables.
    fn seed_torrents(conn: &Connection) {
        conn.execute_batch(
            "INSERT INTO platforms (id, name, core_dir, kind, enabled)
               VALUES ('nes', 'NES', 'NES', 'cartridge', 1);
             INSERT INTO dat_versions (id, platform_id, dat_name, version, source_file, loaded_at,
                                       game_count)
               VALUES (1, 'nes', 'Test', '1', 't.dat', 0, 2);
             INSERT INTO titles (id, platform_id, dat_version_id, name, base_name)
               VALUES (1, 'nes', 1, 'Example', 'Example');
             INSERT INTO roms (id, title_id, name, size)
               VALUES (1, 1, 'a.nes', 8), (2, 1, 'b.nes', 16);
             INSERT INTO sources (id, infohash, display_name, origin_file, state, added_at)
               VALUES (1, 'aa', 'Nested Set', 'a.torrent', 'bound', 0),
                      (2, 'bb', 'Other Set', 'b.torrent', 'bound', 0);",
        )
        .expect("catalog");
    }

    /// The files and candidates of the 0024 schema: every path shape and confidence word.
    fn seed_torrent_rows(conn: &Connection) {
        conn.execute_batch(
            "INSERT INTO torrent_files (source_id, file_index, path, size, rom_id, confidence)
               VALUES (1, 0, 'Nested/Sub/Example Quest (USA).nes', 8, 1, 'hash'),
                      (1, 1, 'Top.nes', 16, NULL, NULL),
                      (1, 2, 'Sét/Ünïcode/но́ва.nes', 8, 2, 'name'),
                      (1, 3, 'Nested/Second (Japan).nes', 16, NULL, 'base'),
                      (2, 0, 'Other/readme.txt', 4, NULL, NULL);
             INSERT INTO torrent_candidates (source_id, file_index, rom_id, confidence)
               VALUES (1, 0, 2, 'fuzzy'), (1, 1, 1, 'size'), (1, 1, 2, 'name'),
                      (1, 3, 1, 'base'), (2, 0, 1, 'size');",
        )
        .expect("rows");
    }

    #[test]
    fn torrent_paths_become_directories_and_codes_with_every_row_kept() {
        use crate::db::candidates::MatchConfidence;
        let mut conn = Connection::open_in_memory().expect("open");
        conn.pragma_update(None, "foreign_keys", true)
            .expect("keys");
        apply_through(&mut conn, 24).expect("older migrations");
        seed_torrents(&conn);
        seed_torrent_rows(&conn);
        let files_before = file_rows(&conn, "f.path", "f.confidence");
        let candidates_before = candidate_rows(&conn, "confidence");

        assert_eq!(
            apply_through(&mut conn, 25).expect("the torrent-paths migration"),
            [25]
        );

        // The view decodes the code to the word the API shows.
        let mut stmt = conn
            .prepare(
                "SELECT f.confidence, v.confidence_text
                 FROM torrent_files f JOIN torrent_file_view v
                   ON v.source_id = f.source_id AND v.file_index = f.file_index",
            )
            .expect("prepare");
        let decoded = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, Option<MatchConfidence>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                ))
            })
            .expect("query")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("rows");
        assert_eq!(decoded.len(), files_before.len());
        for (code, text) in decoded {
            assert_eq!(
                code.map(|c| c.as_str().to_owned()),
                text,
                "the stored code and the view agree"
            );
        }
        assert_eq!(
            file_rows(
                &conn,
                "v.path",
                "CASE f.confidence WHEN 0 THEN 'hash' WHEN 1 THEN 'name'
                                    WHEN 2 THEN 'base' END"
            ),
            files_before,
            "every file row is kept"
        );
        assert_eq!(
            candidate_rows(
                &conn,
                "CASE confidence WHEN 1 THEN 'name' WHEN 2 THEN 'base'
                                 WHEN 3 THEN 'fuzzy' WHEN 4 THEN 'size' END"
            ),
            candidates_before,
            "every candidate row is kept"
        );
        assert_eq!(
            column(
                &conn,
                "SELECT path FROM torrent_dirs ORDER BY source_id, path"
            ),
            ["", "Nested/", "Nested/Sub/", "Sét/Ünïcode/", "Other/"],
            "one row per distinct directory, the top level as ''"
        );
        let codes: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM torrent_candidates WHERE confidence IN (1, 2, 3, 4)",
                [],
                |r| r.get(0),
            )
            .expect("codes");
        assert_eq!(codes, 5, "every candidate code is inside its CHECK");
        let sql = |name: &str| -> String {
            conn.query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [name],
                |r| r.get(0),
            )
            .expect(name)
        };
        assert!(
            sql("torrent_dirs").contains("STRICT"),
            "{}",
            sql("torrent_dirs")
        );
        assert!(
            sql("torrent_files").contains("STRICT, WITHOUT ROWID"),
            "{}",
            sql("torrent_files")
        );
        assert!(
            sql("torrent_candidates").contains("STRICT, WITHOUT ROWID"),
            "{}",
            sql("torrent_candidates")
        );
        assert_eq!(column(&conn, "PRAGMA integrity_check"), ["ok"]);
        assert!(column(&conn, "PRAGMA foreign_key_check").is_empty());
    }

    #[test]
    fn a_torrent_file_label_outside_the_value_table_maps_or_fails() {
        use crate::db::candidates::MatchConfidence;
        let mut conn = Connection::open_in_memory().expect("open");
        apply_through(&mut conn, 24).expect("older migrations");
        seed_torrents(&conn);
        conn.execute(
            "INSERT INTO torrent_files (source_id, file_index, path, size, rom_id, confidence)
             VALUES (1, 0, 'Nested/Top.nes', 8, 1, 'size')",
            [],
        )
        .expect("file");
        assert_eq!(apply_through(&mut conn, 25).expect("migrate"), [25]);
        let code: i64 = conn
            .query_row("SELECT confidence FROM torrent_files", [], |r| r.get(0))
            .expect("code");
        assert_eq!(
            code,
            MatchConfidence::Base.code(),
            "the size-only tier is the base tier"
        );

        // A word the value table does not hold fails the CHECK and rolls back whole.
        let mut conn = Connection::open_in_memory().expect("open");
        apply_through(&mut conn, 24).expect("older migrations");
        seed_torrents(&conn);
        conn.execute(
            "INSERT INTO torrent_files (source_id, file_index, path, size, rom_id, confidence)
             VALUES (1, 0, 'Nested/Top.nes', 8, 1, 'bogus')",
            [],
        )
        .expect("file");
        let before = current_version(&conn).expect("version");
        match apply_through(&mut conn, 25) {
            Err(Error::Migration { version, .. }) => assert_eq!(version, 25),
            other => panic!("expected a migration error, got {other:?}"),
        }
        assert_eq!(
            column(&conn, "SELECT path FROM torrent_files"),
            ["Nested/Top.nes"],
            "the path column and its row are intact"
        );
        assert_eq!(
            current_version(&conn).expect("version"),
            before,
            "not recorded"
        );
    }

    /// One file row: its keys, whole path, size, rom and confidence word.
    type TorrentFileRow = (i64, i64, String, i64, Option<i64>, Option<String>);

    /// Every file row, reading the whole path from `path` and the word from `word`.
    fn file_rows(conn: &Connection, path: &str, word: &str) -> Vec<TorrentFileRow> {
        conn.prepare(&format!(
            "SELECT f.source_id, f.file_index, {path}, f.size, f.rom_id, {word}
             FROM torrent_files f JOIN torrent_file_view v
               ON v.source_id = f.source_id AND v.file_index = f.file_index
             ORDER BY f.source_id, f.file_index"
        ))
        .expect("prepare")
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })
        .expect("query")
        .collect::<rusqlite::Result<_>>()
        .expect("rows")
    }

    /// Every candidate row as `(source_id, file_index, rom_id, confidence word)`.
    fn candidate_rows(conn: &Connection, word: &str) -> Vec<(i64, i64, i64, String)> {
        conn.prepare(&format!(
            "SELECT source_id, file_index, rom_id, {word} FROM torrent_candidates
             ORDER BY source_id, file_index, rom_id"
        ))
        .expect("prepare")
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .expect("query")
        .collect::<rusqlite::Result<_>>()
        .expect("rows")
    }

    const PARENT_AND_CHILD: &str = "\
CREATE TABLE parent (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
CREATE TABLE child (id INTEGER PRIMARY KEY, parent_id INTEGER NOT NULL REFERENCES parent(id),
                    note TEXT NOT NULL);
CREATE INDEX parent_name ON parent (name);";

    const REBUILD_PARENT: &str = "-- foreign_keys: off
DROP INDEX parent_name;
CREATE TABLE parent_new (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
INSERT INTO parent_new (id, name) SELECT id, name FROM parent ORDER BY id;
DROP TABLE parent;
ALTER TABLE parent_new RENAME TO parent;
CREATE INDEX parent_name ON parent (name);";

    const ORPHANING_REBUILD: &str = "-- foreign_keys: off
DROP INDEX parent_name;
CREATE TABLE parent_new (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
INSERT INTO parent_new (id, name) SELECT id, name FROM parent WHERE id = 1 ORDER BY id;
DROP TABLE parent;
ALTER TABLE parent_new RENAME TO parent;
CREATE INDEX parent_name ON parent (name);";

    const PARENT_CHILD_AND_TRIGGER: &str = "\
CREATE TABLE parent (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
CREATE TABLE child (id INTEGER PRIMARY KEY, parent_id INTEGER NOT NULL REFERENCES parent(id),
                    note TEXT NOT NULL);
CREATE TABLE child_log (child_id INTEGER NOT NULL, parent_name TEXT NOT NULL);
CREATE TRIGGER child_logs_parent AFTER INSERT ON child
BEGIN
  INSERT INTO child_log (child_id, parent_name)
    SELECT NEW.id, p.name FROM parent p WHERE p.id = NEW.parent_id;
END;";

    const REBUILD_AROUND_TRIGGER: &str = "-- foreign_keys: off
DROP TRIGGER child_logs_parent;
CREATE TABLE parent_new (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
INSERT INTO parent_new (id, name) SELECT id, name FROM parent ORDER BY id;
DROP TABLE parent;
ALTER TABLE parent_new RENAME TO parent;
CREATE TRIGGER child_logs_parent AFTER INSERT ON child
BEGIN
  INSERT INTO child_log (child_id, parent_name)
    SELECT NEW.id, p.name FROM parent p WHERE p.id = NEW.parent_id;
END;";

    fn two_migrations(create: &'static str, rebuild: &'static str) -> [Migration; 2] {
        [
            Migration {
                version: 1,
                name: "parent_and_child",
                sql: create,
            },
            Migration {
                version: 2,
                name: "rebuild_parent",
                sql: rebuild,
            },
        ]
    }

    fn foreign_keys(conn: &Connection) -> bool {
        conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .expect("pragma")
    }

    fn column(conn: &Connection, sql: &str) -> Vec<String> {
        let mut stmt = conn.prepare(sql).expect("prepare");
        stmt.query_map([], |r| r.get(0))
            .expect("query")
            .collect::<rusqlite::Result<_>>()
            .expect("rows")
    }

    /// Applies `create`, seeds two parents and their children, then applies `rebuild`.
    fn seed_and_rebuild(
        create: &'static str,
        rebuild: &'static str,
    ) -> (Connection, Result<Vec<u32>>) {
        let mut conn = Connection::open_in_memory().expect("open");
        conn.pragma_update(None, "foreign_keys", true)
            .expect("keys on");
        let migs = two_migrations(create, rebuild);
        assert_eq!(apply_list(&mut conn, &migs, 1).expect("schema"), [1]);
        conn.execute_batch(
            "INSERT INTO parent (id, name) VALUES (1, 'One'), (2, 'Two');
             INSERT INTO child (id, parent_id, note) VALUES (1, 1, 'a'), (2, 2, 'b');",
        )
        .expect("rows");
        let outcome = apply_list(&mut conn, &migs, u32::MAX);
        (conn, outcome)
    }

    #[test]
    fn a_marked_migration_rebuilds_a_parent_table_under_its_children() {
        let (conn, outcome) = seed_and_rebuild(PARENT_AND_CHILD, REBUILD_PARENT);
        assert_eq!(outcome.expect("rebuild"), [2]);
        assert_eq!(current_version(&conn).expect("version"), 2);
        assert_eq!(
            column(&conn, "SELECT name FROM parent ORDER BY id"),
            ["One", "Two"]
        );
        assert_eq!(
            column(
                &conn,
                "SELECT p.name FROM child c JOIN parent p ON p.id = c.parent_id ORDER BY c.id"
            ),
            ["One", "Two"],
            "the child rows still reach their parent"
        );
        assert!(foreign_keys(&conn), "the pragma is back on after commit");
    }

    const PARENT_CHILD_WITH_NOTE: &str = "\
CREATE TABLE parent (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
CREATE TABLE child (id INTEGER PRIMARY KEY, parent_id INTEGER NOT NULL REFERENCES parent(id),
                    note TEXT NOT NULL);
CREATE TABLE note (id INTEGER PRIMARY KEY, text TEXT NOT NULL);
CREATE INDEX note_text ON note (text);";

    const REBUILD_NOTE: &str = "-- foreign_keys: off
DROP INDEX note_text;
CREATE TABLE note_new (id INTEGER PRIMARY KEY, text TEXT NOT NULL);
INSERT INTO note_new (id, text) SELECT id, text FROM note ORDER BY id;
DROP TABLE note;
ALTER TABLE note_new RENAME TO note;
CREATE INDEX note_text ON note (text);";

    #[test]
    fn an_orphan_the_database_already_holds_does_not_stop_a_marked_migration() {
        let mut conn = Connection::open_in_memory().expect("open");
        conn.pragma_update(None, "foreign_keys", true)
            .expect("keys on");
        let migs = two_migrations(PARENT_CHILD_WITH_NOTE, REBUILD_NOTE);
        assert_eq!(apply_list(&mut conn, &migs, 1).expect("schema"), [1]);
        conn.pragma_update(None, "foreign_keys", false)
            .expect("keys off");
        conn.execute_batch(
            "INSERT INTO parent (id, name) VALUES (1, 'One');
             INSERT INTO child (id, parent_id, note) VALUES (1, 1, 'a');
             DELETE FROM parent WHERE id = 1;
             INSERT INTO note (id, text) VALUES (1, 'n');",
        )
        .expect("rows");
        conn.pragma_update(None, "foreign_keys", true)
            .expect("keys on");

        assert_eq!(
            apply_list(&mut conn, &migs, u32::MAX).expect("rebuild"),
            [2],
            "an unrelated table's orphan is not this migration's to fix"
        );
        assert_eq!(
            column(&conn, "SELECT note FROM child"),
            ["a"],
            "the orphan stays as it was"
        );
        assert_eq!(column(&conn, "SELECT text FROM note"), ["n"]);
        assert!(foreign_keys(&conn), "the pragma is back on after commit");
    }

    const PARENT_AND_ROWID_CHILD: &str = "\
CREATE TABLE parent (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
CREATE TABLE child (id INTEGER PRIMARY KEY, parent_id INTEGER NOT NULL REFERENCES parent(id),
                    note TEXT NOT NULL) STRICT;
CREATE INDEX child_parent ON child (parent_id);";

    const REBUILD_CHILD_WITHOUT_ROWID: &str = "-- foreign_keys: off
DROP INDEX child_parent;
CREATE TABLE child_new (id INTEGER NOT NULL, parent_id INTEGER NOT NULL REFERENCES parent(id),
                        note TEXT NOT NULL, PRIMARY KEY (id)) STRICT, WITHOUT ROWID;
INSERT INTO child_new (id, parent_id, note) SELECT id, parent_id, note FROM child ORDER BY id;
DROP TABLE child;
ALTER TABLE child_new RENAME TO child;
CREATE INDEX child_parent ON child (parent_id);";

    #[test]
    fn an_orphan_a_rebuilt_rowid_table_held_is_not_one_the_migration_added() {
        let mut conn = Connection::open_in_memory().expect("open");
        conn.pragma_update(None, "foreign_keys", true)
            .expect("keys on");
        let migs = two_migrations(PARENT_AND_ROWID_CHILD, REBUILD_CHILD_WITHOUT_ROWID);
        assert_eq!(apply_list(&mut conn, &migs, 1).expect("schema"), [1]);
        conn.pragma_update(None, "foreign_keys", false)
            .expect("keys off");
        conn.execute_batch(
            "INSERT INTO parent (id, name) VALUES (1, 'One'), (2, 'Two');
             INSERT INTO child (id, parent_id, note) VALUES (1, 1, 'a'), (2, 2, 'b');
             DELETE FROM parent WHERE id = 2;",
        )
        .expect("rows");
        conn.pragma_update(None, "foreign_keys", true)
            .expect("keys on");

        assert_eq!(
            apply_list(&mut conn, &migs, u32::MAX).expect("rebuild"),
            [2],
            "the row it held before the rebuild is no new orphan"
        );
        assert_eq!(
            column(&conn, "SELECT note FROM child ORDER BY id"),
            ["a", "b"]
        );
        let orphans: i64 = conn
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })
            .expect("orphans");
        assert_eq!(orphans, 1, "the orphan is still there, as it was before");
        assert!(foreign_keys(&conn));
    }

    #[test]
    fn a_migration_that_adds_a_second_orphan_of_the_same_kind_rolls_back() {
        let mut conn = Connection::open_in_memory().expect("open");
        conn.pragma_update(None, "foreign_keys", true)
            .expect("keys on");
        // The second migration orphans another child of the same parent and key.
        let migs = two_migrations(
            PARENT_AND_CHILD,
            "-- foreign_keys: off
DELETE FROM parent WHERE id = 3;",
        );
        assert_eq!(apply_list(&mut conn, &migs, 1).expect("schema"), [1]);
        conn.pragma_update(None, "foreign_keys", false)
            .expect("keys off");
        conn.execute_batch(
            "INSERT INTO parent (id, name) VALUES (1, 'One'), (2, 'Two'), (3, 'Three');
             INSERT INTO child (id, parent_id, note) VALUES (1, 1, 'a'), (2, 2, 'b'), (3, 3, 'c');
             DELETE FROM parent WHERE id = 2;",
        )
        .expect("rows");
        conn.pragma_update(None, "foreign_keys", true)
            .expect("keys on");

        match apply_list(&mut conn, &migs, u32::MAX) {
            Err(Error::ForeignKeyCheck { version, table }) => {
                assert_eq!((version, table.as_str()), (2, "child"));
            }
            other => panic!("expected a foreign key failure, got {other:?}"),
        }
        assert_eq!(
            column(&conn, "SELECT name FROM parent ORDER BY id"),
            ["One", "Three"],
            "the parent the migration would have orphaned is back"
        );
        assert_eq!(current_version(&conn).expect("version"), 1, "not recorded");
        assert!(foreign_keys(&conn), "the pragma is back on after rollback");
    }

    #[test]
    fn a_marked_migration_that_orphans_a_child_row_rolls_back() {
        let (conn, outcome) = seed_and_rebuild(PARENT_AND_CHILD, ORPHANING_REBUILD);
        match outcome {
            Err(Error::ForeignKeyCheck { version, table }) => {
                assert_eq!(version, 2);
                assert_eq!(table, "child", "the failure names the offending table");
            }
            other => panic!("expected a foreign key failure, got {other:?}"),
        }
        assert_eq!(current_version(&conn).expect("version"), 1, "not recorded");
        assert_eq!(
            column(&conn, "SELECT name FROM parent ORDER BY id"),
            ["One", "Two"],
            "the old table and its rows are intact"
        );
        assert_eq!(
            column(&conn, "SELECT note FROM child ORDER BY id"),
            ["a", "b"]
        );
        assert!(foreign_keys(&conn), "the pragma is back on after rollback");
    }

    #[test]
    fn an_unmarked_migration_still_enforces_foreign_keys() {
        let (conn, outcome) = seed_and_rebuild(
            PARENT_AND_CHILD,
            "INSERT INTO child (id, parent_id, note) VALUES (3, 9, 'c');",
        );
        match outcome {
            Err(Error::Migration { version, .. }) => assert_eq!(version, 2),
            other => panic!("expected a migration error, got {other:?}"),
        }
        assert_eq!(
            column(&conn, "SELECT note FROM child ORDER BY id"),
            ["a", "b"]
        );
        assert!(foreign_keys(&conn), "the pragma is untouched");
    }

    #[test]
    fn a_rebuild_drops_and_recreates_a_trigger_naming_the_table() {
        let (conn, outcome) = seed_and_rebuild(PARENT_CHILD_AND_TRIGGER, REBUILD_AROUND_TRIGGER);
        assert_eq!(outcome.expect("rebuild"), [2]);
        assert!(names(&conn, "trigger")
            .iter()
            .any(|n| n == "child_logs_parent"));
        conn.execute(
            "INSERT INTO child (id, parent_id, note) VALUES (3, 1, 'c')",
            [],
        )
        .expect("child");
        assert_eq!(
            column(&conn, "SELECT parent_name FROM child_log ORDER BY child_id"),
            ["One", "Two", "One"],
            "the trigger fires again after the rebuild"
        );
    }

    #[test]
    fn only_an_exact_first_line_asks_for_foreign_keys_off() {
        assert!(wants_foreign_keys_off(
            "-- foreign_keys: off\nCREATE TABLE t (i);"
        ));
        assert!(wants_foreign_keys_off(
            "-- foreign_keys: off\r\nCREATE TABLE t (i);"
        ));
        assert!(wants_foreign_keys_off("-- foreign_keys: off"));
        assert!(wants_foreign_keys_off(
            "-- foreign_keys: off \nCREATE TABLE t (i);"
        ));
        assert!(!wants_foreign_keys_off(
            "-- foreign_keys: off to rebuild\nCREATE TABLE t (i);"
        ));
        assert!(!wants_foreign_keys_off(
            "CREATE TABLE t (i);\n-- foreign_keys: off\n"
        ));
        assert!(!wants_foreign_keys_off(
            "--foreign_keys: off\nCREATE TABLE t (i);"
        ));
        assert!(!wants_foreign_keys_off(
            "-- foreign_keys: OFF\nCREATE TABLE t (i);"
        ));
    }
}
