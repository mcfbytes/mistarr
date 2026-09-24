//! Peak RSS of the `mistarr` binary, one process per job, on large synthetic inputs;
//! see `docs/TESTING.md` "Memory budget".

use std::fmt::Write as _;
use std::io::{BufWriter, Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use mistarr_core::hash::Md5Stream;
use mistarr_sources::bencode::{self, Value};
use serde_json::Value as Json;

/// Peak RSS budget during scan or import, `docs/ARCHITECTURE.md` "Resource budgets".
const BUDGET_KIB: u64 = 64 * 1024;

/// Longest a job may take before the test gives up.
const JOB_TIMEOUT: Duration = Duration::from_secs(600);

const MRAS: usize = 1000;
/// Member size and `repeat` of the one MRA part whose rom is larger than the budget.
const BIG_PART_BYTES: usize = 32 * 1024 * 1024;
const BIG_REPEAT: usize = 4;
const ALTERNATIVES: usize = 50;
const ORGANIZED_DIRS: usize = 1000;
const LINKS_PER_DIR: usize = 15;
const DAT_BYTES: usize = 50 * 1024 * 1024;
const EXPORT_BYTES: usize = 16 * 1024 * 1024;
/// MRAs of inline part data, each about 3 MB of hex, and the bytes each decodes to.
const LARGE_MRAS: usize = 16;
const INLINE_BYTES: usize = 1024 * 1024;
const TORRENT_FILES: usize = 50_000;
const LOOSE_FILES: usize = 16_000;
const ZIPPED_FILES: usize = 2_000;
const DISC_DIRS: usize = 1_000;

/// A running `mistarr serve` over one data directory.
struct Server {
    child: Child,
    port: u16,
    db: PathBuf,
}

/// Writes the config and starts `mistarr serve` on `root` until one listens, since another
/// process may take the chosen free port first. Returns the listening server.
fn spawn(root: &Path, extra: &str) -> Server {
    for _ in 0..5 {
        let port = free_port();
        let data = root.join("data");
        std::fs::create_dir_all(&data).expect("mkdir data");
        let config = format!(
            "[server]\nlisten = \"127.0.0.1:{port}\"\n[paths]\nroot = {root:?}\ngames = {games:?}\n\
             [client]\nkind = \"transmission\"\nurl = \"http://127.0.0.1:1/transmission/rpc\"\n\
             [jobs]\nscan_interval_minutes = 0\n{extra}",
            games = root.join("games"),
        );
        std::fs::write(data.join("mistarr.toml"), config).expect("config");
        let child = Command::new(env!("CARGO_BIN_EXE_mistarr"))
            .arg("--data")
            .arg(&data)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn mistarr");
        let mut server = Server {
            child,
            port,
            db: data.join("mistarr.db"),
        };
        if server.wait_listening() {
            return server;
        }
    }
    panic!("server never listened");
}

/// The data segment limit `mistarr` inherits from this process, `None` when unlimited.
fn inherited_data_limit() -> Option<u64> {
    let limits = std::fs::read_to_string("/proc/self/limits").expect("proc limits");
    data_limit_of(&limits)
}

/// The soft `Max data size` of a `/proc/<pid>/limits` text, `None` when unlimited.
fn data_limit_of(limits: &str) -> Option<u64> {
    let line = limits
        .lines()
        .find(|l| l.starts_with("Max data size"))
        .expect("data limit line");
    line.split_whitespace().nth(3).expect("soft").parse().ok()
}

impl Server {
    fn start(root: &Path) -> Self {
        let server = spawn(root, "");
        server.assert_data_limit(192);
        assert!(root.join("data/tmp").is_dir(), "SQLite temporary directory");
        server
    }

    /// `[memory] data_limit_mib` of `mib` is in force, or the lower limit the test inherited.
    fn assert_data_limit(&self, mib: u64) {
        let limits = std::fs::read_to_string(format!("/proc/{}/limits", self.child.id()))
            .expect("proc limits");
        let want = inherited_data_limit().map_or(mib << 20, |l| l.min(mib << 20));
        assert_eq!(data_limit_of(&limits), Some(want), "{limits}");
    }

    /// Waits until the server accepts connections; false when it exited first.
    fn wait_listening(&mut self) -> bool {
        let start = Instant::now();
        while TcpStream::connect(("127.0.0.1", self.port)).is_err() {
            if self.child.try_wait().ok().flatten().is_some() {
                return false;
            }
            assert!(
                start.elapsed() < Duration::from_secs(60),
                "server never listened"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        true
    }

    /// `VmHWM` of the server process in KiB.
    fn peak_kib(&self) -> u64 {
        let status = std::fs::read_to_string(format!("/proc/{}/status", self.child.id()))
            .expect("proc status");
        status
            .lines()
            .find_map(|l| l.strip_prefix("VmHWM:"))
            .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
            .expect("VmHWM")
    }

    fn post(&self, path: &str, body: &str) {
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).expect("connect");
        write!(
            s,
            "POST /api/v1{path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\
             X-Mistarr: 1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len(),
            port = self.port,
        )
        .expect("write");
        let mut reply = String::new();
        s.read_to_string(&mut reply).expect("read");
        assert!(reply.starts_with("HTTP/1.1 200"), "{path}: {reply}");
    }

    /// Waits until `count` jobs of `kind` finished and nothing is queued or running,
    /// then returns the finished rows' `(state, progress)` in id order.
    fn wait_jobs(&self, kind: &str, count: usize) -> Vec<(String, Json)> {
        let start = Instant::now();
        loop {
            if let Some(rows) = self.finished(kind, count) {
                return rows;
            }
            assert!(start.elapsed() < JOB_TIMEOUT, "{kind} did not finish");
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    fn finished(&self, kind: &str, count: usize) -> Option<Vec<(String, Json)>> {
        let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY;
        let conn = rusqlite::Connection::open_with_flags(&self.db, flags).ok()?;
        let open: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE state IN ('queued', 'running', 'paused')",
                [],
                |r| r.get(0),
            )
            .ok()?;
        let mut stmt = conn
            .prepare(
                "SELECT state, COALESCE(progress, 'null') FROM jobs
                 WHERE kind = ?1 AND state IN ('done', 'failed') ORDER BY id",
            )
            .ok()?;
        let rows: Vec<(String, Json)> = stmt
            .query_map([kind], |r| {
                let progress: String = r.get(1)?;
                Ok((
                    r.get(0)?,
                    serde_json::from_str(&progress).unwrap_or(Json::Null),
                ))
            })
            .ok()?
            .collect::<rusqlite::Result<_>>()
            .ok()?;
        (open == 0 && rows.len() >= count).then_some(rows)
    }

    fn count(&self, sql: &str) -> i64 {
        let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY;
        let conn = rusqlite::Connection::open_with_flags(&self.db, flags).expect("open db");
        conn.query_row(sql, [], |r| r.get(0)).expect("count")
    }

    /// Stops the server and returns its peak RSS in KiB.
    fn stop(mut self, label: &str) -> u64 {
        let peak = self.peak_kib();
        let _ = self.child.kill();
        let _ = self.child.wait();
        println!(
            "peak RSS {label}: {} KiB ({:.1} MiB)",
            peak,
            kib_to_mib(peak)
        );
        peak
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[allow(clippy::cast_precision_loss)]
fn kib_to_mib(kib: u64) -> f64 {
    kib as f64 / 1024.0
}

fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    l.local_addr().expect("addr").port()
}

/// Peak RSS of a server that runs no job, measured once per test process.
fn idle_kib() -> u64 {
    static IDLE: OnceLock<u64> = OnceLock::new();
    *IDLE.get_or_init(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let server = spawn(dir.path(), "");
        std::thread::sleep(Duration::from_secs(2));
        server.stop("idle")
    })
}

/// Holds `peak_kib` under the budget, and its growth over an idle server under `delta_mib`.
fn assert_budget(label: &str, peak_kib: u64, delta_mib: u64) {
    assert!(
        peak_kib < BUDGET_KIB,
        "{label}: peak RSS {peak_kib} KiB is over the {BUDGET_KIB} KiB budget"
    );
    let idle = idle_kib();
    let grown = peak_kib.saturating_sub(idle);
    assert!(
        grown < delta_mib << 10,
        "{label}: peak RSS {peak_kib} KiB is {grown} KiB over idle {idle} KiB, above {delta_mib} MiB"
    );
}

fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, bytes).expect("write");
}

/// Deterministic bytes for entry `i`.
fn bytes_for(i: usize, len: usize) -> Vec<u8> {
    let mut x = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x.to_le_bytes()[0]
        })
        .collect()
}

fn hex_of(i: usize, digits: usize) -> String {
    let mut out = String::with_capacity(digits);
    let mut x = (i as u64)
        .wrapping_add(1)
        .wrapping_mul(0x2545_F491_4F6C_DD1D);
    while out.len() < digits {
        let _ = write!(out, "{:016x}", x);
        x = x.rotate_left(17).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
    out.truncate(digits);
    out
}

fn zip_of(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, body) in members {
        z.start_file(*name, zip::write::SimpleFileOptions::default())
            .expect("start");
        z.write_all(body).expect("write");
    }
    z.finish().expect("finish").into_inner()
}

/// Members of each synthetic MRA zip, one `<part>` each, as in a typical MRA.
const PARTS: usize = 24;

/// One MRA naming `zip` whose `<rom>` md5 covers every part; every tenth mixes tag case.
fn mra_text(name: &str, zip: &str, md5: &str, i: usize) -> String {
    let mut parts = String::new();
    for p in 0..PARTS {
        let _ = writeln!(
            parts,
            "    <part name=\"p{p:02}.bin\" crc=\"{}\"/>",
            hex_of(p + i, 8)
        );
    }
    let text = format!(
        "<?xml version=\"1.0\"?>\n<misterromdescription>\n  <name>{name}</name>\n  \
         <setname>ex{i:04}</setname>\n  <rbf>excore</rbf>\n  <mameversion>0245</mameversion>\n  \
         <rom index=\"0\" zip=\"{zip}\" md5=\"{md5}\" type=\"merged\">\n{parts}  </rom>\n\
         <rom index=\"1\"><part>00 01 02 03</part></rom>\n</misterromdescription>\n"
    );
    if i % 10 == 0 {
        text.replace("<name>", "<Name>")
            .replace("<setname>", "<SetName>")
            .replace("<rom index=\"0\"", "<ROM Index=\"0\"")
            .replace("<part name", "<Part NAME")
    } else {
        text
    }
}

/// `_Arcade` with `MRAS` MRAs, `ALTERNATIVES` more under `_alternatives`, hard links,
/// cores, one MRA repeating a 32 MiB part, and an `_Organized` tree of symlinks back to
/// them; plus a zip per MRA.
/// Returns the number of distinct MRA files.
fn arcade_tree(root: &Path) -> usize {
    let arcade = root.join("_Arcade");
    let mame = root.join("games/mame");
    std::fs::create_dir_all(&mame).expect("mkdir");
    for i in 0..MRAS + ALTERNATIVES {
        let bodies: Vec<(String, Vec<u8>)> = (0..PARTS)
            .map(|p| (format!("p{p:02}.bin"), bytes_for(i * PARTS + p, 256)))
            .collect();
        let mut md5 = Md5Stream::new();
        for (_, body) in &bodies {
            md5.update(body);
        }
        let members: Vec<(&str, &[u8])> = bodies
            .iter()
            .map(|(n, b)| (n.as_str(), b.as_slice()))
            .collect();
        let zip = format!("exg{i:04}.zip");
        write(&mame.join(&zip), &zip_of(&members));
        let (name, path) = if i < MRAS {
            let name = format!("Example Game {i:04}");
            (name.clone(), arcade.join(format!("{name}.mra")))
        } else {
            let name = format!("Example Game {:04} (alt {i})", i - MRAS);
            let dir = arcade.join(format!("_alternatives/_Example Game {:04}", i - MRAS));
            (name.clone(), dir.join(format!("{name}.mra")))
        };
        write(&path, mra_text(&name, &zip, &md5.finish(), i).as_bytes());
    }
    let big = bytes_for(0, 1024).repeat(BIG_PART_BYTES / 1024);
    let mut md5 = Md5Stream::new();
    for _ in 0..BIG_REPEAT {
        md5.update(&big);
    }
    write(&mame.join("exbig.zip"), &zip_of(&[("big.bin", &big)]));
    let big_mra = format!(
        "<misterromdescription><name>Example Big Repeat</name><rbf>excore</rbf>\
         <rom index=\"0\" zip=\"exbig.zip\" md5=\"{}\"><part name=\"big.bin\" repeat=\"{BIG_REPEAT}\"/>\
         </rom></misterromdescription>",
        md5.finish()
    );
    write(&arcade.join("Example Big Repeat.mra"), big_mra.as_bytes());
    for i in 0..5 {
        let src = arcade.join(format!("Example Game {i:04}.mra"));
        std::fs::hard_link(&src, arcade.join(format!("Example Game {i:04} link.mra")))
            .expect("hard link");
    }
    for i in 0..3 {
        write(&arcade.join(format!("cores/excore{i}_20240101.rbf")), b"");
    }
    for d in 0..ORGANIZED_DIRS {
        let dir = arcade.join(format!("_Organized/_Group {:02}/_Sub {d:04}", d % 40));
        std::fs::create_dir_all(&dir).expect("mkdir");
        for k in 0..LINKS_PER_DIR {
            let target = arcade.join(format!("Example Game {:04}.mra", (d * 7 + k) % MRAS));
            std::os::unix::fs::symlink(&target, dir.join(format!("Example Game {k:02}.mra")))
                .expect("symlink");
        }
    }
    MRAS + ALTERNATIVES + 1
}

/// A Logiqx DAT of about `DAT_BYTES` binding to NES, clone groups of three regions.
/// Returns the number of games; game `i` is named by [`game_name`].
fn big_dat(path: &Path) -> usize {
    let nes = mistarr_fixture::dat::platform("nes").expect("nes");
    let header = mistarr_fixture::dat::header_name(nes, "Memory Test");
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    let mut out = BufWriter::new(std::fs::File::create(path).expect("create"));
    writeln!(
        out,
        "<?xml version=\"1.0\"?>\n<datafile>\n<header>\n<name>{header}</name>\n\
         <description>{header}</description>\n<version>1</version>\n</header>"
    )
    .expect("write");
    let mut written = 0;
    let mut games = 0;
    let regions = ["USA", "Europe", "Japan"];
    while written < DAT_BYTES {
        let name = game_name(games, &regions);
        let game = format!(
            "<game name=\"{name}\">\n<description>{name}</description>\n\
             <rom name=\"{name}.nes\" size=\"{size}\" crc=\"{crc}\" md5=\"{md5}\" sha1=\"{sha1}\"/>\n</game>\n",
            size = rom_size(games),
            crc = hex_of(games, 8),
            md5 = hex_of(games + 7, 32),
            sha1 = hex_of(games + 13, 40),
        );
        written += game.len();
        out.write_all(game.as_bytes()).expect("write");
        games += 1;
    }
    out.write_all(b"</datafile>\n").expect("write");
    out.flush().expect("flush");
    games
}

/// A zipped No-Intro DB export of about `EXPORT_BYTES` of XML binding to NES: each game
/// has two sources repeating a headered and a headerless file, and clone groups of three
/// reference their parent's archive number. Returns the number of games.
fn big_export(path: &Path) -> usize {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    let file = std::fs::File::create(path).expect("create");
    let mut zip = zip::ZipWriter::new(BufWriter::new(file));
    let member = "Example Vendor - Nintendo Entertainment System (DB Export) (20260101-000000).xml";
    zip.start_file(member, zip::write::SimpleFileOptions::default())
        .expect("start");
    zip.write_all(
        b"<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<header>\n\t<version>20260101-000000</version>\n\
          \t<author>tester</author>\n</header>\n<datafile>\n",
    )
    .expect("write");
    let regions = ["USA", "Europe", "Japan"];
    let (mut written, mut games) = (0, 0);
    while written < EXPORT_BYTES {
        let name = game_name(games, &regions);
        let parent = games - games % 3;
        let clone = if parent == games {
            "P".to_owned()
        } else {
            format!("{:06}", parent + 1)
        };
        let size = rom_size(games);
        let mut sources = String::new();
        for s in 0..2 {
            let _ = write!(
                sources,
                "\t\t<source>\n\t\t\t<details id=\"{s}\" section=\"Trusted Dump\" region=\"{r}\"/>\n\
                 \t\t\t<file id=\"{s}1\" extension=\"nes\" size=\"{h}\" crc32=\"{}\" md5=\"{}\" sha1=\"{}\" \
                 header=\"4E 45 53 1A 02 01 00 00 00 00 00 00 00 00 00 00\" format=\"Headered\"/>\n\
                 \t\t\t<file id=\"{s}2\" extension=\"unh\" size=\"{size}\" crc32=\"{}\" md5=\"{}\" sha1=\"{}\" \
                 format=\"Headerless\"/>\n\t\t</source>\n",
                hex_of(games + 3, 8),
                hex_of(games + 5, 32),
                hex_of(games + 9, 40),
                hex_of(games, 8),
                hex_of(games + 7, 32),
                hex_of(games + 13, 40),
                r = regions[games % 3],
                h = size + 16,
            );
        }
        let game = format!(
            "\t<game name=\"{name}\">\n\t\t<archive number=\"{:06}\" clone=\"{clone}\" name=\"{name}\" \
             region=\"{r}\" languages=\"En\"/>\n{sources}\t</game>\n",
            games + 1,
            r = regions[games % 3],
        );
        written += game.len();
        zip.write_all(game.as_bytes()).expect("write");
        games += 1;
    }
    zip.write_all(b"</datafile>\n").expect("write");
    zip.finish().expect("finish");
    games
}

fn game_name(i: usize, regions: &[&str]) -> String {
    format!("Example Game {:06} ({})", i / 3, regions[i % 3])
}

fn rom_size(i: usize) -> u64 {
    16_384 + (i as u64 % 64) * 1024
}

/// A multi-file torrent of `TORRENT_FILES` files named after the DAT's roms.
fn big_torrent(path: &Path) {
    let regions = ["USA", "Europe", "Japan"];
    let files: Vec<Value> = (0..TORRENT_FILES)
        .map(|i| {
            let mut f = std::collections::BTreeMap::new();
            f.insert(b"length".to_vec(), Value::Int(rom_size(i) as i64));
            let name = format!("{}.nes", game_name(i, &regions));
            f.insert(
                b"path".to_vec(),
                Value::List(vec![
                    Value::Bytes(format!("Group {:03}", i / 1000).into_bytes()),
                    Value::Bytes(name.into_bytes()),
                ]),
            );
            Value::Dict(f)
        })
        .collect();
    let total: u64 = (0..TORRENT_FILES).map(rom_size).sum();
    let piece = 1u64 << 20;
    let pieces = usize::try_from(total.div_ceil(piece)).expect("pieces");
    let mut info = std::collections::BTreeMap::new();
    info.insert(b"name".to_vec(), Value::Bytes(b"Example Set".to_vec()));
    info.insert(b"piece length".to_vec(), Value::Int(piece as i64));
    info.insert(b"pieces".to_vec(), Value::Bytes(bytes_for(7, pieces * 20)));
    info.insert(b"files".to_vec(), Value::List(files));
    let mut top = std::collections::BTreeMap::new();
    top.insert(b"info".to_vec(), Value::Dict(info));
    write(path, &bencode::encode(&Value::Dict(top)));
}

/// Loose, zipped and disc files under `games/`: returns the number of files.
fn games_tree(root: &Path) -> usize {
    let gba = root.join("games/GBA");
    for i in 0..LOOSE_FILES {
        write(
            &gba.join(format!("Example Game {i:05} (USA).gba")),
            &bytes_for(i, 1024),
        );
    }
    for i in 0..ZIPPED_FILES {
        let name = format!("Example Zipped {i:05} (USA)");
        let body = bytes_for(i + LOOSE_FILES, 1024);
        write(
            &gba.join(format!("{name}.zip")),
            &zip_of(&[(&format!("{name}.gba"), &body)]),
        );
    }
    let psx = root.join("games/PSX");
    for i in 0..DISC_DIRS {
        let dir = psx.join(format!("Example Disc {i:04} (USA)"));
        let bin = format!("Example Disc {i:04} (USA).bin");
        write(&dir.join(&bin), &bytes_for(i + 50_000, 2352));
        write(
            &dir.join(format!("Example Disc {i:04} (USA).cue")),
            format!("FILE \"{bin}\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n")
                .as_bytes(),
        );
    }
    LOOSE_FILES + ZIPPED_FILES + DISC_DIRS * 2
}

#[test]
fn arcade_catalogue_stays_under_budget() {
    let dir = tempfile::tempdir().expect("tempdir");
    let distinct = arcade_tree(dir.path());

    let server = Server::start(dir.path());
    let rows = server.wait_jobs("arcade_catalog", 1);
    let titles = server.count("SELECT COUNT(*) FROM titles WHERE source = 'mra' AND retired = 0");
    let matched =
        server.count("SELECT COUNT(*) FROM titles WHERE source = 'mra' AND mra_check = 'match'");
    let peak = server.stop("arcade_catalog, first run");
    let (state, progress) = &rows[0];
    println!("progress: {progress}");
    assert_eq!(state, "done", "{progress}");
    assert_eq!(usize::try_from(titles).expect("count"), distinct);
    assert_eq!(usize::try_from(matched).expect("count"), distinct);
    assert_eq!(progress["parsed"], distinct, "each distinct MRA read once");
    assert_budget("arcade_catalog", peak, 12);

    let server = Server::start(dir.path());
    let rows = server.wait_jobs("arcade_catalog", 2);
    let peak = server.stop("arcade_catalog, unchanged rerun");
    let (_, progress) = &rows[1];
    println!("progress: {progress}");
    assert_eq!(progress["parsed"], 0, "unchanged MRAs are not read again");
    assert_eq!(
        progress["checked"], 0,
        "unchanged sets are not checked again"
    );
    assert_budget("arcade_catalog rerun", peak, 12);
}

/// `_Arcade` with `LARGE_MRAS` MRAs of about 3 MB each, one `<part>` of inline hex beside a
/// zipped part under one md5, all in one catalogue batch. Returns the number of MRAs.
fn large_mra_tree(root: &Path) -> usize {
    let arcade = root.join("_Arcade");
    let mame = root.join("games/mame");
    std::fs::create_dir_all(&mame).expect("mkdir");
    for i in 0..LARGE_MRAS {
        let inline = bytes_for(i + 90_000, INLINE_BYTES);
        let zipped = bytes_for(i + 95_000, 4096);
        let zip = format!("exinl{i:02}.zip");
        write(&mame.join(&zip), &zip_of(&[("z.bin", &zipped)]));
        let mut md5 = Md5Stream::new();
        md5.update(&inline);
        md5.update(&zipped);
        let mut hex = String::with_capacity(INLINE_BYTES * 3);
        for (k, b) in inline.iter().enumerate() {
            let _ = write!(hex, "{b:02X}{}", if k % 32 == 31 { '\n' } else { ' ' });
        }
        let name = format!("Example Inline {i:02}");
        let text = format!(
            "<misterromdescription><name>{name}</name><rbf>excore</rbf>\n\
             <rom index=\"0\" zip=\"{zip}\" md5=\"{}\"><part>\n{hex}</part><part name=\"z.bin\"/></rom>\n\
             </misterromdescription>\n",
            md5.finish()
        );
        write(&arcade.join(format!("{name}.mra")), text.as_bytes());
    }
    LARGE_MRAS
}

#[test]
fn large_inline_mras_stay_under_budget() {
    let dir = tempfile::tempdir().expect("tempdir");
    let count = large_mra_tree(dir.path());

    let server = Server::start(dir.path());
    let rows = server.wait_jobs("arcade_catalog", 1);
    let matched =
        server.count("SELECT COUNT(*) FROM titles WHERE source = 'mra' AND mra_check = 'match'");
    let peak = server.stop("arcade_catalog, large inline MRAs");
    let (state, progress) = &rows[0];
    println!("progress: {progress}");
    assert_eq!(state, "done", "{progress}");
    assert_eq!(usize::try_from(matched).expect("count"), count);
    assert_eq!(progress["parsed"], count, "each MRA read once in the batch");
    assert_budget("arcade_catalog, large inline MRAs", peak, 12);
}

#[test]
fn dat_and_torrent_import_stay_under_budget() {
    let dir = tempfile::tempdir().expect("tempdir");
    let games = big_dat(&dir.path().join("data/dats/memory.dat"));
    println!("DAT of {games} games");

    let server = Server::start(dir.path());
    let rows = server.wait_jobs("dat_import", 1);
    let titles = server.count("SELECT COUNT(*) FROM titles WHERE retired = 0");
    let peak = server.stop("dat_import");
    assert_eq!(rows[0].0, "done", "{}", rows[0].1);
    assert_eq!(usize::try_from(titles).expect("count"), games);
    assert_budget("dat_import", peak, 12);

    big_torrent(&dir.path().join("data/sources/example.torrent"));
    let server = Server::start(dir.path());
    let rows = server.wait_jobs("source_import", 1);
    let files = server.count("SELECT COUNT(*) FROM torrent_files");
    let matched = server.count("SELECT COUNT(*) FROM torrent_files WHERE rom_id IS NOT NULL");
    let peak = server.stop("source_import");
    assert_eq!(rows[0].0, "done", "{}", rows[0].1);
    assert_eq!(usize::try_from(files).expect("count"), TORRENT_FILES);
    assert_eq!(usize::try_from(matched).expect("count"), TORRENT_FILES);
    assert_budget("source_import", peak, 16);
}

#[test]
fn db_export_import_stays_under_budget() {
    let dir = tempfile::tempdir().expect("tempdir");
    let games = big_export(
        &dir.path()
            .join("data/dats/Example Vendor - NES (DB Export) (20260101-000000).zip"),
    );
    println!("DB export of {games} games");

    let server = Server::start(dir.path());
    let rows = server.wait_jobs("dat_import", 1);
    let titles = server.count("SELECT COUNT(*) FROM titles WHERE retired = 0");
    let headerless = server.count(
        "SELECT COUNT(*) FROM roms WHERE name LIKE '%.nes' AND header IS NOT NULL AND size < 100000",
    );
    let clones = server.count("SELECT COUNT(*) FROM titles WHERE parent_id <> id");
    let peak = server.stop("dat_import, DB export");
    assert_eq!(rows[0].0, "done", "{}", rows[0].1);
    assert_eq!(usize::try_from(titles).expect("count"), games);
    assert_eq!(usize::try_from(headerless).expect("count"), games);
    assert_eq!(
        usize::try_from(clones).expect("count"),
        games - games.div_ceil(3)
    );
    assert_budget("dat_import, DB export", peak, 12);
}

#[test]
fn scan_stays_under_budget() {
    let dir = tempfile::tempdir().expect("tempdir");
    let files = games_tree(dir.path());

    let server = Server::start(dir.path());
    server.post("/system/scan", "{\"platform_id\":\"gba\"}");
    server.wait_jobs("scan", 1);
    server.post("/system/scan", "{\"platform_id\":\"psx\"}");
    let rows = server.wait_jobs("scan", 2);
    let stored = server.count("SELECT COUNT(*) FROM files");
    let peak = server.stop("scan");
    assert!(rows.iter().all(|(s, _)| s == "done"), "{rows:?}");
    assert_eq!(usize::try_from(stored).expect("count"), files);
    assert_budget("scan", peak, 16);
}

#[test]
fn a_tiny_memory_limit_is_raised_to_the_floor() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = spawn(dir.path(), "[memory]\ndata_limit_mib = 2\n");
    server.assert_data_limit(64);
}
