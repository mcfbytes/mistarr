//! Tests of the import in RAM: the chunked copy, the check, the swap and every way out.

use super::*;
use crate::db::fixtures::{key, pid, platform_ref};
use crate::db::settings;
use crate::db::testutil;

/// A sink that records each write's size and when it was synced.
#[derive(Default)]
struct Counting {
    writes: Vec<usize>,
    synced_after: Option<usize>,
}

impl Write for Counting {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.writes.push(b.len());
        Ok(b.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Durable for Counting {
    fn sync(&mut self) -> io::Result<()> {
        self.synced_after = Some(self.writes.len());
        Ok(())
    }
}

/// A reader that hands out at most 1000 bytes per call, as a pipe might.
struct Trickle<'a>(&'a [u8]);

impl Read for Trickle<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = buf.len().min(self.0.len()).min(1000);
        buf[..n].copy_from_slice(&self.0[..n]);
        self.0 = &self.0[n..];
        Ok(n)
    }
}

#[test]
fn the_copy_back_writes_whole_mebibytes_then_syncs_once() {
    let data = vec![7u8; CHUNK_BYTES * 2 + CHUNK_BYTES / 2];
    let mut sink = Counting::default();
    let mut betweens = 0;
    let n = copy_durable(&mut Trickle(&data), &mut sink, &mut || {
        betweens += 1;
        Ok(())
    })
    .expect("copy");
    assert_eq!(n, data.len() as u64);
    assert_eq!(sink.writes, [CHUNK_BYTES, CHUNK_BYTES, CHUNK_BYTES / 2]);
    assert_eq!(
        sink.synced_after,
        Some(3),
        "synced once, after the last write"
    );
    assert_eq!(betweens, 2);

    let mut exact = Counting::default();
    copy_durable(&mut &data[..CHUNK_BYTES * 2], &mut exact, &mut || Ok(())).expect("copy");
    assert_eq!(exact.writes, [CHUNK_BYTES, CHUNK_BYTES]);
    assert_eq!(exact.synced_after, Some(2));
}

#[test]
fn a_stop_between_chunks_ends_the_copy_unsynced() {
    let data = vec![1u8; CHUNK_BYTES * 3];
    let mut sink = Counting::default();
    let r = copy_durable(&mut &data[..], &mut sink, &mut || Err(Error::Cancelled));
    assert!(matches!(r, Err(Error::Cancelled)));
    assert_eq!(sink.writes, [CHUNK_BYTES]);
    assert_eq!(sink.synced_after, None);
}

#[test]
fn write_new_makes_one_write_syscall_per_mebibyte() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (src, new) = (dir.path().join("src"), dir.path().join("src.new"));
    let data: Vec<u8> = (0..CHUNK_BYTES * 3 + 100)
        .map(|i| u8::try_from(i % 251).unwrap_or(0))
        .collect();
    fs::write(&src, &data).expect("write");
    let (n, writes) = counted(|| write_new(&src, &new, &mut || Ok(())).expect("copy"));
    assert_eq!(n, data.len() as u64);
    assert_eq!(fs::read(&new).expect("read"), data);
    if let Some(w) = writes {
        assert_eq!(w, 4, "one write per MiB and one for the rest");
    }
}

#[test]
fn the_check_names_what_is_short() {
    let mib = |n: u64| n * MIB;
    let ok = Budget {
        available: Some(mib(378)),
        ram_room: Some(mib(240)),
        card_room: Some(mib(4000)),
    };
    let size = mib(37);
    assert_eq!(need(size, 0), mib(37) + mib(37) / 2 + MARGIN_BYTES);
    assert_eq!(refusal(&ok, size, 0, mib(128)), None);
    let cases = [
        (
            Budget {
                available: None,
                ..ok
            },
            "cannot be read",
        ),
        (
            Budget {
                available: Some(mib(200)),
                ..ok
            },
            "200 MiB of memory available, 216 MiB needed",
        ),
        (
            Budget {
                ram_room: Some(mib(50)),
                ..ok
            },
            "50 MiB free in the memory directory",
        ),
        (
            Budget {
                ram_room: None,
                ..ok
            },
            "cannot be read",
        ),
        (
            Budget {
                card_room: Some(mib(20)),
                ..ok
            },
            "20 MiB free on the card",
        ),
    ];
    for (budget, says) in cases {
        let reason = refusal(&budget, size, 0, mib(128)).expect("refused");
        assert!(reason.contains(says), "{reason}");
    }
    let unknown_card = Budget {
        card_room: None,
        ..ok
    };
    assert_eq!(refusal(&unknown_card, size, 0, mib(128)), None);
}

#[test]
fn full_storage_is_recognised_from_io_and_sqlite() {
    assert!(storage_full(&io::Error::from_raw_os_error(28).into()));
    assert!(storage_full(&io::Error::from_raw_os_error(12).into()));
    let sqlite_full =
        rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_FULL), None);
    assert!(storage_full(&sqlite_full.into()));
    assert!(!storage_full(&io::Error::from_raw_os_error(2).into()));
    assert!(!storage_full(&Error::Cancelled));
}

#[test]
fn startup_removes_a_stale_new_file_and_this_databases_copies_only() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("data/mistarr.db");
    fs::create_dir_all(db.parent().expect("parent")).expect("mkdir");
    fs::write(&db, b"the database").expect("write");
    fs::write(sibling(&db, NEW_SUFFIX), b"half a database").expect("write");
    let ram = dir.path().join("ram");
    let ours = ram.join(format!("{}12", work_prefix(&db)));
    let theirs = ram.join(format!("{}12", work_prefix(&dir.path().join("other.db"))));
    for d in [&ours, &theirs] {
        fs::create_dir_all(d).expect("mkdir");
        fs::write(d.join(COPY_NAME), b"copy").expect("write");
    }
    assert_eq!(clean_stale(&db, &ram).expect("clean"), 2);
    assert!(!sibling(&db, NEW_SUFFIX).exists());
    assert!(!ours.exists());
    assert!(
        theirs.join(COPY_NAME).is_file(),
        "another server's copy stays"
    );
    assert_eq!(
        clean_stale(&db, &dir.path().join("missing")).expect("clean"),
        0
    );
}

/// A watch that records phases and can stop the run at one of them.
#[derive(Default)]
struct Script {
    phases: Vec<Phase>,
    stop_at: Option<Phase>,
}

impl Watch for Script {
    fn phase(&mut self, phase: Phase) {
        self.phases.push(phase);
    }

    fn between(&mut self, phase: Phase) -> Result<()> {
        if self.stop_at == Some(phase) {
            return Err(Error::Cancelled);
        }
        Ok(())
    }
}

/// A database holding `k = old` and `bulk` bytes besides, its WAL emptied.
fn card(bulk: usize) -> (tempfile::TempDir, Db) {
    let (dir, db) = testutil::db();
    db.write_blocking(|c| {
        settings::set(c, "k", "old")?;
        settings::set(c, "bulk", &"x".repeat(bulk))?;
        super::super::wal_emptied(c).map(|_| ())
    })
    .expect("seed");
    (dir, db)
}

fn plan(ram: &Path) -> Plan {
    Plan {
        dir: ram.to_path_buf(),
        floor: 0,
        job: Some(crate::db::ids::JobId::new(7)),
        input: 0,
    }
}

fn sha1(path: &Path) -> mistarr_core::Sha1 {
    let file = File::open(path).expect("open");
    mistarr_core::hash::hash_reader(file, mistarr_core::hash::HeaderRule::None, None)
        .expect("hash")
        .sha1
}

fn get(db: &Db, key: &str) -> Option<String> {
    db.read_blocking(|c| settings::get(c, key)).expect("read")
}

/// Nothing an import in RAM leaves behind: no `.new`, no working directory in `ram`.
fn assert_clean(ram: &Path, db: &Db) {
    assert!(
        !sibling(db.path(), NEW_SUFFIX).exists(),
        "a .new file is left"
    );
    let left: Vec<_> = fs::read_dir(ram)
        .map(|d| d.flatten().map(|e| e.file_name()).collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "left {left:?}");
}

#[test]
fn readers_see_the_old_file_until_the_swap_and_the_new_one_after() {
    let ram = testutil::ram_dir();
    let (dir, db) = card(3 * CHUNK_BYTES);
    let mut watch = Script::default();
    let out = db
        .hold_writer_blocking(|h| {
            run(h, &plan(ram.path()), &mut watch, |ram| {
                ram.write_blocking(|c| settings::set(c, "k", "new"))?;
                let during = get(&db, "k");
                Ok((during, true))
            })
        })
        .expect("run");
    let Ram::Done(during, report) = out else {
        panic!("fell back: {out:?}");
    };
    assert_eq!(during.as_deref(), Some("old"), "a read during the import");
    assert_eq!(
        get(&db, "k").as_deref(),
        Some("new"),
        "a read after the swap"
    );
    assert_eq!(
        watch.phases,
        [Phase::Copying, Phase::Importing, Phase::Writing]
    );
    assert_eq!(report.bytes, fs::metadata(db.path()).expect("stat").len());
    db.write_blocking(|c| settings::set(c, "after", "1"))
        .expect("the reopened writer writes");
    assert_eq!(get(&db, "after").as_deref(), Some("1"));
    assert_clean(ram.path(), &db);
    // A fresh open sees one whole file, as a restart would.
    drop(db);
    let again = Db::open(&dir.path().join("test.db")).expect("reopen");
    assert_eq!(get(&again, "k").as_deref(), Some("new"));
}

#[test]
fn the_card_takes_about_one_write_per_mebibyte() {
    let ram = testutil::ram_dir();
    let (_dir, db) = card(5 * CHUNK_BYTES);
    let out = db
        .hold_writer_blocking(|h| {
            run(h, &plan(ram.path()), &mut Script::default(), |ram| {
                ram.write_blocking(|c| settings::set(c, "k", "new"))?;
                Ok(((), true))
            })
        })
        .expect("run");
    let Ram::Done((), report) = out else {
        panic!("fell back: {out:?}");
    };
    let Some(writes) = report.card_writes else {
        eprintln!("no per-thread I/O accounting; skipped");
        return;
    };
    let chunks = report.bytes.div_ceil(CHUNK_BYTES as u64);
    eprintln!("{} bytes, {writes} writes", report.bytes);
    // Reopening maps SQLite's shared-memory file, a one-byte write per 4 KiB page of it.
    assert!(writes >= chunks && writes <= chunks + 12, "{writes} writes");
}

#[test]
fn a_stop_during_the_import_or_the_write_back_leaves_the_card_byte_identical() {
    let ram = testutil::ram_dir();
    for stop_at in [Phase::Copying, Phase::Importing, Phase::Writing] {
        let (_dir, db) = card(3 * CHUNK_BYTES);
        let before = sha1(db.path());
        let mut watch = Script {
            stop_at: Some(stop_at).filter(|p| *p != Phase::Importing),
            ..Script::default()
        };
        let out = db.hold_writer_blocking(|h| {
            run(h, &plan(ram.path()), &mut watch, |ram| {
                ram.write_blocking(|c| settings::set(c, "k", "new"))?;
                if stop_at == Phase::Importing {
                    return Err(Error::Cancelled);
                }
                Ok(((), true))
            })
        });
        assert!(matches!(out, Err(Error::Cancelled)), "{stop_at:?}: {out:?}");
        assert_eq!(sha1(db.path()), before, "{stop_at:?}");
        assert_eq!(get(&db, "k").as_deref(), Some("old"));
        assert_clean(ram.path(), &db);
        db.write_blocking(|c| settings::set(c, "k", "later"))
            .expect("the writer still writes");
    }
}

#[test]
fn short_memory_or_a_full_copy_falls_back_with_the_card_untouched() {
    let ram = testutil::ram_dir();
    let (_dir, db) = card(1024);
    let before = sha1(db.path());
    let short = Plan {
        floor: u64::MAX / 2,
        ..plan(ram.path())
    };
    let mut ran = false;
    let out = db
        .hold_writer_blocking(|h| {
            run(h, &short, &mut Script::default(), |_| {
                ran = true;
                Ok(((), true))
            })
        })
        .expect("run");
    assert!(
        matches!(&out, Ram::Fallback(r) if r.detail.contains("memory available")),
        "{out:?}"
    );
    assert!(!ran, "the work never ran");

    let out = db
        .hold_writer_blocking(|h| {
            run(h, &plan(ram.path()), &mut Script::default(), |ram| {
                ram.write_blocking(|c| settings::set(c, "k", "new"))?;
                Err::<((), bool), _>(io::Error::from_raw_os_error(28).into())
            })
        })
        .expect("run");
    assert!(
        matches!(&out, Ram::Fallback(r) if r.detail.contains("memory ran out importing")),
        "{out:?}"
    );
    assert_eq!(sha1(db.path()), before);
    assert_clean(ram.path(), &db);
}

#[test]
fn work_that_changes_nothing_is_not_written_back() {
    let ram = testutil::ram_dir();
    let (_dir, db) = card(1024);
    let before = sha1(db.path());
    let out = db
        .hold_writer_blocking(|h| {
            run(h, &plan(ram.path()), &mut Script::default(), |ram| {
                Ok((get(ram, "k"), false))
            })
        })
        .expect("run");
    assert!(
        matches!(&out, Ram::Done(Some(v), r) if v == "old" && r.bytes == 0),
        "{out:?}"
    );
    assert_eq!(sha1(db.path()), before);
    assert_clean(ram.path(), &db);
}

#[test]
fn a_swap_refuses_a_file_it_cannot_rename_and_keeps_the_old_one() {
    let (dir, db) = card(1024);
    let missing = dir.path().join("absent.new");
    let r = db.hold_writer_blocking(|h| h.replace_file(&missing));
    assert!(
        matches!(r, Err(Error::File { ref path, .. }) if *path == missing),
        "{r:?}"
    );
    assert_eq!(get(&db, "k").as_deref(), Some("old"));
    db.write_blocking(|c| settings::set(c, "k", "kept"))
        .expect("reopened on the old file");
}

#[test]
fn another_process_holding_the_database_its_wal_or_its_shm_is_seen() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("m.db");
    for suffix in ["", "-wal", "-shm"] {
        let file = sibling(&db, suffix);
        fs::write(&file, b"x").expect("write");
        assert!(!held_elsewhere(&db));
        let child = std::process::Command::new("sleep")
            .arg("30")
            .stdin(File::open(&file).expect("open"))
            .spawn()
            .expect("sleep");
        let mut child = KillOnDrop(child);
        crate::testing::eventually_blocking("a child holding the file to be seen", || {
            held_elsewhere(&db)
        });
        child.0.kill().expect("kill");
        child.0.wait().expect("wait");
        assert!(!held_elsewhere(&db));
    }
}

#[test]
fn a_swap_whose_reopen_fails_is_its_own_error_and_never_a_fallback() {
    let (_dir, db) = card(1024);
    let new = sibling(db.path(), NEW_SUFFIX);
    fs::write(&new, b"not a database, renamed in all the same").expect("write");
    let r = db.hold_writer_blocking(|h| h.replace_file(&new));
    let Err(e) = r else {
        panic!("reopened a file that is not a database");
    };
    assert!(matches!(e, Error::Reopen(_)), "{e:?}");
    assert!(!storage_full(&e));
    assert!(
        db.read_blocking(|c| settings::get(c, "k")).is_err(),
        "every statement fails until a restart"
    );
    let full =
        rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_FULL), None);
    assert!(!storage_full(&Error::Reopen(Box::new(full.into()))));
}

#[test]
fn a_reopen_that_fails_after_the_rename_reports_it_over_the_swap() {
    let (dir, db) = card(1024);
    let other = dir.path().join("other.db");
    let copy = Db::open(&other).expect("open");
    copy.write_blocking(|c| settings::set(c, "k", "new"))
        .expect("set");
    copy.close().expect("close");
    let full =
        rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_FULL), None);
    let r = db.hold_writer_blocking(|h| h.replace_with(&other, |_| Err(full.into())));
    assert!(matches!(&r, Err(Error::Reopen(_))), "{r:?}");
    assert!(!other.exists(), "the swap itself ran");
    let again = Connection::open(db.path()).expect("open");
    let k = settings::get(&again, "k").expect("read");
    assert_eq!(k.as_deref(), Some("new"), "the card holds the new file");
}

/// A config over `root` whose import directory is `ram`.
fn config_at(root: &Path, ram: &Path) -> crate::config::Config {
    let mut config = crate::config::Config::default();
    config.paths.root = root.to_path_buf();
    config.paths.games = root.join("games");
    config.paths.data = root.join("data");
    config.memory.import_dir = ram.to_path_buf();
    fs::create_dir_all(&config.paths.data).expect("mkdir");
    config
}

/// A closed database at `path` holding `k = value`.
fn closed_with(path: &Path, value: &str) {
    let db = Db::open(path).expect("open");
    db.write_blocking(|c| settings::set(c, "k", value))
        .expect("set");
    db.close().expect("close");
}

#[test]
fn a_start_after_a_crash_at_any_step_of_the_swap_opens_one_whole_database() {
    let ram = testutil::ram_dir();
    // The files a crash leaves, `(db, .new, .old, .swap)`, and the value the start then
    // reads, `None` when it must refuse to start. A torn rename on exFAT leaves neither
    // of its names readable, which is why `.new` or nothing can stand alone.
    let cases = [
        (Some("old"), Some("partial"), None, false, Some("old")),
        (Some("old"), Some("new"), None, false, Some("old")),
        (Some("old"), Some("new"), None, true, Some("old")),
        (None, Some("new"), Some("old"), true, Some("new")),
        (None, Some("new"), None, true, Some("new")),
        (None, Some("new"), None, false, None),
        (Some("new"), None, Some("old"), true, Some("new")),
        (Some("new"), None, None, true, Some("new")),
        (Some("new"), None, None, false, Some("new")),
        (None, None, Some("old"), true, Some("old")),
        (None, None, None, true, None),
        (None, Some("partial"), None, true, None),
        (None, Some("partial"), Some("old"), true, None),
    ];
    for (i, (db_file, new_file, old_file, marked, want)) in cases.into_iter().enumerate() {
        let root = tempfile::tempdir().expect("tempdir");
        let mut config = config_at(root.path(), ram.path());
        let path = config.paths.db();
        let place = |value: Option<&str>, at: &Path| match value {
            Some("partial") => {
                closed_with(&root.path().join("whole.db"), "new");
                let bytes = fs::read(root.path().join("whole.db")).expect("read");
                fs::write(at, &bytes[..bytes.len() / 2]).expect("write");
            }
            Some(v) => {
                let made = root.path().join(format!("{v}.db"));
                closed_with(&made, v);
                fs::rename(&made, at).expect("place");
            }
            None => {}
        };
        place(db_file, &path);
        place(new_file, &sibling(&path, NEW_SUFFIX));
        place(old_file, &sibling(&path, OLD_SUFFIX));
        if marked {
            fs::write(sibling(&path, SWAP_SUFFIX), b"").expect("marker");
        }
        let opened = crate::app::open_db(&mut config);
        let Some(want) = want else {
            assert!(opened.is_err(), "case {i}: started");
            assert!(!path.exists(), "case {i}: created a database");
            if new_file.is_some() {
                assert!(
                    sibling(&path, NEW_SUFFIX).exists(),
                    "case {i}: .new removed"
                );
            }
            continue;
        };
        let db = opened.unwrap_or_else(|e| panic!("case {i}: {e}")).db;
        assert_eq!(get(&db, "k").as_deref(), Some(want), "case {i}");
        for suffix in [NEW_SUFFIX, OLD_SUFFIX, SWAP_SUFFIX] {
            assert!(!sibling(&path, suffix).exists(), "case {i}: {suffix} left");
        }
    }
}

#[test]
fn a_database_is_never_created_beside_the_files_of_a_swap() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("m.db");
    for suffix in [OLD_SUFFIX, NEW_SUFFIX, SWAP_SUFFIX] {
        fs::write(sibling(&path, suffix), b"").expect("write");
        assert!(Db::open(&path).is_err(), "created beside {suffix}");
        assert!(!path.exists());
        fs::remove_file(sibling(&path, suffix)).expect("remove");
    }
    Db::open(&path).expect("a fresh database elsewhere");
}

#[test]
fn a_start_after_a_torn_rename_in_a_migrations_swap_keeps_the_migrated_copy() {
    let ram = testutil::ram_dir();
    let root = tempfile::tempdir().expect("tempdir");
    let mut config = config_at(root.path(), ram.path());
    let path = config.paths.db();
    let latest = super::super::migrate::latest();
    at_version(&path, latest - 1, 0.01);
    let before = titles(&path);
    let new = sibling(&path, NEW_SUFFIX);
    fs::copy(&path, &new).expect("copy");
    Db::open(&new)
        .and_then(Db::close)
        .expect("migrate the copy");
    // The first rename torn: only the migrated copy and the marker can be read.
    fs::write(sibling(&path, SWAP_SUFFIX), b"").expect("marker");
    fs::remove_file(&path).expect("tear");
    let db = crate::app::open_db(&mut config).expect("open").db;
    let v = db
        .read_blocking(super::super::migrate::current_version)
        .expect("version");
    assert_eq!(v, latest);
    drop(db);
    assert_eq!(titles(&path), before);
    assert!(!new.exists() && !sibling(&path, SWAP_SUFFIX).exists());
}

#[test]
fn a_start_after_a_crash_before_a_migrations_swap_migrates_again() {
    let ram = testutil::ram_dir();
    let root = tempfile::tempdir().expect("tempdir");
    let mut config = config_at(root.path(), ram.path());
    let path = config.paths.db();
    let latest = super::super::migrate::latest();
    at_version(&path, latest - 1, 0.0);
    // The whole `.new` a crash between the write-back and the swap leaves.
    let new = sibling(&path, NEW_SUFFIX);
    fs::copy(&path, &new).expect("copy");
    Db::open(&new)
        .and_then(Db::close)
        .expect("migrate the copy");
    let before = sha1(&path);
    assert!(clean_stale(&path, ram.path()).expect("clean") >= 1);
    assert_eq!(sha1(&path), before, "the card file is not the copy");
    fs::copy(&path, &new).expect("copy again");
    let db = crate::app::open_db(&mut config).expect("open").db;
    let v = db
        .read_blocking(super::super::migrate::current_version)
        .expect("version");
    assert_eq!(v, latest);
    assert!(!new.exists());
}

#[test]
fn a_migration_that_fails_on_the_copy_leaves_the_card_as_it_was_and_runs_in_place() {
    let ram = testutil::ram_dir();
    let root = tempfile::tempdir().expect("tempdir");
    let mut config = config_at(root.path(), ram.path());
    let path = config.paths.db();
    // Migration 17 drops this table; without it that migration fails.
    at_version(&path, 16, 0.0);
    let c = Connection::open(&path).expect("open");
    c.execute_batch("DROP TABLE dat_stage").expect("drop");
    super::super::wal_emptied(&c).expect("checkpoint");
    drop(c);
    let before = sha1(&path);
    let r = migrate_in_ram(&path, &plan(ram.path()), None);
    assert!(matches!(r, Err(Error::Migration { .. })), "{r:?}");
    assert_eq!(sha1(&path), before, "the card is byte-identical");
    assert_eq!(fs::read_dir(ram.path()).map_or(0, Iterator::count), 0);
    let opened = crate::app::open_db(&mut config);
    assert!(
        matches!(opened, Err(Error::Migration { .. })),
        "the open migrated in place and failed the same way"
    );
    let c = Connection::open(&path).expect("open");
    let v = super::super::migrate::current_version(&c).expect("version");
    assert_eq!(v, 16, "rolled back");
    assert!(!sibling(&path, NEW_SUFFIX).exists());
}

#[test]
fn a_directory_off_tmpfs_or_beside_the_database_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("m.db");
    let beside = dir_refusal(&dir.path().join("ram"), &db).expect("refused");
    assert!(
        beside.contains("not in RAM") || beside.contains("same file system"),
        "{beside}"
    );
    let ram = testutil::ram_dir();
    assert_eq!(dir_refusal(ram.path(), &db), None);
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(ram.path(), &link).expect("symlink");
    let linked = dir_refusal(&link, &db).expect("refused");
    assert!(linked.contains("symlink"), "{linked}");
}

#[test]
fn memory_falling_below_the_floor_stops_the_work_with_a_fallback() {
    let (_dir, db) = card(1024);
    let ram = testutil::ram_dir();
    let before = sha1(db.path());
    let out = db
        .hold_writer_blocking(|h| {
            run(h, &plan(ram.path()), &mut Script::default(), |copy| {
                copy.write_blocking(|c| settings::set(c, "k", "new"))?;
                memory_left(u64::MAX)?;
                Ok(((), true))
            })
        })
        .expect("run");
    assert!(
        matches!(&out, Ram::Fallback(r) if r.detail.contains("fell to")),
        "{out:?}"
    );
    assert_eq!(sha1(db.path()), before);
    assert_clean(ram.path(), &db);
}

/// A WAL database at `path` migrated through `version` only, holding the synthetic
/// catalogue at `scale`, with its WAL emptied.
fn at_version(path: &Path, version: u32, scale: f64) {
    let mut c = Connection::open(path).expect("open");
    c.pragma_update(None, "journal_mode", "WAL").expect("wal");
    crate::db::migrate::apply_through(&mut c, version).expect("migrate");
    if version >= 24 {
        crate::db::platforms::seed(&mut c, &mistarr_mister::platforms::PLATFORMS).expect("seed");
    } else {
        for p in &mistarr_mister::platforms::PLATFORMS {
            c.execute(
                "INSERT INTO platforms (id, name, core_dir, kind) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![p.id, p.name, p.core_dir, p.kind.as_str()],
            )
            .expect("platform");
        }
    }
    // The synthetic catalogue writes the columns of the current schema.
    if version >= 26 {
        crate::synth::seed(&mut c, scale, 1).expect("synth");
    }
    super::super::wal_emptied(&c).expect("checkpoint");
}

fn titles(path: &Path) -> i64 {
    let c = Connection::open(path).expect("open");
    c.query_row("SELECT COUNT(*) FROM titles", [], |r| r.get(0))
        .expect("count")
}

#[test]
fn startup_migrations_run_in_ram_and_swap_in() {
    let ram = testutil::ram_dir();
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("m.db");
    let latest = super::super::migrate::latest();
    at_version(&db, latest - 1, 0.01);
    let before = titles(&db);
    let report = migrate_in_ram(&db, &plan(ram.path()), None)
        .expect("migrate")
        .expect("ran in RAM");
    assert_eq!(report.bytes, fs::metadata(&db).expect("stat").len());
    let c = Connection::open(&db).expect("open");
    assert_eq!(
        super::super::migrate::current_version(&c).expect("version"),
        latest
    );
    drop(c);
    assert_eq!(titles(&db), before);
    assert!(!sibling(&db, NEW_SUFFIX).exists());
    let left = fs::read_dir(ram.path()).map_or(0, Iterator::count);
    assert_eq!(left, 0);
    assert!(
        migrate_in_ram(&db, &plan(ram.path()), None)
            .expect("again")
            .is_none(),
        "nothing left to migrate"
    );
}

#[test]
fn startup_migrations_stay_on_the_card_when_memory_is_short_or_the_schema_is_newer() {
    let ram = testutil::ram_dir();
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("m.db");
    assert!(migrate_in_ram(&db, &plan(ram.path()), None)
        .expect("no database")
        .is_none());
    at_version(&db, super::super::migrate::latest() - 1, 0.01);
    let before = sha1(&db);
    let short = Plan {
        floor: u64::MAX / 2,
        ..plan(ram.path())
    };
    assert!(migrate_in_ram(&db, &short, None).expect("short").is_none());
    assert_eq!(sha1(&db), before, "left for the open to migrate");

    let newer = dir.path().join("newer.db");
    at_version(&newer, super::super::migrate::latest(), 0.0);
    Connection::open(&newer)
        .expect("open")
        .execute("INSERT INTO schema_version VALUES (9999, 'future', 1)", [])
        .expect("future");
    let too_new = migrate_in_ram(&newer, &plan(ram.path()), None);
    assert!(
        matches!(too_new, Err(Error::SchemaTooNew { found: 9999, .. })),
        "{too_new:?}"
    );
}

/// The last migration on the bench catalogue with half its roms lacking a sha1, as
/// arcade and export roms do, applied in place and in RAM: card writes.
#[test]
#[ignore = "about a minute; run by hand with --ignored --nocapture"]
fn the_last_migration_in_place_and_in_ram() {
    let ram = testutil::ram_dir();
    let dir = tempfile::tempdir().expect("tempdir");
    let latest = super::super::migrate::latest();
    let (a, b) = (dir.path().join("a.db"), dir.path().join("b.db"));
    at_version(&a, latest - 1, 1.0);
    let c = Connection::open(&a).expect("open");
    c.execute("UPDATE roms SET sha1 = NULL WHERE id % 2 = 0", [])
        .expect("md5 only");
    super::super::wal_emptied(&c).expect("checkpoint");
    drop(c);
    fs::copy(&a, &b).expect("copy");
    let size = fs::metadata(&a).expect("stat").len();
    let started = Instant::now();
    let (applied, writes) = counted(|| {
        let mut c = Connection::open(&a).expect("open");
        c.pragma_update(None, "temp_store", "MEMORY").expect("temp");
        super::super::migrate::apply(&mut c).expect("apply")
    });
    let in_place = started.elapsed();
    assert_eq!(applied, [latest]);
    let started = Instant::now();
    let r = migrate_in_ram(&b, &plan(ram.path()), None)
        .expect("migrate")
        .expect("in RAM");
    eprintln!(
        "migration {latest} on {size} bytes: in place {writes:?} writes, {in_place:?}; \
         in RAM {:?} card writes of {} bytes, {:?}",
        r.card_writes,
        r.bytes,
        started.elapsed()
    );
}

#[test]
fn the_budget_reads_memory_and_both_directories() {
    let dir = tempfile::tempdir().expect("tempdir");
    let b = Budget::read(dir.path(), &dir.path().join("m.db"));
    assert!(b.available.is_some_and(|a| a > 0), "{b:?}");
    assert!(b.ram_room.is_some() && b.card_room.is_some(), "{b:?}");
    let gone = Budget::read(&dir.path().join("none"), Path::new("m.db"));
    assert_eq!((gone.ram_room, gone.card_room), (None, None));
}

/// Loads `names` as one DAT's games on `platform`, committed as a DAT import commits them.
fn load_games(
    c: &mut rusqlite::Connection,
    platform: &str,
    dat: &str,
    names: &[&str],
) -> Result<()> {
    use crate::db::{dats, titles};
    let tx = c.transaction()?;
    let v = dats::NewVersion {
        dat_name: dat,
        version: "1",
        source_file: "a.dat",
        platform: Some(key(&pid(platform))),
        now: 1,
    };
    let version = dats::upsert_version(&tx, &v)?.id;
    for name in names {
        let t = titles::TitleInput {
            name,
            base_name: name,
            group_key: name,
            clone_of: None,
            regions: &[],
            languages: &[],
            revision: None,
            flags: &[],
        };
        titles::upsert_title(&tx, &platform_ref(&pid(platform)), version, &t, &[])?;
    }
    titles::recompute::link_parents(&tx, version, true)?;
    titles::recompute::recompute_platform(
        &tx,
        key(&pid(platform)),
        &mistarr_core::select::Prefs::default(),
    )?;
    crate::db::commit(tx)
}

/// Groups on `platform` a browse finds, searching for `q` when given.
fn found(db: &Db, platform: &str, q: Option<&str>) -> u64 {
    let filter = crate::db::titles::browse::Browse {
        q: q.map(str::to_owned),
        ..crate::db::titles::browse::Browse::default()
    };
    let page = crate::db::sql::Page {
        limit: 10,
        offset: 0,
    };
    db.read_blocking(|c| {
        crate::db::titles::browse::browse(c, &platform_ref(&pid(platform)), &filter, page)
    })
    .expect("browse")
    .total
}

#[test]
fn a_search_run_before_an_import_finds_its_titles_after_the_swap_and_in_place() {
    let ram = testutil::ram_dir();
    let (_dir, db) = testutil::db();
    db.write_blocking(|c| {
        crate::db::platforms::seed(c, &mistarr_mister::platforms::PLATFORMS)?;
        load_games(c, "nes", "Maker - Console", &["Zorbl Harbor"])
    })
    .expect("seed");
    // The reader's statements and index state are warm on both platforms first.
    assert_eq!(found(&db, "nes", Some("zorbl")), 1);
    assert_eq!(found(&db, "gba", Some("zorbl")), 0);
    let out = db
        .hold_writer_blocking(|h| {
            run(h, &plan(ram.path()), &mut Script::default(), |copy| {
                copy.write_blocking(|c| {
                    load_games(
                        c,
                        "gba",
                        "Maker - Handheld",
                        &["Zorbl Quest", "Vexil Plinth"],
                    )
                })?;
                Ok((found(&db, "gba", Some("zorbl")), true))
            })
        })
        .expect("run");
    let Ram::Done(during, _) = out else {
        panic!("fell back: {out:?}");
    };
    assert_eq!(during, 0, "a search during the import reads the old file");
    assert_eq!(found(&db, "gba", None), 2);
    assert_eq!(found(&db, "gba", Some("zorbl")), 1);
    assert_eq!(found(&db, "gba", Some("plin")), 1);
    assert_eq!(found(&db, "nes", Some("zorbl")), 1);
    db.write_blocking(|c| load_games(c, "gba", "Maker - Handheld", &["Zorbl Tessel"]))
        .expect("in place");
    assert_eq!(found(&db, "gba", None), 3);
    assert_eq!(found(&db, "gba", Some("zorbl")), 2);
}

/// Kills the child when a failed assertion unwinds past it.
struct KillOnDrop(std::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Every table's rows in rowid order where it has one, and the schema, as text.
fn dump(c: &Connection) -> Vec<String> {
    let mut out = Vec::new();
    let mut schema = c
        .prepare("SELECT type, name, tbl_name, COALESCE(sql, '') FROM sqlite_master ORDER BY name")
        .expect("schema");
    let rows = schema
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(3)?,
            ))
        })
        .expect("rows");
    let mut tables = Vec::new();
    for row in rows {
        let (kind, name, sql) = row.expect("row");
        out.push(format!("{kind} {name} {sql}"));
        if kind == "table" && !name.starts_with("sqlite_") {
            tables.push((name, sql.to_uppercase().contains("WITHOUT ROWID")));
        }
    }
    for (name, without_rowid) in tables {
        let head = if without_rowid { "" } else { "rowid, " };
        let order = if without_rowid { "1" } else { "rowid" };
        let Ok(mut stmt) = c.prepare(&format!("SELECT {head}* FROM \"{name}\" ORDER BY {order}"))
        else {
            continue;
        };
        let width = stmt.column_count();
        let mut rows = stmt.query([]).expect("query");
        while let Some(r) = rows.next().expect("next") {
            let cells: Vec<String> = (0..width)
                .map(|i| format!("{:?}", r.get_ref(i).expect("cell")))
                .collect();
            out.push(format!("{name}: {}", cells.join("|")));
        }
    }
    out
}

/// Fills a table of about `mib` MiB, with a rowid-only table beside it that loses every
/// other row, then drops the big one: most of the file is free pages.
fn churn(c: &Connection, mib: usize) -> Result<()> {
    c.execute_batch(
        "CREATE TABLE big (id INTEGER PRIMARY KEY, v BLOB);
         CREATE TABLE loose (a, b);
         WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 200)
           INSERT INTO loose (a, b) SELECT i, 'row ' || i FROM n;
         DELETE FROM loose WHERE a % 2 = 0;",
    )?;
    let mut add = c.prepare("INSERT INTO big (v) VALUES (zeroblob(65536))")?;
    for _ in 0..mib * 16 {
        add.execute([])?;
    }
    drop(add);
    c.execute_batch("DROP TABLE big")?;
    Ok(())
}

/// A finished copy in WAL mode in a new directory in RAM, with `mib` MiB of free pages.
fn fragmented(ram: &Path, mib: usize) -> (PathBuf, PathBuf) {
    let work = ram.join("work");
    fs::create_dir_all(&work).expect("work dir");
    let copy = work.join(COPY_NAME);
    let c = Connection::open(&copy).expect("open");
    c.pragma_update_and_check(None, "journal_mode", "WAL", |_| Ok(()))
        .expect("wal");
    c.pragma_update(None, "user_version", 41).expect("version");
    c.pragma_update(None, "application_id", 7).expect("id");
    churn(&c, mib).expect("churn");
    super::super::wal_emptied(&c).expect("checkpoint");
    c.close().expect("close");
    (work, copy)
}

#[test]
fn a_copy_with_many_free_pages_goes_back_compacted_with_identical_rows() {
    let ram = testutil::ram_dir();
    let (_dir, db) = card(CHUNK_BYTES);
    let mut watch = Script::default();
    let out = db
        .hold_writer_blocking(|h| {
            run(h, &plan(ram.path()), &mut watch, |copy| {
                copy.write_blocking(|c| churn(c, 6))?;
                let rows = copy.read_blocking(|c| Ok(dump(c)))?;
                let size = file_len(copy.path())?;
                Ok(((rows, size), true))
            })
        })
        .expect("run");
    let Ram::Done((rows, copy_bytes), report) = out else {
        panic!("fell back: {out:?}");
    };
    assert_eq!(
        watch.phases,
        [
            Phase::Copying,
            Phase::Importing,
            Phase::Compacting,
            Phase::Writing
        ]
    );
    let after = file_len(db.path()).expect("len");
    assert_eq!(report.bytes, after);
    assert!(after * 2 < copy_bytes, "{after} of {copy_bytes} bytes");
    assert_eq!(db.read_blocking(|c| Ok(dump(c))).expect("dump"), rows);
    let (free, mode, loose): (i64, String, i64) = db
        .read_blocking(|c| {
            Ok((
                c.pragma_query_value(None, "freelist_count", |r| r.get(0))?,
                c.pragma_query_value(None, "journal_mode", |r| r.get(0))?,
                c.query_row("SELECT MAX(rowid) FROM loose", [], |r| r.get(0))?,
            ))
        })
        .expect("read");
    assert_eq!((free, mode.as_str(), loose), (0, "wal", 199));
    if let Some(writes) = report.card_writes {
        let chunks = report.bytes.div_ceil(CHUNK_BYTES as u64);
        assert!(writes >= chunks && writes <= chunks + 12, "{writes} writes");
    }
    assert_clean(ram.path(), &db);
}

#[test]
fn the_compacted_file_keeps_its_header_and_goes_back_one_write_per_mebibyte() {
    let ram = testutil::ram_dir();
    let (work, copy) = fragmented(ram.path(), 6);
    let whole = file_len(&copy).expect("len");
    let source = compact(&copy, &work, 0, &mut || {});
    assert_eq!(source, work.join(COMPACT_NAME));
    assert!(!copy.exists(), "the copy no longer holds RAM");
    let size = file_len(&source).expect("len");
    assert!(size * 2 < whole, "{size} of {whole} bytes");
    let c = Connection::open(&source).expect("open");
    let read = |name: &str| -> i64 { c.pragma_query_value(None, name, |r| r.get(0)).expect(name) };
    assert_eq!(read("user_version"), 41);
    assert_eq!(read("application_id"), 7);
    assert_eq!(read("freelist_count"), 0);
    let mode: String = c
        .pragma_query_value(None, "journal_mode", |r| r.get(0))
        .expect("mode");
    assert_eq!(mode, "wal");
    drop(c);
    let mut sink = Counting::default();
    let mut from = File::open(&source).expect("open");
    let n = copy_durable(&mut from, &mut sink, &mut || Ok(())).expect("copy");
    assert_eq!(n, size);
    assert_eq!(sink.writes.len() as u64, size.div_ceil(CHUNK_BYTES as u64));
}

#[test]
fn a_copy_with_few_free_pages_is_written_back_as_it_is() {
    let ram = testutil::ram_dir();
    let work = ram.path().join("work");
    fs::create_dir_all(&work).expect("work dir");
    let copy = work.join(COPY_NAME);
    let c = Connection::open(&copy).expect("open");
    c.execute_batch(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, v BLOB);
         INSERT INTO t (v) VALUES (zeroblob(400000));",
    )
    .expect("fill");
    c.close().expect("close");
    let mut announced = false;
    let source = compact(&copy, &work, 0, &mut || announced = true);
    assert_eq!(source, copy);
    assert!(!announced && !work.join(COMPACT_NAME).exists());
}

#[test]
fn a_copy_just_under_the_threshold_is_not_compacted() {
    let ram = testutil::ram_dir();
    let work = ram.path().join("work");
    fs::create_dir_all(&work).expect("work dir");
    let copy = work.join(COPY_NAME);
    let c = Connection::open(&copy).expect("open");
    c.execute_batch(
        "CREATE TABLE a (id INTEGER PRIMARY KEY, v BLOB);
         CREATE TABLE b (id INTEGER PRIMARY KEY, v BLOB);
         INSERT INTO a (v) VALUES (zeroblob(4096 * 80));
         INSERT INTO b (v) VALUES (zeroblob(4096 * 20));
         DROP TABLE b;",
    )
    .expect("fill");
    let (pages, free): (i64, i64) = (
        c.pragma_query_value(None, "page_count", |r| r.get(0))
            .expect("pages"),
        c.pragma_query_value(None, "freelist_count", |r| r.get(0))
            .expect("free"),
    );
    assert!(free > 0 && free * 4 <= pages, "{free} of {pages}");
    c.close().expect("close");
    let budget = Budget {
        available: Some(u64::MAX),
        ram_room: Some(u64::MAX),
        card_room: None,
    };
    let r = compacted(&copy, &work.join(COMPACT_NAME), 0, &budget, &mut || {});
    assert_eq!(r, Ok(None));
}

#[test]
fn short_memory_or_room_skips_compaction_and_leaves_the_copy() {
    let ram = testutil::ram_dir();
    let (work, copy) = fragmented(ram.path(), 4);
    let before = sha1(&copy);
    let roomy = Budget {
        available: Some(1 << 40),
        ram_room: Some(1 << 40),
        card_room: None,
    };
    let out = work.join(COMPACT_NAME);
    let cases = [
        (
            Budget {
                available: None,
                ..roomy
            },
            "cannot be read",
        ),
        (
            Budget {
                available: Some(MIB),
                ..roomy
            },
            "MiB of memory available",
        ),
        (
            Budget {
                ram_room: Some(0),
                ..roomy
            },
            "free in the memory directory",
        ),
        (
            Budget {
                ram_room: None,
                ..roomy
            },
            "cannot be read",
        ),
    ];
    for (budget, want) in cases {
        let mut announced = false;
        let r = compacted(&copy, &out, 0, &budget, &mut || announced = true);
        assert!(
            matches!(&r, Err(why) if why.contains(want)),
            "{want}: {r:?}"
        );
        assert!(!announced && !out.exists());
    }
    let floor = u64::MAX / 2;
    assert_eq!(compact(&copy, &work, floor, &mut || {}), copy);
    assert_eq!(sha1(&copy), before);
}

#[test]
fn a_failing_vacuum_falls_back_to_the_copy() {
    let ram = testutil::ram_dir();
    let (work, copy) = fragmented(ram.path(), 4);
    fs::write(work.join(COMPACT_NAME), b"in the way").expect("blocker");
    let mut announced = false;
    let source = compact(&copy, &work, 0, &mut || announced = true);
    assert!(announced, "the compaction started");
    assert_eq!(source, copy);
    assert!(!work.join(COMPACT_NAME).exists(), "no partial file is left");
    let c = Connection::open(&copy).expect("the copy still opens");
    let loose: i64 = c
        .query_row("SELECT COUNT(*) FROM loose", [], |r| r.get(0))
        .expect("count");
    assert_eq!(loose, 100);
}

#[test]
fn the_compaction_check_names_what_is_short() {
    let roomy = Budget {
        available: Some(300 * MIB),
        ram_room: Some(100 * MIB),
        card_room: None,
    };
    let need = 128 * MIB + 70 * MIB + COMPACT_MARGIN_BYTES;
    assert_eq!(compaction_refusal(&roomy, 128 * MIB, 70 * MIB), Ok(()));
    let exact = Budget {
        available: Some(need),
        ..roomy
    };
    assert_eq!(compaction_refusal(&exact, 128 * MIB, 70 * MIB), Ok(()));
    let under = Budget {
        available: Some(need - 1),
        ..roomy
    };
    assert!(compaction_refusal(&under, 128 * MIB, 70 * MIB)
        .is_err_and(|e| e.contains("214 MiB needed")));
    assert!(compaction_refusal(&roomy, 0, 101 * MIB).is_err_and(|e| e.contains("100 MiB free")));
}

#[test]
fn a_startup_migration_compacts_the_free_pages_it_finds() {
    let ram = testutil::ram_dir();
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("m.db");
    at_version(&db, super::super::migrate::latest() - 1, 0.01);
    let c = Connection::open(&db).expect("open");
    churn(&c, 6).expect("churn");
    super::super::wal_emptied(&c).expect("checkpoint");
    c.close().expect("close");
    let before = (titles(&db), file_len(&db).expect("len"));
    let report = migrate_in_ram(&db, &plan(ram.path()), None)
        .expect("migrate")
        .expect("ran in RAM");
    assert_eq!(report.bytes, file_len(&db).expect("len"));
    assert!(
        report.bytes * 2 < before.1,
        "{} of {} bytes",
        report.bytes,
        before.1
    );
    assert_eq!(titles(&db), before.0);
    let c = Connection::open(&db).expect("open");
    let free: i64 = c
        .pragma_query_value(None, "freelist_count", |r| r.get(0))
        .expect("free");
    assert_eq!(free, 0);
    assert_eq!(fs::read_dir(ram.path()).map_or(0, Iterator::count), 0);
}

/// Every table's rows and the schema of the database at `path`.
fn dump_file(path: &Path) -> Vec<String> {
    let c = Connection::open(path).expect("open");
    dump(&c)
}

#[test]
fn the_real_schema_survives_compaction() {
    let ram = testutil::ram_dir();
    let work = ram.path().join("work");
    fs::create_dir_all(&work).expect("work dir");
    let copy = work.join(COPY_NAME);
    at_version(&copy, super::super::migrate::latest(), 0.02);
    let c = Connection::open(&copy).expect("open");
    churn(&c, 6).expect("churn");
    let live: i64 = c
        .query_row("SELECT COUNT(*) FROM titles", [], |r| r.get(0))
        .expect("count");
    assert!(live > 0, "the catalogue has titles");
    // Free pages are what matters here, not the rows the deleted titles leave dangling.
    c.pragma_update(None, "foreign_keys", false)
        .expect("keys off");
    let gone = c
        .execute("DELETE FROM titles WHERE id % 3 = 0", [])
        .expect("delete titles");
    assert!(gone > 0, "some titles are deleted");
    super::super::wal_emptied(&c).expect("checkpoint");
    c.close().expect("close");
    let before = dump_file(&copy);
    let source = compact(&copy, &work, 0, &mut || {});
    assert_eq!(source, work.join(COMPACT_NAME), "compacted");
    let c = Connection::open(&source).expect("open");
    let ok: String = c
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .expect("integrity");
    assert_eq!(ok, "ok");
    c.execute(
        "INSERT INTO title_search(title_search) VALUES('integrity-check')",
        [],
    )
    .expect("the search index matches its content");
    assert_eq!(dump(&c), before);
}

fn roomy() -> Budget {
    Budget {
        available: Some(u64::MAX),
        ram_room: Some(u64::MAX),
        card_room: None,
    }
}

fn short() -> Budget {
    Budget {
        available: Some(0),
        ..roomy()
    }
}

/// Runs an import on a fresh card database whose copy ends with many free pages, reading
/// the memory through `probe`; returns the rows and the copy's size.
fn import_with(ram: &Path, db: &Db, probe: Probe<'_>) -> (Vec<String>, u64, Script, Report) {
    let mut watch = Script::default();
    let out = db
        .hold_writer_blocking(|h| {
            run_with(
                h,
                &plan(ram),
                &mut watch,
                |copy| {
                    copy.write_blocking(|c| churn(c, 6))?;
                    let rows = copy.read_blocking(|c| Ok(dump(c)))?;
                    Ok(((rows, file_len(copy.path())?), true))
                },
                probe,
            )
        })
        .expect("run");
    let Ram::Done((rows, copy_bytes), report) = out else {
        panic!("fell back: {out:?}");
    };
    (rows, copy_bytes, watch, report)
}

fn free_pages(db: &Db) -> i64 {
    db.read_blocking(|c| Ok(c.pragma_query_value(None, "freelist_count", |r| r.get(0))?))
        .expect("free")
}

#[test]
fn a_copy_refused_for_memory_is_compacted_from_the_card_after_the_swap() {
    let ram = testutil::ram_dir();
    let (_dir, db) = card(CHUNK_BYTES);
    let mut calls = 0;
    let mut probe = |_: &Path| {
        calls += 1;
        if calls == 1 {
            short()
        } else {
            roomy()
        }
    };
    let (rows, copy_bytes, watch, report) = import_with(ram.path(), &db, &mut probe);
    assert_eq!(calls, 2, "the memory is checked again after the swap");
    assert_eq!(
        watch.phases,
        [
            Phase::Copying,
            Phase::Importing,
            Phase::Writing,
            Phase::Compacting,
            Phase::Writing
        ]
    );
    let after = file_len(db.path()).expect("len");
    assert_eq!(report.bytes, after);
    assert!(after * 2 < copy_bytes, "{after} of {copy_bytes} bytes");
    assert_eq!(db.read_blocking(|c| Ok(dump(c))).expect("dump"), rows);
    assert_eq!(free_pages(&db), 0);
    if let Some(writes) = report.card_writes {
        let chunks = (copy_bytes + after).div_ceil(CHUNK_BYTES as u64);
        assert!(writes >= chunks && writes <= chunks + 24, "{writes} writes");
    }
    assert_clean(ram.path(), &db);
}

#[test]
fn a_second_check_that_fails_leaves_the_written_back_card_file() {
    let ram = testutil::ram_dir();
    let (_dir, db) = card(CHUNK_BYTES);
    let mut calls = 0;
    let mut probe = |_: &Path| {
        calls += 1;
        short()
    };
    let (rows, copy_bytes, watch, _) = import_with(ram.path(), &db, &mut probe);
    assert_eq!(calls, 2);
    assert_eq!(watch.phases.last(), Some(&Phase::Writing));
    assert_eq!(watch.phases.len(), 3, "no second compaction began");
    assert!(file_len(db.path()).expect("len") * 10 > copy_bytes * 9);
    assert!(free_pages(&db) > 0);
    assert_eq!(db.read_blocking(|c| Ok(dump(c))).expect("dump"), rows);
    assert_clean(ram.path(), &db);
}

#[test]
fn a_vacuum_failing_after_the_swap_leaves_the_card_file() {
    let ram = testutil::ram_dir();
    let (_dir, db) = card(CHUNK_BYTES);
    let mut calls = 0;
    let mut probe = |dir: &Path| {
        calls += 1;
        if calls == 1 {
            return short();
        }
        fs::write(dir.join(COMPACT_NAME), b"in the way").expect("blocker");
        roomy()
    };
    let (rows, copy_bytes, watch, _) = import_with(ram.path(), &db, &mut probe);
    assert_eq!(calls, 2);
    assert_eq!(watch.phases.last(), Some(&Phase::Compacting));
    assert!(file_len(db.path()).expect("len") * 10 > copy_bytes * 9);
    assert!(free_pages(&db) > 0);
    assert_eq!(db.read_blocking(|c| Ok(dump(c))).expect("dump"), rows);
    assert_clean(ram.path(), &db);
}

#[test]
fn a_copy_compacted_before_the_write_back_is_not_compacted_again() {
    let ram = testutil::ram_dir();
    let (_dir, db) = card(CHUNK_BYTES);
    let mut calls = 0;
    let mut probe = |_: &Path| {
        calls += 1;
        roomy()
    };
    let (_, _, watch, _) = import_with(ram.path(), &db, &mut probe);
    assert_eq!(calls, 1);
    assert_eq!(
        watch
            .phases
            .iter()
            .filter(|p| **p == Phase::Compacting)
            .count(),
        1
    );
    assert_eq!(free_pages(&db), 0);
}

#[test]
fn a_startup_migration_refused_compaction_compacts_after_the_swap() {
    let ram = testutil::ram_dir();
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("m.db");
    at_version(&db, super::super::migrate::latest() - 1, 0.01);
    let c = Connection::open(&db).expect("open");
    churn(&c, 6).expect("churn");
    super::super::wal_emptied(&c).expect("checkpoint");
    c.close().expect("close");
    let before = (titles(&db), file_len(&db).expect("len"));
    let mut calls = 0;
    let mut probe = |_: &Path| {
        calls += 1;
        if calls == 1 {
            short()
        } else {
            roomy()
        }
    };
    let report = migrate_with(&db, &plan(ram.path()), None, &mut probe)
        .expect("migrate")
        .expect("ran in RAM");
    assert_eq!(calls, 2);
    assert_eq!(report.bytes, file_len(&db).expect("len"));
    assert!(
        report.bytes * 2 < before.1,
        "{} of {}",
        report.bytes,
        before.1
    );
    assert_eq!(titles(&db), before.0);
    let c = Connection::open(&db).expect("open");
    let free: i64 = c
        .pragma_query_value(None, "freelist_count", |r| r.get(0))
        .expect("free");
    assert_eq!(free, 0);
    assert_eq!(fs::read_dir(ram.path()).map_or(0, Iterator::count), 0);
    assert!(!sibling(&db, NEW_SUFFIX).exists());
}

#[test]
fn a_startup_migration_whose_second_check_fails_keeps_the_migrated_file() {
    let ram = testutil::ram_dir();
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("m.db");
    let latest = super::super::migrate::latest();
    at_version(&db, latest - 1, 0.01);
    let c = Connection::open(&db).expect("open");
    churn(&c, 6).expect("churn");
    super::super::wal_emptied(&c).expect("checkpoint");
    c.close().expect("close");
    let rows = titles(&db);
    let mut calls = 0;
    let mut probe = |_: &Path| {
        calls += 1;
        short()
    };
    migrate_with(&db, &plan(ram.path()), None, &mut probe)
        .expect("migrate")
        .expect("ran in RAM");
    assert_eq!(calls, 2);
    let c = Connection::open(&db).expect("open");
    assert_eq!(
        super::super::migrate::current_version(&c).expect("version"),
        latest
    );
    let free: i64 = c
        .pragma_query_value(None, "freelist_count", |r| r.get(0))
        .expect("free");
    assert!(free > 0);
    drop(c);
    assert_eq!(titles(&db), rows);
    assert_eq!(fs::read_dir(ram.path()).map_or(0, Iterator::count), 0);
}
