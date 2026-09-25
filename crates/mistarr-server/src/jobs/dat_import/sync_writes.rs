//! Write syscalls of a DAT load and its recompute; see `docs/ARCHITECTURE.md` "Writes on a sync mount".

use std::fmt::Write as _;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use super::*;
use crate::db::dat_stage::{StagedGame, StagedRom};
use crate::db::files::{FileId, FileState, Hashed};

/// Write syscalls and bytes the calling thread made so far, from `/proc/thread-self/io`;
/// `None` where the kernel does not account them.
fn thread_writes() -> Option<(u64, u64)> {
    let io = std::fs::read_to_string("/proc/thread-self/io").ok()?;
    let field = |name: &str| {
        io.lines()
            .find_map(|l| l.strip_prefix(name))
            .and_then(|v| v.trim().parse::<u64>().ok())
    };
    Some((field("syscw:")?, field("wchar:")?))
}

/// Write syscalls and bytes `f` makes on the calling thread.
fn writes_of<T>(f: impl FnOnce() -> T) -> (T, Option<(u64, u64)>) {
    let before = thread_writes();
    let out = f();
    let after = thread_writes();
    let delta = before
        .zip(after)
        .map(|((c0, b0), (c1, b1))| (c1 - c0, b1 - b0));
    (out, delta)
}

/// A deterministic xorshift generator for hashes.
struct Rng(u64);

impl Rng {
    fn hex(&mut self, digits: usize) -> String {
        let mut out = String::with_capacity(digits);
        while out.len() < digits {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            let _ = write!(out, "{:016x}", self.0);
        }
        out.truncate(digits);
        out
    }
}

/// One track of the DAT, as a scan that found it before the DAT loaded stores it.
struct Track {
    dir: String,
    name: String,
    size: i64,
    crc32: String,
    md5: String,
    sha1: String,
}

/// A Logiqx DAT of `games` disc entries binding to psx under a family of its own, each a
/// cue sheet and one to three tracks with every hash, and the tracks it lists.
fn psx_dat(games: usize) -> (String, Vec<Track>) {
    let psx = mistarr_mister::platforms::by_id("psx").expect("psx");
    let header = format!("Example Vendor - {}", psx.name);
    let mut xml = format!(
        "<?xml version=\"1.0\"?>\n<datafile><header><name>{header}</name>\
         <version>2</version></header>\n"
    );
    let mut rng = Rng(0x5eed_0042);
    let mut tracks = Vec::new();
    for (i, name) in crate::synth::game_names(games, 42).iter().enumerate() {
        let _ = writeln!(
            xml,
            "<game name=\"{name}\"><description>{name}</description>"
        );
        let mut rom = |file: String, size: i64, rng: &mut Rng| {
            let (crc32, md5, sha1) = (rng.hex(8), rng.hex(32), rng.hex(40));
            let _ = writeln!(
                xml,
                "<rom name=\"{file}\" size=\"{size}\" crc=\"{crc32}\" md5=\"{md5}\" sha1=\"{sha1}\"/>"
            );
            Track {
                dir: format!("PSX/{name}"),
                name: file,
                size,
                crc32,
                md5,
                sha1,
            }
        };
        tracks.push(rom(format!("{name}.cue"), 300, &mut rng));
        for t in 1..=1 + i % 3 {
            let size = 1_000_000 + i64::try_from(i * 7 + t).unwrap_or(0);
            tracks.push(rom(format!("{name} (Track {t}).bin"), size, &mut rng));
        }
        xml.push_str("</game>\n");
    }
    xml.push_str("</datafile>\n");
    (xml, tracks)
}

/// A file database holding the synthetic catalogue at `scale` and `unmatched` hashed psx
/// files no rom matches yet, in disc directories of three, a third of them tracks of `dat`.
/// Its writer keeps temporary tables in memory, so only database and WAL writes count.
fn catalogue(dir: &Path, scale: f64, unmatched: usize, dat: &[Track]) -> Db {
    let db = Db::open(&dir.join("sync.db")).expect("open");
    let psx = PlatformId("psx".into());
    db.write_blocking(|c| {
        crate::db::platforms::seed(c, &mistarr_mister::platforms::PLATFORMS)?;
        crate::synth::seed(c, scale, 1)?;
        let tx = c.transaction()?;
        let mut rng = Rng(0x0dd_f11e);
        for i in 0..unmatched {
            let known = dat.get(i).filter(|_| i % 3 == 0);
            let own = Track {
                dir: format!("PSX/Unlisted Disc {:05}", i / 3),
                name: format!("Unlisted Disc {:05} (Track {}).bin", i / 3, i % 3 + 1),
                size: 2_000_000,
                crc32: rng.hex(8),
                md5: rng.hex(32),
                sha1: rng.hex(40),
            };
            let t = known.unwrap_or(&own);
            let hashed = Hashed {
                crc32: Some(&t.crc32),
                md5: Some(&t.md5),
                sha1: Some(&t.sha1),
                header_rule: Some("none"),
            };
            let path = format!("{}/{}", t.dir, t.name);
            files::upsert(
                &tx,
                &psx,
                &path,
                t.size,
                1,
                &hashed,
                None,
                FileState::Unverified,
                1,
            )?;
        }
        crate::db::commit(tx)?;
        c.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
        // Temporary files live in RAM on the board; here they stay out of the count.
        c.pragma_update(None, "temp_store", "MEMORY")?;
        Ok(())
    })
    .expect("catalogue");
    db
}

/// The recompute job's passes over psx, each chunk in its own transaction, as it runs them.
fn recompute(db: &Db) {
    let psx = PlatformId("psx".into());
    loop {
        let taken = db
            .write_blocking(|c| {
                let tx = c.transaction()?;
                let taken = rematch_chunk(&tx, &psx)?;
                crate::db::commit(tx)?;
                Ok(taken)
            })
            .expect("rematch");
        if taken < REMATCH_CHUNK as usize {
            break;
        }
    }
    let mut after = FileId(0);
    loop {
        let chunk = db
            .write_blocking(|c| {
                let tx = c.transaction()?;
                let chunk = match_unmatched_chunk(&tx, &psx, after)?;
                crate::db::commit(tx)?;
                Ok(chunk)
            })
            .expect("unmatched");
        after = chunk.last;
        if chunk.read < REMATCH_CHUNK as usize {
            break;
        }
    }
    db.write_blocking(|c| {
        let tx = c.transaction()?;
        titles::recompute_platform(&tx, "psx", &Prefs::default())?;
        crate::db::commit(tx)
    })
    .expect("recompute");
}

fn request() -> Request {
    Request {
        source_file: "psx.dat".into(),
        file_stem: "psx".into(),
        bind: None,
        prefs: Prefs::default(),
        now: 2,
        stop: watch::channel(false).1,
        gate: watch::channel(GateState::default()).1,
        meter: None,
        abort_on_hold: false,
    }
}

/// Loads `xml` and runs the recompute, returning the write syscalls and bytes of each.
fn measure(db: &Db, xml: &str) -> Option<[(u64, u64); 2]> {
    let (outcome, import) =
        writes_of(|| import_member(db, Cursor::new(xml.as_bytes()), &request(), "").expect("load"));
    assert!(matches!(outcome, Outcome::Loaded(_)), "{outcome:?}");
    let start = std::time::Instant::now();
    let ((), recomputed) = writes_of(|| recompute(db));
    eprintln!("recompute took {:?}", start.elapsed());
    Some([import?, recomputed?])
}

/// Runs the ignored test `name` of this module alone in a child test process with `env`
/// set, and fails when it fails.
fn run_alone(name: &str, env: &[(&str, &Path)]) {
    let module = module_path!().split_once("::").map_or("", |(_, m)| m);
    let mut cmd = std::process::Command::new(std::env::current_exe().expect("test binary"));
    cmd.arg(format!("{module}::{name}")).args([
        "--exact",
        "--ignored",
        "--nocapture",
        "--test-threads=1",
    ]);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    eprintln!("{text}");
    assert!(out.status.success() && text.contains("1 passed"), "{text}");
}

/// [`load_writes_alone`] in a process of its own: the soft heap limit and the heap it
/// bounds are process-wide, so other tests' connections would make the writer spill.
#[test]
fn a_dat_load_writes_each_dirty_page_about_once() {
    if thread_writes().is_none() {
        eprintln!("no per-thread I/O accounting; skipped");
        return;
    }
    run_alone("load_writes_alone", &[]);
}

/// [`stage_shrinks_alone`] in a process of its own whose temporary directory is its own.
#[cfg(target_os = "linux")]
#[test]
fn the_stage_gives_its_temporary_space_back() {
    let dir = tempfile::tempdir().expect("tempdir");
    run_alone(
        "stage_shrinks_alone",
        &[(crate::db::SQLITE_TMPDIR, dir.path())],
    );
}

/// Bytes of the files this process holds open under `dir`, deleted ones included, as
/// SQLite's temporary files are.
#[cfg(target_os = "linux")]
fn open_bytes_under(dir: &Path) -> u64 {
    let mut total = 0;
    for fd in std::fs::read_dir("/proc/self/fd").expect("fds").flatten() {
        let Ok(target) = std::fs::read_link(fd.path()) else {
            continue;
        };
        if target.starts_with(dir) {
            total += std::fs::metadata(fd.path()).map_or(0, |m| m.len());
        }
    }
    total
}

/// A staged game of `name` with three tracks carrying every hash.
fn staged_game(name: String, rng: &mut Rng) -> StagedGame {
    let roms = (1..=3)
        .map(|t| StagedRom {
            name: format!("{name} (Track {t}).bin"),
            size: 1_000_000,
            crc32: Some(rng.hex(8)),
            md5: Some(rng.hex(32)),
            sha1: Some(rng.hex(40)),
            status: "good".into(),
            header: None,
        })
        .collect();
    StagedGame {
        base_name: name.clone(),
        group_key: name.to_lowercase(),
        clone_of: None,
        regions: vec!["Europe".into()],
        languages: Vec::new(),
        revision: None,
        flags: Vec::new(),
        roms,
        name,
    }
}

/// Stages 20 000 games, empties the stage, then loads a DAT, and checks the temporary
/// files under `SQLITE_TMPDIR` hold the stage and shrink back once it is empty.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "run alone by the_stage_gives_its_temporary_space_back"]
fn stage_shrinks_alone() {
    let tmp = PathBuf::from(std::env::var_os(crate::db::SQLITE_TMPDIR).expect("SQLITE_TMPDIR"));
    let dir = tempfile::tempdir().expect("tempdir");
    let db = catalogue(dir.path(), 0.01, 0, &[]);
    db.write_blocking(|c| Ok(c.pragma_update(None, "temp_store", "FILE")?))
        .expect("temporary files");
    let mut rng = Rng(0x57a9_e000);
    let games: Vec<StagedGame> = crate::synth::game_names(20_000, 7)
        .into_iter()
        .map(|name| staged_game(name, &mut rng))
        .collect();
    let staged = db
        .write_blocking(|c| {
            for chunk in games.chunks(500) {
                append_chunk(c, chunk)?;
            }
            Ok(open_bytes_under(&tmp))
        })
        .expect("stage");
    db.write_blocking(|c| {
        let tx = c.transaction()?;
        dat_stage::clear(&tx)?;
        crate::db::commit(tx)
    })
    .expect("clear");
    let cleared = open_bytes_under(&tmp);
    let (xml, _) = psx_dat(2_000);
    let outcome = import_member(&db, Cursor::new(xml.as_bytes()), &request(), "").expect("load");
    assert!(matches!(outcome, Outcome::Loaded(_)), "{outcome:?}");
    let loaded = open_bytes_under(&tmp);
    eprintln!(
        "staged {staged} bytes, {cleared} once cleared, {loaded} after a {} byte load",
        xml.len()
    );
    assert!(staged > 4 << 20, "{staged} bytes staged");
    assert!(
        cleared < 256 << 10,
        "{cleared} bytes left once the stage emptied"
    );
    assert!(loaded < 256 << 10, "{loaded} bytes left after a load");
}

#[test]
fn a_full_disk_names_the_directory_with_less_room() {
    let (tmp, db) = (
        Path::new("/tmp/mistarr"),
        Path::new("/media/fat/mistarr/mistarr.db"),
    );
    let ram_full = |p: &Path| Some(if p == tmp { 0 } else { 1 << 30 });
    let card_full = |p: &Path| Some(if p == tmp { 1 << 30 } else { 0 });
    let msg = full_message(Some(tmp), db, ram_full);
    assert!(msg.starts_with("/tmp/mistarr is full"), "{msg}");
    let msg = full_message(Some(tmp), db, card_full);
    assert!(msg.starts_with("/media/fat/mistarr is full"), "{msg}");
    let msg = full_message(None, db, ram_full);
    assert!(msg.starts_with("/media/fat/mistarr is full"), "{msg}");
    let full =
        rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_FULL), None);
    assert!(disk_full(&Error::Db(full)));
    assert!(!disk_full(&Error::Cancelled));
}

/// Write syscalls of a 450-game psx load on a catalogue a tenth the bench size: about
/// 1 300, against 2 000 when the writer spills dirty pages before its commit and 1 700
/// with every rom index full. The recompute writes about 100.
#[test]
#[ignore = "run alone by a_dat_load_writes_each_dirty_page_about_once"]
fn load_writes_alone() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (xml, tracks) = psx_dat(450);
    let db = catalogue(dir.path(), 0.1, 600, &tracks);
    let [(import, _), (recompute, _)] = measure(&db, &xml).expect("per-thread I/O accounting");
    eprintln!("import {import} writes, recompute {recompute} writes");
    assert!(import < 1_500, "{import} write syscalls for the load");
    assert!(
        recompute < 200,
        "{recompute} write syscalls for the recompute"
    );
}

/// The measurement behind `docs/ARCHITECTURE.md` "Writes on a sync mount": the bench
/// catalogue, a 450-game psx load and 3 000 unmatched psx files.
#[test]
#[ignore = "about a minute; run by hand with --ignored --nocapture"]
fn sync_writes_on_the_bench_catalogue() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (xml, tracks) = psx_dat(450);
    eprintln!("DAT of {} bytes", xml.len());
    let db = catalogue(dir.path(), 1.0, 3_000, &tracks);
    let [(iw, ib), (rw, rb)] = measure(&db, &xml).expect("per-thread I/O accounting");
    eprintln!("import {iw} writes, {ib} bytes; recompute {rw} writes, {rb} bytes");
}

/// Loads `xml` from a file in `dir` through the import in RAM and returns its report.
fn measure_ram(db: &Db, dir: &std::path::Path, xml: &str) -> ram::Report {
    let path = dir.join("psx.dat");
    std::fs::write(&path, xml).expect("write");
    let plan = ram::Plan {
        dir: dir.join("ram"),
        floor: 0,
        job: 1,
    };
    let req = request();
    let out = db
        .hold_writer_blocking(|h| {
            ram::run(h, &plan, &mut (), |ram| {
                import_all(ram, &path, &[Member::Plain], &req, &mut |_, _, _| Ok(()))
            })
        })
        .expect("run");
    match out {
        Ram::Done(outcomes, report) => {
            assert!(matches!(outcomes[..], [Outcome::Loaded(_)]), "{outcomes:?}");
            report
        }
        Ram::Fallback(reason) => panic!("fell back: {reason}"),
    }
}

/// A 450-game psx load and its recompute on a tenth of the catalogue, through the copy
/// in RAM: the card sees one write per MiB of the database and a few more.
#[test]
fn a_load_in_ram_writes_the_card_about_once_per_mebibyte() {
    if thread_writes().is_none() {
        eprintln!("no per-thread I/O accounting; skipped");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let (xml, tracks) = psx_dat(450);
    let db = catalogue(dir.path(), 0.1, 600, &tracks);
    let report = measure_ram(&db, dir.path(), &xml);
    let writes = report.card_writes.expect("per-thread I/O accounting");
    let chunks = report.bytes.div_ceil(ram::CHUNK_BYTES as u64);
    eprintln!(
        "{} bytes, {chunks} chunks, {writes} card writes",
        report.bytes
    );
    assert!(writes <= chunks + 16, "{writes} writes for {chunks} MiB");
}

/// In place against in RAM, on the bench catalogue with 3 000 unmatched psx files, for a
/// 450-game and a 10 000-game psx DAT: card writes and host wall time of each.
#[test]
#[ignore = "minutes; run by hand with --ignored --nocapture --test-threads=1"]
fn in_place_and_in_ram_on_the_bench_catalogue() {
    for games in [450, 10_000] {
        let (xml, tracks) = psx_dat(games);
        let dir = tempfile::tempdir().expect("tempdir");
        let db = catalogue(dir.path(), 1.0, 3_000, &tracks);
        let size = std::fs::metadata(db.path()).expect("stat").len();
        let start = std::time::Instant::now();
        let [(iw, ib), (rw, rb)] = measure(&db, &xml).expect("per-thread I/O accounting");
        let in_place = start.elapsed();
        drop(db);

        let dir = tempfile::tempdir().expect("tempdir");
        let db = catalogue(dir.path(), 1.0, 3_000, &tracks);
        let start = std::time::Instant::now();
        let r = measure_ram(&db, dir.path(), &xml);
        let in_ram = start.elapsed();
        eprintln!(
            "{games} games, database {size} bytes before, {} after\n  \
             in place: {} writes ({} bytes), {in_place:?}\n  \
             in RAM: {:?} card writes, copy {:?}, import {:?}, write-back {:?}, {in_ram:?}",
            r.bytes,
            iw + rw,
            ib + rb,
            r.card_writes,
            r.copy_in,
            r.work,
            r.write_back,
        );
    }
}

/// Write syscalls of migration 17 on the bench catalogue with every rom keyed, the most
/// its partial indexes can hold, through [`Db::open`] as a start runs it.
#[test]
#[ignore = "about a minute; run by hand with --ignored --nocapture"]
fn migration_writes_on_the_bench_catalogue() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("sync.db");
    let db = Db::open(&path).expect("open");
    db.write_blocking(|c| {
        crate::db::platforms::seed(c, &mistarr_mister::platforms::PLATFORMS)?;
        crate::synth::seed(c, 1.0, 1)?;
        c.execute_batch(
            "UPDATE roms SET match_name = lower(name), match_base = lower(name);
             DROP INDEX roms_md5; CREATE INDEX roms_md5 ON roms(md5);
             DROP INDEX roms_match_base; CREATE INDEX roms_match_base ON roms(match_base, size);
             DROP INDEX roms_size; CREATE INDEX roms_size ON roms(size);
             CREATE TABLE dat_stage (seq INTEGER PRIMARY KEY, game TEXT NOT NULL);
             DELETE FROM schema_version WHERE version = 17;",
        )?;
        c.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
        Ok(())
    })
    .expect("catalogue at 16");
    drop(db);
    let copy = dir.path().join("copy.db");
    std::fs::copy(&path, &copy).expect("copy");
    let size = std::fs::metadata(&path).expect("size").len();
    let start = std::time::Instant::now();
    let steps = crate::migrating::Steps::default();
    let (db, writes) = writes_of(|| Db::open_counting(&path, &steps).expect("migrate"));
    eprintln!("steps {}", steps.load(std::sync::atomic::Ordering::Relaxed));
    let (w, b) = writes.expect("per-thread I/O accounting");
    eprintln!(
        "database {size} bytes: migration {w} writes, {b} bytes, {:?}",
        start.elapsed()
    );
    let v = db
        .read_blocking(crate::db::migrate::current_version)
        .expect("version");
    assert_eq!(v, crate::db::migrate::latest());
    let plan = ram::Plan {
        dir: dir.path().join("ram"),
        floor: 0,
        job: 0,
    };
    let start = std::time::Instant::now();
    let r = ram::migrate_in_ram(&copy, &plan, None)
        .expect("migrate")
        .expect("in RAM");
    eprintln!(
        "in RAM: {:?} card writes of {} bytes, {:?}",
        r.card_writes,
        r.bytes,
        start.elapsed()
    );
}
