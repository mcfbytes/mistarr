//! Write syscalls of a DAT load and its recompute; see `docs/ARCHITECTURE.md` "Writes on a sync mount".

use std::fmt::Write as _;
use std::io::Cursor;

use super::*;
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
fn catalogue(dir: &std::path::Path, scale: f64, unmatched: usize, dat: &[Track]) -> Db {
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

/// [`load_writes_alone`] in a process of its own: the soft heap limit and the heap it
/// bounds are process-wide, so other tests' connections would make the writer spill.
#[test]
fn a_dat_load_writes_each_dirty_page_about_once() {
    if thread_writes().is_none() {
        eprintln!("no per-thread I/O accounting; skipped");
        return;
    }
    let module = module_path!().split_once("::").map_or("", |(_, m)| m);
    let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .arg(format!("{module}::load_writes_alone"))
        .args(["--exact", "--ignored", "--nocapture", "--test-threads=1"])
        .output()
        .expect("run");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    eprintln!("{text}");
    assert!(out.status.success() && text.contains("1 passed"), "{text}");
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
