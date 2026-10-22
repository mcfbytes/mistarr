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
    if m.version >= FLUSH_FROM {
        flush_groups(&tx).map_err(|e| match e {
            Error::Db(source) => fail(source),
            other => other,
        })?;
    }
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

/// The first migration after which `groups::flush` matches the schema: it reads file states as codes.
const FLUSH_FROM: u32 = 24;

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
    use std::fmt::Write as _;

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
            "INSERT INTO platforms (slug, name, core_dir, kind) VALUES ('p', 'P', 'P', 0);
             INSERT INTO dat_versions (platform_id, dat_name, version, source_file, loaded_at, game_count)
               VALUES ('p', 'd', '1', 'd.dat', 0, 1);
             INSERT INTO titles (platform_id, dat_version_id, name, base_name) VALUES ('p', 1, 't', 't');
             INSERT INTO roms (title_id, name, size, md5, sha1) VALUES (1, 'a', 4, unhex(printf('%032x', 1)), unhex(printf('%040x', 1)));
             INSERT INTO roms (title_id, name, size, md5) VALUES (1, 'b', 4, unhex(printf('%032x', 2)));",
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
            "INSERT INTO platforms (slug, name, core_dir, kind, enabled)
               VALUES ('nes', 'NES', 'NES', 0, 1);
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

    /// The reason words of `files.reason` in code order, as the 0023 schema stored them.
    const REASON_WORDS: [&str; 17] = [
        "off",
        "pending",
        "no_layout",
        "unreadable",
        "not_chd",
        "version",
        "parent",
        "not_cd",
        "too_large",
        "codec",
        "gdrom",
        "old_layout",
        "cooked",
        "pregap",
        "corrupt",
        "checksum",
        "no_checksum",
    ];

    /// `n` as `digits` hex digits.
    fn digits(n: u64, digits: usize) -> String {
        format!("{n:0digits$x}")
    }

    /// Platforms, DATs, titles and roms of every shape the 0023 schema allows, plus
    /// files, CHD cache rows and failures, with hashes as the text it stored.
    fn seed_hashes(conn: &Connection) {
        seed_catalogue(conn);
        seed_roms(conn);
        seed_files(conn);
        seed_chd(conn);
    }

    /// Platforms, DAT versions and titles for the rows below.
    fn seed_catalogue(conn: &Connection) {
        conn.execute_batch(
            "INSERT INTO platforms (id, name, core_dir, kind, enabled) VALUES
               ('nes', 'NES', 'NES', 'cartridge', 1), ('psx', 'PSX', 'PSX', 'disc', 1),
               ('arcade', 'Arcade', 'arcade', 'arcade', 1);
             INSERT INTO dat_versions (id, platform_id, dat_name, version, source_file, loaded_at,
                                       game_count, source)
               VALUES (1, 'nes', 'Example NES', '1', 'n.dat', 0, 2, 'dat'),
                      (2, 'psx', 'Example Discs', '1', 'd.dat', 0, 1, 'dat'),
                      (3, 'arcade', '_Arcade', '1', '_Arcade', 0, 1, 'mra');
             INSERT INTO titles (id, platform_id, dat_version_id, name, base_name, parent_id, source)
               VALUES (1, 'nes', 1, 'Example Quest (USA)', 'Example Quest', 1, 'dat'),
                      (2, 'nes', 1, 'Nova (World)', 'Nova', 2, 'dat'),
                      (3, 'psx', 2, 'Disc Sample (USA)', 'Disc Sample', 3, 'dat'),
                      (4, 'arcade', 3, 'Sample Cabinet', 'Sample Cabinet', 4, 'mra');",
        )
        .expect("catalogue");
    }

    /// A rom of each shape: no hashes, partial hashes, mixed case, MRA zips.
    fn seed_roms(conn: &Connection) {
        let (crc, md5, sha1) = (
            digits(0x0a0b_0c0d, 8),
            digits(0xfeed, 32),
            digits(0xbeef, 40),
        );
        let (crc_up, md5_up) = (crc.to_uppercase(), digits(0xabcd, 32).to_uppercase());
        let (c7, s9) = (digits(7, 8), digits(9, 40));
        let (c1, m2, s3) = (digits(1, 8), digits(2, 32), digits(3, 40));
        conn.execute_batch(&format!(
            "INSERT INTO roms (id, title_id, name, size, crc32, md5, sha1, status, retired,
                               match_name, match_base, zip_dir, present) VALUES
             (1, 1, 'Example Quest (USA).nes', 16, '{crc_up}', '{md5}', '{sha1}', 'good', 0,
              'example quest (usa).nes', 'example quest', NULL, 0),
             (2, 1, 'unkeyed.nes', 0, NULL, NULL, NULL, 'nodump', 0, NULL, NULL, NULL, 0),
             (3, 2, 'Nova (World).nes', 32, '{c7}', NULL, NULL, 'baddump', 1,
              'nova (world).nes', 'nova', NULL, 0),
             (4, 2, 'nova alt.nes', 48, NULL, NULL, '{s9}', 'verified', 0,
              'nova alt.nes', NULL, NULL, 0),
             (5, 3, 'Disc Sample (USA).cue', 100, NULL, NULL, NULL, 'good', 0, NULL, NULL, NULL, 0),
             (6, 3, 'Disc Sample (USA) (Track 1).bin', 2352, '{c1}', '{m2}', '{s3}', 'verified', 0,
              NULL, NULL, NULL, 0),
             (7, 4, 'samplecab.zip', 0, NULL, '{md5_up}', NULL, 'good', 0, NULL, NULL, 'mame', 1),
             (8, 4, 'samplecab2.zip', 0, NULL, NULL, NULL, 'nodump', 0, NULL, NULL, 'mame', 0);"
        ))
        .expect("roms");
    }

    /// A file in every state under every header rule, with and without hashes.
    fn seed_files(conn: &Connection) {
        let rules = [
            Some("none"),
            Some("ines"),
            Some("smc"),
            Some("a78"),
            Some("lnx"),
            Some("n64"),
            Some("chd"),
            None,
        ];
        let states = [
            "pending",
            "verified",
            "misnamed",
            "unverified",
            "bad",
            "unidentified",
        ];
        for (i, rule) in rules.iter().enumerate() {
            let state = states[i % 5];
            conn.execute(
                "INSERT INTO files (platform_id, rel_path, size, mtime, crc32, md5, sha1, header_rule,
                                    rom_id, state, scanned_at, crc32_whole, md5_whole, sha1_whole)
                 VALUES ('nes', ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?3, ?10, ?11, ?12)",
                params![
                    format!("NES/f{i}.nes"),
                    i64::try_from(100 + i).expect("size"),
                    i64::try_from(i).expect("time"),
                    (i % 3 != 2).then(|| digits(i as u64 + 1, 8).to_uppercase()),
                    (i % 3 == 0).then(|| digits(i as u64 + 1, 32)),
                    (i % 3 == 0).then(|| digits(i as u64 + 1, 40)),
                    rule,
                    (i % 2 == 0).then_some(1),
                    state,
                    (i % 4 == 1).then(|| digits(i as u64 + 50, 8)),
                    (i % 4 == 1).then(|| digits(i as u64 + 50, 32)),
                    (i % 4 == 1).then(|| digits(i as u64 + 50, 40)),
                ],
            )
            .expect("file");
        }
        for state in states {
            conn.execute(
                "INSERT INTO files (platform_id, rel_path, size, mtime, state, scanned_at)
                 VALUES ('psx', ?1, 1, 1, ?2, 1)",
                params![format!("PSX/{state}.bin"), state],
            )
            .expect("file");
        }
    }

    /// The CHD images of the 0023 schema: reasons, track hashes, whole hashes and failures.
    fn seed_chd(conn: &Connection) {
        for (i, reason) in REASON_WORDS.iter().enumerate() {
            conn.execute(
                "INSERT INTO files (platform_id, rel_path, size, mtime, header_rule, state, scanned_at, reason)
                 VALUES ('psx', ?1, 1, 1, 'chd', 'unidentified', 1, ?2)",
                params![format!("PSX/r{i:02}.chd"), reason],
            )
            .expect("file");
            conn.execute(
                "INSERT INTO chd_failures (chd_sha1, chd_size, mtime, reason, decoder, failed_at)
                 VALUES (?1, ?2, 5, ?3, 1, ?4)",
                params![
                    digits(0x1000 + i as u64, 40),
                    i64::try_from(i).expect("size"),
                    reason,
                    i64::try_from(i * 7).expect("time")
                ],
            )
            .expect("failure");
        }
        for i in 1..=3_u64 {
            conn.execute(
                "INSERT INTO chd_tracks (chd_sha1, chd_size, track, size, crc32, md5, sha1)
                 VALUES (?1, 10, ?2, ?3, ?4, ?5, ?6)",
                params![
                    digits(0x2000, 40),
                    i64::try_from(i).expect("track"),
                    2352 * i64::try_from(i).expect("n"),
                    digits(i, 8),
                    digits(i + 10, 32),
                    digits(i + 20, 40)
                ],
            )
            .expect("track");
        }
        conn.execute(
            "INSERT INTO chd_tracks (chd_sha1, chd_size, track, size, crc32, md5, sha1)
             VALUES (?1, 11, 1, 2352, ?2, ?3, ?4)",
            params![
                digits(0x2001, 40),
                digits(5, 8),
                digits(6, 32),
                digits(7, 40)
            ],
        )
        .expect("track");
        for (i, mtime) in [3_i64, 4].into_iter().enumerate() {
            conn.execute(
                "INSERT INTO chd_whole (chd_sha1, chd_size, mtime, crc32, md5, sha1)
                 VALUES (?1, 10, ?2, ?3, ?4, ?5)",
                params![
                    digits(0x3000, 40),
                    mtime,
                    digits(i as u64 + 1, 8),
                    digits(i as u64 + 2, 32),
                    digits(i as u64 + 3, 40)
                ],
            )
            .expect("whole");
        }
    }

    /// Every row of `sql`, each column as text; a BLOB as lowercase hex and NULL as `NULL`.
    fn dump(conn: &Connection, sql: &str) -> Vec<Vec<String>> {
        use rusqlite::types::ValueRef;
        let mut stmt = conn.prepare(sql).expect("prepare");
        let n = stmt.column_count();
        stmt.query_map([], |r| {
            (0..n)
                .map(|i| {
                    Ok(match r.get_ref(i)? {
                        ValueRef::Null => "NULL".to_owned(),
                        ValueRef::Integer(v) => v.to_string(),
                        ValueRef::Real(v) => v.to_string(),
                        ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
                        ValueRef::Blob(b) => b.iter().fold(String::new(), |mut hex, x| {
                            let _ = write!(hex, "{x:02x}");
                            hex
                        }),
                    })
                })
                .collect()
        })
        .expect("query")
        .collect::<rusqlite::Result<_>>()
        .expect("rows")
    }

    /// `column` of every row of `dump` rewritten from its code to its word by `word`.
    fn decode(rows: &mut [Vec<String>], column: usize, word: impl Fn(i64) -> Option<&'static str>) {
        for row in rows {
            if row[column] != "NULL" {
                let code = row[column].parse().expect("code");
                row[column] = word(code).expect("a code in the table").to_owned();
            }
        }
    }

    /// The rows of the tables the migration rebuilds, read as text before it runs.
    struct HashRows {
        roms: Vec<Vec<String>>,
        files: Vec<Vec<String>>,
        tracks: Vec<Vec<String>>,
        whole: Vec<Vec<String>>,
        failures: Vec<Vec<String>>,
    }

    /// The 0023 rows of the rebuilt tables with hashes as lowercase text and words as words.
    fn hash_rows_before(conn: &Connection) -> HashRows {
        HashRows {
            roms: dump(
                conn,
                "SELECT id, title_id, name, size, lower(crc32), lower(md5), lower(sha1), status,
                        retired, match_name, match_base, header, zip_dir, present
                 FROM roms ORDER BY id",
            ),
            files: dump(
                conn,
                "SELECT id, platform_id, rel_path, size, mtime, lower(crc32), lower(md5),
                        lower(sha1), header_rule, rom_id, state, scanned_at, reason,
                        lower(crc32_whole), lower(md5_whole), lower(sha1_whole)
                 FROM files ORDER BY id",
            ),
            tracks: dump(
                conn,
                "SELECT lower(chd_sha1), chd_size, track, size, lower(crc32), lower(md5), lower(sha1)
                 FROM chd_tracks ORDER BY lower(chd_sha1), chd_size, track",
            ),
            whole: dump(
                conn,
                "SELECT lower(chd_sha1), chd_size, mtime, lower(crc32), lower(md5), lower(sha1)
                 FROM chd_whole ORDER BY lower(chd_sha1), chd_size, mtime",
            ),
            failures: dump(
                conn,
                "SELECT lower(chd_sha1), chd_size, mtime, reason, decoder, failed_at
                 FROM chd_failures ORDER BY lower(chd_sha1), chd_size, mtime",
            ),
        }
    }

    /// Holds every rom equal to its 0023 row through `hex()` and the status word.
    fn check_roms(conn: &Connection, before: &[Vec<String>]) {
        use crate::db::titles::RomStatus;
        let mut roms = dump(
            conn,
            "SELECT r.id, r.title_id, r.name, r.size, v.crc32_hex, v.md5_hex, v.sha1_hex, r.status,
                    r.retired, r.match_name, r.match_base, r.header, r.zip_dir, r.present
             FROM roms r JOIN rom_view v ON v.id = r.id ORDER BY r.id",
        );
        let status = |c| RomStatus::ALL.iter().find(|s| s.code() == c);
        decode(&mut roms, 7, |c| status(c).map(|s| s.as_str()));
        assert_eq!(roms, before, "every rom keeps its hashes, status and flags");
        for row in dump(conn, "SELECT status, status_text FROM rom_view ORDER BY id") {
            let word = status(row[0].parse().expect("code")).map(|s| s.as_str());
            assert_eq!(word, Some(row[1].as_str()), "the view decodes the code");
        }
    }

    /// Holds every file equal to its 0023 row, with rule, state and reason as words.
    fn check_files(conn: &Connection, before: &[Vec<String>]) {
        use crate::db::chd::Unidentified;
        use crate::db::files::{FileState, StoredRule};
        let rule = |c| {
            StoredRule::ALL
                .iter()
                .find(|r| r.code() == c)
                .map(|r| r.as_str())
        };
        let state = |c| {
            FileState::ALL
                .iter()
                .find(|s| s.code() == c)
                .map(|s| s.as_str())
        };
        let reason = |c| Unidentified::from_stored(c).map(Unidentified::as_str);
        let mut files = dump(
            conn,
            "SELECT f.id, v.platform, f.rel_path, f.size, f.mtime, v.crc32_hex, v.md5_hex,
                    v.sha1_hex, f.header_rule, f.rom_id, f.state, f.scanned_at, f.reason,
                    NULLIF(lower(hex(f.crc32_whole)), ''), NULLIF(lower(hex(f.md5_whole)), ''),
                    NULLIF(lower(hex(f.sha1_whole)), '')
             FROM files f JOIN file_view v ON v.id = f.id ORDER BY f.id",
        );
        decode(&mut files, 8, rule);
        decode(&mut files, 10, state);
        decode(&mut files, 12, reason);
        assert_eq!(
            files, before,
            "every file keeps its hashes, rule, state and reason"
        );
        let view = dump(
            conn,
            "SELECT header_rule, header_rule_text, state, state_text, reason, reason_text
             FROM file_view ORDER BY id",
        );
        let text = |code: &str, words: &dyn Fn(i64) -> Option<&'static str>| {
            code.parse()
                .ok()
                .and_then(words)
                .unwrap_or("NULL")
                .to_owned()
        };
        for row in view {
            assert_eq!(text(&row[0], &rule), row[1], "header rule text");
            assert_eq!(text(&row[2], &state), row[3], "state text");
            assert_eq!(text(&row[4], &reason), row[5], "reason text");
        }
        for (i, word) in REASON_WORDS.iter().enumerate() {
            let code = Unidentified::parse(word).expect("a reason").code();
            assert_eq!(code, i64::try_from(i).expect("code"), "{word}");
        }
    }

    /// Holds the CHD failures, tracks and whole hashes equal to their 0023 rows.
    fn check_chd(conn: &Connection, before: &HashRows) {
        use crate::db::chd::Unidentified;
        let mut failures = dump(
            conn,
            "SELECT lower(hex(chd_sha1)), chd_size, mtime, reason, decoder, failed_at
             FROM chd_failures ORDER BY chd_sha1, chd_size, mtime",
        );
        decode(&mut failures, 3, |c| {
            Unidentified::from_stored(c).map(Unidentified::as_str)
        });
        assert_eq!(failures, before.failures, "every failure keeps its reason");
        let tracks = dump(
            conn,
            "SELECT lower(hex(chd_sha1)), chd_size, track, size, lower(hex(crc32)),
                    lower(hex(md5)), lower(hex(sha1))
             FROM chd_tracks ORDER BY chd_sha1, chd_size, track",
        );
        assert_eq!(tracks, before.tracks);
        let whole = dump(
            conn,
            "SELECT lower(hex(chd_sha1)), chd_size, mtime, lower(hex(crc32)),
                    lower(hex(md5)), lower(hex(sha1))
             FROM chd_whole ORDER BY chd_sha1, chd_size, mtime",
        );
        assert_eq!(whole, before.whole);
    }

    /// Holds the hash columns to BLOBs of their width, the tables STRICT, and the
    /// indexes, triggers and views back.
    fn check_forms(conn: &Connection) {
        let widths = [("crc32", 4), ("md5", 16), ("sha1", 20)];
        let whole = [("crc32_whole", 4), ("md5_whole", 16), ("sha1_whole", 20)];
        for (table, columns) in [
            ("roms", widths),
            ("files", widths),
            ("files", whole),
            ("chd_tracks", widths),
            ("chd_whole", widths),
        ] {
            for (column, width) in columns {
                let wrong: i64 = conn
                    .query_row(
                        &format!(
                            "SELECT COUNT(*) FROM {table} WHERE NOT (typeof({column}) = 'null'
                               OR (typeof({column}) = 'blob' AND length({column}) = {width}))"
                        ),
                        [],
                        |r| r.get(0),
                    )
                    .expect("types");
                assert_eq!(wrong, 0, "{table}.{column}");
            }
        }
        let nulls: i64 = conn
            .query_row("SELECT COUNT(*) FROM roms WHERE sha1 IS NULL", [], |r| {
                r.get(0)
            })
            .expect("nulls");
        assert_eq!(nulls, 5, "a rom with no hash keeps NULL, not an empty blob");
        for table in ["roms", "files", "chd_tracks", "chd_whole", "chd_failures"] {
            let sql: String = conn
                .query_row(
                    "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [table],
                    |r| r.get(0),
                )
                .expect(table);
            assert!(sql.contains("STRICT"), "{table} is not STRICT");
            assert_eq!(
                sql.contains("WITHOUT ROWID"),
                table.starts_with("chd_"),
                "{table}"
            );
        }
        let indexes = names(conn, "index");
        for index in [
            "roms_sha1",
            "roms_md5",
            "roms_crc",
            "roms_match_name",
            "roms_match_base",
            "roms_id_title",
            "roms_size",
            "roms_chd_size",
            "roms_track_size",
            "files_rom",
            "files_state",
            "files_rel_lower",
            "chd_tracks_sha1",
        ] {
            assert!(indexes.iter().any(|n| n == index), "missing index {index}");
        }
        let triggers = names(conn, "trigger");
        for trigger in [
            "roms_insert_groups",
            "roms_update_groups",
            "roms_delete_groups",
            "files_insert_groups",
            "files_update_groups",
            "files_delete_groups",
        ] {
            assert!(
                triggers.iter().any(|n| n == trigger),
                "missing trigger {trigger}"
            );
        }
        assert_eq!(
            names(conn, "view"),
            [
                "file_view",
                "live_rom_view",
                "rom_view",
                "title_search_source",
                "title_view",
                "torrent_file_view"
            ]
        );
    }

    #[test]
    fn hashes_become_blobs_and_codes_with_every_row_kept() {
        let mut conn = Connection::open_in_memory().expect("open");
        // As the board opens: keys on, so the rebuild runs the way it will there.
        conn.pragma_update(None, "foreign_keys", true)
            .expect("keys");
        apply_through(&mut conn, 23).expect("older migrations");
        seed_hashes(&conn);
        let before = hash_rows_before(&conn);
        assert_eq!((before.roms.len(), before.files.len()), (8, 8 + 6 + 17));

        assert_eq!(
            apply_through(&mut conn, 24).expect("the hash migration"),
            [24]
        );

        check_roms(&conn, &before.roms);
        check_files(&conn, &before.files);
        check_chd(&conn, &before);
        check_forms(&conn);
        assert_eq!(column(&conn, "PRAGMA integrity_check"), ["ok"]);
        assert!(column(&conn, "PRAGMA foreign_key_check").is_empty());
        assert!(foreign_keys(&conn), "the pragma is back on");
    }

    /// Every platform of the binary's table and two slugs outside it, with rows on several
    /// of them in the tables that name a platform, in the 0023 shape.
    fn seed_platform_rows(conn: &Connection) {
        use mistarr_mister::platforms::PLATFORMS;
        for p in &PLATFORMS {
            conn.execute(
                "INSERT INTO platforms (id, name, core_dir, kind, core_present, enabled)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    p.id,
                    p.name,
                    p.core_dir,
                    p.kind.as_str(),
                    p.id == "snes",
                    p.id != "gg"
                ],
            )
            .expect("platform");
        }
        conn.execute_batch(
            "INSERT INTO platforms (id, name, core_dir, kind)
               VALUES ('zeta', 'Zeta', 'Zeta', 'romset'), ('alpha', 'Alpha', 'Alpha', 'disc');
             INSERT INTO dat_versions (id, platform_id, dat_name, version, source_file, loaded_at,
                                       game_count, source)
               VALUES (1, 'nes', 'Example NES', '1', 'n.dat', 0, 1, 'dat'),
                      (2, 'alpha', 'Example Alpha', '1', 'a.dat', 0, 1, 'dat'),
                      (3, NULL, 'Unbound', '1', 'u.dat', 0, 0, 'dat');
             INSERT INTO titles (id, platform_id, dat_version_id, name, base_name, parent_id, source)
               VALUES (1, 'nes', 1, 'Example Quest (USA)', 'Example Quest', 1, 'dat'),
                      (2, 'alpha', 2, 'Nova (World)', 'Nova', 2, 'dat');
             INSERT INTO sources (id, infohash, display_name, origin_file, state, added_at,
                                  platform_id, suggested_platform_id)
               VALUES (1, 'aa', 'Set A', 'a.torrent', 'bound', 0, 'nes', NULL),
                      (2, 'bb', 'Set B', 'b.torrent', 'unbound', 0, NULL, 'zeta');
             INSERT INTO files (id, platform_id, rel_path, size, mtime, state, scanned_at)
               VALUES (1, 'nes', 'NES/a.nes', 4, 1, 'verified', 1),
                      (2, 'nes', 'NES/b.nes', 5, 2, 'pending', 2),
                      (3, 'alpha', 'Alpha/g.bin', 6, 3, 'unverified', 3),
                      (4, 'psx', 'PSX/d.bin', 7, 4, 'misnamed', 4),
                      (5, 'arcade', 'mame/x.zip', 8, 5, 'verified', 5);
             INSERT INTO scan_progress (platform_id, done_dirs, updated_at)
               VALUES ('nes', '[\"NES\"]', 9), ('zeta', '[]', 10), ('psx', '[\"PSX/D\"]', 11);",
        )
        .expect("rows");
    }

    /// The rows a platform key must not change, as the slug names them.
    const PLATFORM_FILES: &str = "SELECT f.id, f.platform_id, f.rel_path, f.size, f.mtime, f.state,
                f.scanned_at FROM files f ORDER BY f.id";
    const PLATFORM_CHILDREN: [&str; 3] = [
        "SELECT id, platform_id, dat_name FROM dat_versions ORDER BY id",
        "SELECT id, platform_id, dat_version_id, name FROM titles ORDER BY id",
        "SELECT id, platform_id, suggested_platform_id FROM sources ORDER BY id",
    ];

    #[test]
    fn platforms_take_fixed_keys_and_files_and_scans_follow_them() {
        use crate::db::platforms::{fixed_key, KindCode};
        use mistarr_core::PlatformKey;
        use mistarr_mister::platforms::PLATFORMS;
        let mut conn = Connection::open_in_memory().expect("open");
        conn.pragma_update(None, "foreign_keys", true)
            .expect("keys");
        apply_through(&mut conn, 23).expect("older migrations");
        seed_platform_rows(&conn);
        let files_before = dump(&conn, PLATFORM_FILES);
        let scans_before = dump(
            &conn,
            "SELECT platform_id, done_dirs, updated_at FROM scan_progress ORDER BY platform_id",
        );
        let children_before: Vec<_> = PLATFORM_CHILDREN.iter().map(|q| dump(&conn, q)).collect();
        let mut platforms_before = dump(
            &conn,
            "SELECT id, name, core_dir, kind, core_present, enabled FROM platforms ORDER BY id",
        );
        assert_eq!(platforms_before.len(), PLATFORMS.len() + 2);

        assert_eq!(apply_through(&mut conn, 24).expect("the migration"), [24]);

        let mut after = dump(
            &conn,
            "SELECT slug, name, core_dir, kind, core_present, enabled FROM platforms ORDER BY slug",
        );
        decode(&mut after, 3, |c| {
            KindCode::ALL
                .iter()
                .find(|k| k.code() == c)
                .map(|k| k.as_str())
        });
        platforms_before.sort();
        after.sort();
        assert_eq!(
            after, platforms_before,
            "every platform row keeps its columns"
        );
        for (at, p) in PLATFORMS.iter().enumerate() {
            let key: i64 = conn
                .query_row("SELECT id FROM platforms WHERE slug = ?1", [p.id], |r| {
                    r.get(0)
                })
                .expect("key");
            assert_eq!(Some(key), fixed_key(p.id).map(PlatformKey::get), "{}", p.id);
            assert_eq!(key, i64::try_from(at).expect("slot") + 1, "{}", p.id);
        }
        assert_eq!(
            column(
                &conn,
                "SELECT slug || '=' || id FROM platforms WHERE id >= 1000 ORDER BY id"
            ),
            ["alpha=1000", "zeta=1001"],
            "a slug outside the table takes 1000 up, in slug order"
        );

        let files_after = dump(
            &conn,
            "SELECT f.id, v.platform, f.rel_path, f.size, f.mtime, v.state_text, f.scanned_at
             FROM files f JOIN file_view v ON v.id = f.id ORDER BY f.id",
        );
        assert_eq!(
            files_after, files_before,
            "every file keeps its row through the slug"
        );
        let keyed = dump(
            &conn,
            "SELECT f.id, f.platform = p.id FROM files f JOIN platforms p ON p.slug = (
               SELECT v.platform FROM file_view v WHERE v.id = f.id) ORDER BY f.id",
        );
        assert!(keyed.iter().all(|r| r[1] == "1"), "{keyed:?}");
        assert_eq!(
            dump(
                &conn,
                "SELECT p.slug, s.done_dirs, s.updated_at
                 FROM scan_progress s JOIN platforms p ON p.id = s.platform ORDER BY p.slug"
            ),
            scans_before
                .iter()
                .map(|r| vec![r[0].clone(), r[1].clone(), r[2].clone()])
                .collect::<Vec<_>>(),
            "every scan row keeps its directories"
        );
        for (query, before) in PLATFORM_CHILDREN.iter().zip(&children_before) {
            assert_eq!(&dump(&conn, query), before, "{query}");
        }
        assert_eq!(column(&conn, "PRAGMA integrity_check"), ["ok"]);
        assert!(column(&conn, "PRAGMA foreign_key_check").is_empty());
        assert!(foreign_keys(&conn), "the pragma is back on");
        for table in ["platforms", "files", "scan_progress"] {
            let sql: String = conn
                .query_row(
                    "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [table],
                    |r| r.get(0),
                )
                .expect(table);
            assert!(sql.contains("STRICT"), "{table}");
        }
    }

    #[test]
    fn a_platform_key_or_slug_the_table_lacks_is_refused() {
        let mut conn = Connection::open_in_memory().expect("open");
        conn.pragma_update(None, "foreign_keys", true)
            .expect("keys");
        apply_through(&mut conn, 23).expect("older migrations");
        seed_platform_rows(&conn);
        apply_through(&mut conn, 24).expect("the migration");
        assert!(conn
            .execute(
                "INSERT INTO files (platform, rel_path, size, mtime, state, scanned_at)
                 VALUES (999, 'x', 1, 1, 0, 1)",
                [],
            )
            .is_err());
        assert!(conn
            .execute(
                "INSERT INTO scan_progress (platform, updated_at) VALUES (999, 1)",
                []
            )
            .is_err());
        assert!(conn
            .execute(
                "INSERT INTO dat_versions (platform_id, dat_name, version, source_file, loaded_at,
                                           game_count, source)
                 VALUES ('nowhere', 'd', '1', 'd.dat', 0, 0, 'dat')",
                [],
            )
            .is_err());
        conn.execute(
            "INSERT INTO dat_versions (platform_id, dat_name, version, source_file, loaded_at,
                                       game_count, source)
             VALUES ('zeta', 'd', '1', 'd.dat', 0, 0, 'dat')",
            [],
        )
        .expect("a slug of the table is accepted");
        assert!(conn
            .execute(
                "UPDATE sources SET platform_id = 'nowhere' WHERE id = 1",
                []
            )
            .is_err());
    }

    #[test]
    fn a_file_naming_a_platform_the_table_lacks_gets_a_row_to_key_by() {
        let mut conn = Connection::open_in_memory().expect("open");
        conn.pragma_update(None, "foreign_keys", false)
            .expect("keys");
        apply_through(&mut conn, 23).expect("older migrations");
        conn.execute_batch(
            "INSERT INTO files (platform_id, rel_path, size, mtime, state, scanned_at)
               VALUES ('gb', 'a.gb', 4, 0, 'pending', 0), ('nowhere', 'b.gb', 4, 0, 'pending', 0);
             INSERT INTO scan_progress (platform_id, updated_at) VALUES ('gb', 1);",
        )
        .expect("orphans");
        apply_through(&mut conn, 24).expect("the migration");
        assert_eq!(
            column(
                &conn,
                "SELECT f.rel_path || '=' || p.slug FROM files f
                 JOIN platforms p ON p.id = f.platform ORDER BY f.rel_path"
            ),
            ["a.gb=gb", "b.gb=nowhere"]
        );
        let ids = column(&conn, "SELECT slug || '=' || id FROM platforms ORDER BY id");
        assert_eq!(ids, ["gb=5", "nowhere=1000"]);
        assert!(column(&conn, "PRAGMA foreign_key_check").is_empty());
    }

    #[test]
    fn the_group_triggers_follow_verified_files_after_the_rebuild() {
        let mut conn = Connection::open_in_memory().expect("open");
        apply(&mut conn).expect("apply");
        conn.execute_batch(
            "INSERT INTO platforms (slug, name, core_dir, kind, enabled)
               VALUES ('nes', 'NES', 'NES', 0, 1);
             INSERT INTO dat_versions (id, platform_id, dat_name, version, source_file, loaded_at,
                                       game_count)
               VALUES (1, 'nes', 'Example NES', '1', 'n.dat', 0, 1);
             INSERT INTO titles (id, platform_id, dat_version_id, name, base_name, parent_id)
               VALUES (1, 'nes', 1, 'Example Quest (USA)', 'Example Quest', 1);
             INSERT INTO roms (id, title_id, name, size) VALUES (1, 1, 'a.nes', 4);
             DELETE FROM title_groups_dirty;",
        )
        .expect("rows");
        let dirty = |conn: &Connection| -> i64 {
            conn.query_row("SELECT COUNT(*) FROM title_groups_dirty", [], |r| r.get(0))
                .expect("dirty")
        };
        conn.execute("INSERT INTO files (platform, rel_path, size, mtime, rom_id, state, scanned_at) VALUES (1, 'a.nes', 4, 0, 1, 3, 0)", [])
            .expect("unverified file");
        assert_eq!(dirty(&conn), 0, "an unverified file changes no group");
        conn.execute("UPDATE files SET state = 1", [])
            .expect("verify");
        assert_eq!(dirty(&conn), 1, "a verified file marks its group");
        conn.execute("DELETE FROM title_groups_dirty", [])
            .expect("clear");
        conn.execute("DELETE FROM files", []).expect("delete");
        assert_eq!(dirty(&conn), 1, "removing it marks the group again");
    }

    /// Applies 0023, seeds one rom, runs `extra` against the 0023 schema and applies 0024.
    fn hash_migration_with(extra: &str) -> (Connection, Result<Vec<u32>>) {
        let mut conn = Connection::open_in_memory().expect("open");
        conn.pragma_update(None, "foreign_keys", true)
            .expect("keys");
        apply_through(&mut conn, 23).expect("older migrations");
        conn.execute_batch(
            "INSERT INTO platforms (id, name, core_dir, kind, enabled)
               VALUES ('psx', 'PSX', 'PSX', 'disc', 1);
             INSERT INTO dat_versions (id, platform_id, dat_name, version, source_file, loaded_at,
                                       game_count)
               VALUES (1, 'psx', 'Example Discs', '1', 'd.dat', 0, 1);
             INSERT INTO titles (id, platform_id, dat_version_id, name, base_name)
               VALUES (1, 'psx', 1, 'Disc Sample (USA)', 'Disc Sample');
             INSERT INTO roms (id, title_id, name, size, crc32, md5, sha1, status)
               VALUES (1, 1, 'Disc.cue', 4, 'aabbccdd', '00000000000000000000000000000001',
                       '0000000000000000000000000000000000000001', 'good');
             INSERT INTO files (id, platform_id, rel_path, size, mtime, state, scanned_at)
               VALUES (1, 'psx', 'PSX/a.bin', 4, 0, 'pending', 0);",
        )
        .expect("rows");
        conn.execute_batch(extra).expect("bad row");
        let outcome = apply_through(&mut conn, 24);
        (conn, outcome)
    }

    #[test]
    fn text_a_blob_or_a_code_cannot_hold_fails_the_migration_and_rolls_it_back() {
        let bad = [
            "UPDATE roms SET sha1 = 'zz00000000000000000000000000000000000001'",
            "UPDATE roms SET sha1 = '00000001'",
            "UPDATE roms SET md5 = ''",
            "UPDATE roms SET crc32 = 'aabbccddee'",
            "UPDATE roms SET status = 'great'",
            "UPDATE roms SET retired = 2",
            "UPDATE roms SET present = -1",
            "UPDATE roms SET size = -1",
            "UPDATE files SET md5 = 'abcd'",
            "UPDATE files SET sha1_whole = 'not hex at all, but forty characters long'",
            "UPDATE files SET state = 'lost'",
            "UPDATE files SET header_rule = 'weird'",
            "UPDATE files SET state = 'unidentified', reason = 'odd'",
            "UPDATE files SET state = 'verified', reason = 'off'",
            "INSERT INTO chd_tracks VALUES ('abcd', 1, 1, 1, 'aabbccdd',
               '00000000000000000000000000000001', '0000000000000000000000000000000000000001')",
            "INSERT INTO chd_tracks VALUES ('0000000000000000000000000000000000000001', 1, 0, 1,
               'aabbccdd', '00000000000000000000000000000001',
               '0000000000000000000000000000000000000001')",
            "INSERT INTO chd_whole VALUES ('0000000000000000000000000000000000000001', 1, 1,
               'aabbccdd', 'xyz', '0000000000000000000000000000000000000001')",
            "INSERT INTO chd_failures VALUES ('0000000000000000000000000000000000000001', 1, 1,
               'strange', 1, 1)",
        ];
        for extra in bad {
            let (conn, outcome) = hash_migration_with(extra);
            match outcome {
                Err(Error::Migration { version, .. }) => assert_eq!(version, 24, "{extra}"),
                other => panic!("expected a migration error for {extra}, got {other:?}"),
            }
            assert_eq!(current_version(&conn).expect("version"), 23, "{extra}");
            assert_eq!(
                column(
                    &conn,
                    "SELECT name FROM sqlite_master WHERE name = 'roms_new'"
                ),
                Vec::<String>::new(),
                "{extra}: the half-built table is gone"
            );
            let strict: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE name = 'roms' AND sql LIKE '%STRICT%'",
                    [],
                    |r| r.get(0),
                )
                .expect("schema");
            assert_eq!(strict, 0, "{extra}: roms is still the 0023 table");
            assert_eq!(
                column(&conn, "SELECT CAST(COUNT(*) AS TEXT) FROM files"),
                ["1"],
                "{extra}"
            );
            assert!(foreign_keys(&conn), "{extra}: keys are on again");
        }
        let (conn, outcome) = hash_migration_with(
            "UPDATE roms SET crc32 = 'AABBCCDD';
             UPDATE files SET state = 'unidentified', reason = 'off', header_rule = 'chd';",
        );
        assert_eq!(
            outcome.expect("migrate"),
            [24],
            "upper case hex and valid codes pass"
        );
        assert_eq!(
            column(&conn, "SELECT lower(hex(crc32)) FROM roms"),
            ["aabbccdd"]
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
