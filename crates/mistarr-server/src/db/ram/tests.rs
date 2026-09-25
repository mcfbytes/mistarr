//! Tests of the import in RAM: the chunked copy, the check, the swap and every way out.

use super::*;
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
fn meminfo_gives_the_available_bytes() {
    let text = "MemTotal:  498000 kB\nMemFree: 1 kB\nMemAvailable:   378000 kB\n";
    assert_eq!(mem_available(text), Some(378_000 * 1024));
    assert_eq!(mem_available("MemTotal: 1 kB\n"), None);
    assert_eq!(mem_available("MemAvailable: lots kB\n"), None);
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
        job: 7,
        input: 0,
    }
}

fn sha1(path: &Path) -> String {
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
    assert!(matches!(r, Err(Error::Io(_))), "{r:?}");
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
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .stdin(File::open(&file).expect("open"))
            .spawn()
            .expect("sleep");
        let seen = (0..200).any(|_| {
            let held = held_elsewhere(&db);
            if !held {
                std::thread::sleep(Duration::from_millis(10));
            }
            held
        });
        child.kill().expect("kill");
        child.wait().expect("wait");
        assert!(seen, "a child holding {suffix:?} is seen");
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
        let (db, _, _) = opened.unwrap_or_else(|e| panic!("case {i}: {e}"));
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
    let (db, _, _) = crate::app::open_db(&mut config).expect("open");
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
    let (db, _, _) = crate::app::open_db(&mut config).expect("open");
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
    c.execute_batch(
        "CREATE TABLE schema_version (version INTEGER PRIMARY KEY, name TEXT NOT NULL, \
         applied_at INTEGER NOT NULL)",
    )
    .expect("schema_version");
    for m in super::super::migrate::MIGRATIONS
        .iter()
        .filter(|m| m.version <= version)
    {
        // Each migration in one transaction, as `migrate::apply` runs them.
        c.execute_batch("BEGIN").expect("begin");
        c.execute_batch(m.sql).expect("migration");
        if super::super::has_table(&c, "title_groups_dirty").expect("table") {
            super::super::groups::flush(&c).expect("flush");
        }
        c.execute(
            "INSERT INTO schema_version VALUES (?1, ?2, 1)",
            rusqlite::params![m.version, m.name],
        )
        .expect("record");
        c.execute_batch("COMMIT").expect("end");
    }
    super::super::platforms::seed(&mut c, &mistarr_mister::platforms::PLATFORMS).expect("seed");
    crate::synth::seed(&mut c, scale, 1).expect("synth");
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
