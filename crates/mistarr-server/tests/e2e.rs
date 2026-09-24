//! `docs/TESTING.md` layer 2: the synthetic set seeded by a real torrent
//! client through a local tracker, fetched and placed by the real app.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::Ordering::SeqCst;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{config_in, get, request, Sse};
use mistarr_clients::{
    ClientError, ClientTorrentId, DownloadClient, InfoHash, Rtorrent, SeedPolicy, TorrentSource,
    TorrentState, Transmission,
};
use mistarr_fixture::set::{self, Layout};
use mistarr_fixture::torrent;
use mistarr_fixture::tracker::Tracker;
use mistarr_server::app::{self, Options, Running};
use mistarr_server::config::{ClientChoice, Config};
use serde_json::Value;

/// Comma list of daemon binaries whose absence fails the test instead of skipping it.
const REQUIRE_ENV: &str = "MISTARR_E2E_REQUIRE";
/// Directory that receives daemon and server logs; a temporary one otherwise.
const LOGS_ENV: &str = "MISTARR_E2E_LOGS";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Transmission,
    Rtorrent,
}

impl Kind {
    fn binary(self) -> &'static str {
        match self {
            Kind::Transmission => "transmission-daemon",
            Kind::Rtorrent => "rtorrent",
        }
    }

    fn choice(self) -> ClientChoice {
        match self {
            Kind::Transmission => ClientChoice::Transmission,
            Kind::Rtorrent => ClientChoice::Rtorrent,
        }
    }
}

fn on_path(binary: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(binary).is_file()))
}

/// True when the test should run; says why it is skipped otherwise.
fn available(kind: Kind) -> bool {
    if on_path(kind.binary()) {
        return true;
    }
    let required = std::env::var(REQUIRE_ENV).unwrap_or_default();
    assert!(
        !required.split(',').any(|r| r.trim() == kind.binary()),
        "{} is not on PATH and {REQUIRE_ENV} requires it",
        kind.binary()
    );
    let bin = kind.binary();
    eprintln!(
        "skipping the {bin} end-to-end test: {bin} is not installed (apt-get install -y {bin})"
    );
    false
}

/// A port that is free now, below Linux's ephemeral range so that no outgoing
/// connection takes it before the daemon binds it; never the same one twice.
fn free_port() -> u16 {
    static NEXT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);
    let base = 20_000 + u16::try_from(std::process::id() % 1000).expect("pid") * 10;
    let _ = NEXT.compare_exchange(0, base, SeqCst, SeqCst);
    loop {
        let port = NEXT.fetch_add(1, SeqCst);
        assert!(port < 32_000, "no free port below the ephemeral range");
        if std::net::TcpListener::bind(("0.0.0.0", port)).is_ok() {
            return port;
        }
    }
}

/// A torrent client process with its own config, session and ports.
struct Daemon {
    kind: Kind,
    child: Child,
    url: String,
}

impl Daemon {
    fn spawn(kind: Kind, home: &Path, logs: &Path, role: &str) -> Self {
        let downloads = home.join("downloads");
        std::fs::create_dir_all(&downloads).expect("mkdir");
        let log =
            std::fs::File::create(logs.join(format!("{}-{role}.log", kind.binary()))).expect("log");
        let err = log.try_clone().expect("log");
        let peer = free_port();
        let (mut cmd, url) = match kind {
            Kind::Transmission => {
                let rpc = free_port();
                let settings = serde_json::json!({
                    "rpc-enabled": true,
                    "rpc-bind-address": "127.0.0.1",
                    "rpc-port": rpc,
                    "rpc-authentication-required": false,
                    "rpc-whitelist-enabled": false,
                    "rpc-host-whitelist-enabled": false,
                    "peer-port": peer,
                    "peer-port-random-on-start": false,
                    "port-forwarding-enabled": false,
                    "dht-enabled": false,
                    "lpd-enabled": false,
                    "pex-enabled": false,
                    "utp-enabled": false,
                    "download-dir": downloads,
                    "incomplete-dir-enabled": false,
                    "rename-partial-files": false,
                    "start-added-torrents": false,
                    "download-queue-enabled": false,
                    "seed-queue-enabled": false,
                    "queue-stalled-enabled": false,
                    "ratio-limit-enabled": false,
                    "idle-seeding-limit-enabled": false,
                    "speed-limit-down-enabled": false,
                    "speed-limit-up-enabled": false,
                    "alt-speed-enabled": false,
                    "blocklist-enabled": false,
                    "watch-dir-enabled": false,
                    "cache-size-mb": 4,
                });
                let config = home.join("config");
                std::fs::create_dir_all(&config).expect("mkdir");
                std::fs::write(config.join("settings.json"), settings.to_string()).expect("write");
                let mut cmd = Command::new(kind.binary());
                cmd.arg("-f").arg("-g").arg(&config).arg("--log-level=info");
                (cmd, format!("http://127.0.0.1:{rpc}/transmission/rpc"))
            }
            Kind::Rtorrent => {
                let scgi = free_port();
                let session = home.join("session");
                std::fs::create_dir_all(&session).expect("mkdir");
                let rc = format!(
                    "directory.default.set = {dl}\n\
                     session.path.set = {session}\n\
                     network.scgi.open_port = 127.0.0.1:{scgi}\n\
                     network.port_range.set = {peer}-{peer}\n\
                     network.port_random.set = no\n\
                     dht.mode.set = disable\n\
                     protocol.pex.set = no\n\
                     trackers.use_udp.set = no\n\
                     network.xmlrpc.size_limit.set = 8M\n\
                     pieces.hash.on_completion.set = yes\n\
                     system.daemon.set = true\n\
                     log.open_file = \"main\", {log}\n\
                     log.add_output = \"info\", \"main\"\n",
                    dl = downloads.display(),
                    session = session.display(),
                    log = logs.join(format!("rtorrent-{role}.internal.log")).display(),
                );
                let rc_path = home.join("rtorrent.rc");
                std::fs::write(&rc_path, rc).expect("write rc");
                let mut cmd = Command::new(kind.binary());
                cmd.arg("-n")
                    .arg("-o")
                    .arg(format!("import={}", rc_path.display()));
                (cmd, format!("127.0.0.1:{scgi}"))
            }
        };
        let child = cmd
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(err)
            .spawn()
            .expect("spawn daemon");
        Self { kind, child, url }
    }

    fn client(&self) -> Arc<dyn DownloadClient> {
        match self.kind {
            Kind::Transmission => Arc::new(Transmission::new(&self.url).expect("url")),
            Kind::Rtorrent => Arc::new(Rtorrent::new(&self.url).expect("addr")),
        }
    }

    async fn ready(&mut self) -> Arc<dyn DownloadClient> {
        let client = self.client();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if client.probe().await.is_ok() {
                return client;
            }
            if let Ok(Some(status)) = self.child.try_wait() {
                panic!("{} exited during start: {status}", self.kind.binary());
            }
            assert!(
                Instant::now() < deadline,
                "{} never answered",
                self.kind.binary()
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Polls `f` every 200 ms until it returns true, failing after `limit`.
async fn wait_for<F, Fut>(what: &str, limit: Duration, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + limit;
    while !f().await {
        assert!(
            Instant::now() < deadline,
            "timed out after {limit:?} waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// What a timed-out wait prints: the app's rows and the downloading client's view.
struct Probe {
    addr: std::net::SocketAddr,
    client: Arc<dyn DownloadClient>,
    torrents: Vec<ClientTorrentId>,
}

impl Probe {
    async fn report(&self) -> String {
        let mut out = String::new();
        for path in ["downloads", "sources", "system/jobs", "imports"] {
            let body = get(self.addr, &format!("/api/v1/{path}")).await.body;
            out.push_str(&format!("{path}: {body}\n"));
        }
        for id in &self.torrents {
            out.push_str(&format!(
                "client {id}: {:?}\n",
                self.client.status(id).await
            ));
        }
        out
    }

    async fn wait<F, Fut>(&self, what: &str, limit: Duration, mut f: F)
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        let deadline = Instant::now() + limit;
        while !f().await {
            if Instant::now() >= deadline {
                panic!(
                    "timed out after {limit:?} waiting for {what}\n{}",
                    self.report().await
                );
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

fn options(dir: &Path) -> Options {
    Options {
        corename_path: dir.join("CORENAME"),
        corename_poll: Duration::from_millis(200),
        status_interval: Duration::from_secs(3600),
        sources_poll: Duration::from_millis(200),
        sources_min_age_secs: 0,
        magnet_poll: Duration::from_secs(1),
        magnet_started_poll: Duration::from_millis(500),
        dats_poll: Duration::from_millis(200),
        dats_min_age: Duration::ZERO,
        poll_active: Duration::from_millis(500),
        poll_idle: Duration::from_secs(1),
        poll_backoff: Duration::from_secs(5),
    }
}

async fn start(config: &Config, dir: &Path) -> Running {
    app::start(config.clone(), options(dir))
        .await
        .expect("start")
}

fn hex(hash: [u8; 20]) -> String {
    InfoHash::from_bytes(hash).to_string()
}

fn infohash(metainfo: &[u8]) -> [u8; 20] {
    mistarr_sources::torrent::parse_torrent(metainfo)
        .expect("parse")
        .infohash
}

/// Adds `metainfo` to the seeder with every file wanted and waits until it seeds.
async fn seed(client: &dyn DownloadClient, metainfo: &[u8], dir: &Path, tracker: &Tracker) {
    let count = mistarr_sources::torrent::parse_torrent(metainfo)
        .expect("parse")
        .files
        .len();
    let all: Vec<u32> = (0..u32::try_from(count).expect("count")).collect();
    let src = TorrentSource::Metainfo(metainfo.to_vec());
    let id = client
        .add(src, dir, &all, SeedPolicy::Client)
        .await
        .expect("seeder add");
    client.start(&id).await.expect("seeder start");
    wait_for(
        "the seeder to verify its data",
        Duration::from_secs(60),
        || async {
            let st = client.status(&id).await.expect("seeder status");
            st.state == TorrentState::Seeding && st.files.iter().all(|f| f.is_complete())
        },
    )
    .await;
    let hash = infohash(metainfo);
    wait_for("the seeder's announce", Duration::from_secs(60), || async {
        tracker.peers(&hash).iter().any(|(_, seeding)| *seeding)
    })
    .await;
}

async fn json(addr: std::net::SocketAddr, path: &str) -> Value {
    let r = get(addr, path).await;
    assert_eq!(r.status, 200, "{path}: {}", r.body);
    r.json()
}

async fn items(addr: std::net::SocketAddr, path: &str) -> Vec<Value> {
    json(addr, path).await["items"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Moves `from` into `dir` the way a user drops a file: copy beside, then rename.
fn drop_into(from: &Path, dir: &Path) {
    let name = from.file_name().expect("name");
    let tmp = dir.parent().expect("parent").join(name);
    std::fs::copy(from, &tmp).expect("copy");
    std::fs::rename(&tmp, dir.join(name)).expect("rename");
}

/// The single browse item matching `q` on `platform`.
async fn title(addr: std::net::SocketAddr, platform: &str, q: &str) -> Value {
    let q = q.replace(' ', "%20");
    let found = items(addr, &format!("/api/v1/platforms/{platform}/titles?q={q}")).await;
    assert_eq!(found.len(), 1, "{q}: {found:?}");
    found[0].clone()
}

/// `(id, state)` of every download, sorted by id.
async fn download_states(addr: std::net::SocketAddr) -> Vec<(i64, String)> {
    let mut out: Vec<_> = items(addr, "/api/v1/downloads")
        .await
        .iter()
        .map(|d| {
            (
                d["id"].as_i64().expect("id"),
                d["state"].as_str().expect("state").to_owned(),
            )
        })
        .collect();
    out.sort();
    out
}

/// Every platform's `(id, have, unverified)` where it has titles.
async fn counts(addr: std::net::SocketAddr) -> Vec<(String, i64, i64)> {
    items(addr, "/api/v1/platforms")
        .await
        .iter()
        .filter(|p| p["counts"]["titles"].as_i64() > Some(0))
        .map(|p| {
            (
                p["id"].as_str().expect("id").to_owned(),
                p["counts"]["have"].as_i64().expect("have"),
                p["counts"]["unverified"].as_i64().expect("unverified"),
            )
        })
        .collect()
}

async fn roms_verified(addr: std::net::SocketAddr, title_id: i64) -> bool {
    let detail = json(addr, &format!("/api/v1/titles/{title_id}")).await;
    let pick = detail["pick_variant_id"].clone();
    detail["variants"]
        .as_array()
        .expect("variants")
        .iter()
        .filter(|v| v["id"] == pick)
        .flat_map(|v| v["roms"].as_array().cloned().unwrap_or_default())
        .all(|r| r["file_state"] == "verified")
}

struct Timings {
    started: Instant,
    marks: Vec<(&'static str, Duration)>,
}

impl Timings {
    fn mark(&mut self, what: &'static str) {
        self.marks.push((what, self.started.elapsed()));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn synthetic_set_through_transmission() {
    if available(Kind::Transmission) {
        run(Kind::Transmission).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn synthetic_set_through_rtorrent() {
    if available(Kind::Rtorrent) {
        run(Kind::Rtorrent).await;
    }
}

#[allow(clippy::too_many_lines)] // One user journey, read top to bottom.
async fn run(kind: Kind) {
    let mut t = Timings {
        started: Instant::now(),
        marks: Vec::new(),
    };
    let work = tempfile::tempdir().expect("tempdir");
    let logs = std::env::var_os(LOGS_ENV).map_or_else(|| work.path().join("logs"), PathBuf::from);
    let logs = logs.join(kind.binary());
    std::fs::create_dir_all(&logs).expect("logs");
    let _ = mistarr_server::logging::init(Some(&logs.join("mistarr.log")));

    let layout: Layout = set::generate(&work.path().join("fixture")).expect("set");
    let lan =
        mistarr_fixture::tracker::local_ipv4().expect("a non-loopback IPv4 address for peers");
    let tracker = Tracker::start("127.0.0.1:0", Some(lan)).expect("tracker");
    let announce = tracker.announce_url();
    let cart = torrent::build(&layout.cart_dir, &announce, None).expect("cart torrent");
    let disc = torrent::build(&layout.disc_dir, &announce, None).expect("disc torrent");
    let torrents = work.path().join("torrents");
    std::fs::create_dir_all(&torrents).expect("mkdir");
    std::fs::write(torrents.join("test-console.torrent"), &cart).expect("write");
    std::fs::write(torrents.join("test-disc.torrent"), &disc).expect("write");
    t.mark("set generated");

    let mut seeder = Daemon::spawn(kind, &work.path().join("seeder"), &logs, "seeder");
    let seeding = seeder.ready().await;
    let roms = layout.cart_dir.parent().expect("roms dir");
    seed(seeding.as_ref(), &cart, roms, &tracker).await;
    seed(seeding.as_ref(), &disc, roms, &tracker).await;
    t.mark("seeder seeding");

    let mut leecher = Daemon::spawn(kind, &work.path().join("leecher"), &logs, "leecher");
    let fetching = leecher.ready().await;
    let home = work.path().join("home");
    std::fs::create_dir_all(&home).expect("mkdir");
    let mut config = config_in(&home);
    config.client.kind = kind.choice();
    config.client.url.clone_from(&leecher.url);
    // Slow enough that no wanted file completes while the three wants arrive.
    config.limits.down_kbps_core = 1;
    let running = start(&config, &home).await;
    let addr = running.addr;
    let paths = running.app.config().paths;
    let probe = Probe {
        addr,
        client: Arc::clone(&fetching),
        torrents: [&cart, &disc]
            .iter()
            .map(|m| ClientTorrentId::new(hex(infohash(m))))
            .collect(),
    };
    probe
        .wait("the client detected", Duration::from_secs(20), || async {
            json(addr, "/api/v1/system/status").await["client"]["reachable"] == true
        })
        .await;
    let mut sse = Sse::open(addr, "/api/v1/events", &[]).await;
    sse.until("event: status").await;

    drop_into(&layout.cart_dat, &paths.dats());
    drop_into(&layout.disc_dat, &paths.dats());
    sse.until_count("event: dat.loaded", 2, Duration::from_secs(30))
        .await;
    let dats = items(addr, "/api/v1/dats").await;
    let mut bound: Vec<_> = dats
        .iter()
        .map(|d| d["platform_id"].as_str().unwrap_or("unbound").to_owned())
        .collect();
    bound.sort();
    assert_eq!(bound, [set::CART_PLATFORM, set::DISC_PLATFORM]);
    t.mark("DATs loaded");

    drop_into(&torrents.join("test-console.torrent"), &paths.sources());
    drop_into(&torrents.join("test-disc.torrent"), &paths.sources());
    sse.until("event: source.changed").await;
    let rejected = paths.sources().join("rejected");
    probe
        .wait("both sources bound", Duration::from_secs(30), || async {
            let reasons: Vec<_> = std::fs::read_dir(&rejected)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| std::fs::read_to_string(e.path()).unwrap_or_default())
                .collect();
            assert!(reasons.is_empty(), "rejected: {reasons:?}");
            let sources = items(addr, "/api/v1/sources").await;
            sources.len() == 2 && sources.iter().all(|s| s["state"] == "bound")
        })
        .await;
    let sources = items(addr, "/api/v1/sources").await;
    for s in &sources {
        assert_eq!(s["state"], "bound", "{s}");
        assert_eq!(s["seed_policy"], "none", "{s}");
    }
    t.mark("sources bound");

    // The user wants titles while a core runs, so every want lands before a
    // file completes and seed policy `none` stops the torrent in between.
    std::fs::write(home.join("CORENAME"), "NES").expect("corename");
    probe
        .wait("the core gate", Duration::from_secs(10), || async {
            json(addr, "/api/v1/system/status").await["pause_reason"] == "core"
        })
        .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let hidden = items(addr, "/api/v1/platforms/nes/titles?q=Fixture%20System").await;
    assert!(hidden.is_empty(), "the BIOS entry is hidden: {hidden:?}");
    let quest = title(addr, set::CART_PLATFORM, "Example Quest").await;
    assert_eq!(quest["pick_name"], set::CLONE_PICK);
    let broken = title(addr, set::CART_PLATFORM, "Broken Tale").await;
    let disc_title = title(addr, set::DISC_PLATFORM, "Disc Example").await;
    let wanted: Vec<i64> = [&quest, &broken, &disc_title]
        .iter()
        .map(|v| v["parent_id"].as_i64().expect("parent id"))
        .collect();
    for id in &wanted {
        let r = request(
            addr,
            "POST",
            &format!("/api/v1/titles/{id}/want"),
            &[],
            Some("{}"),
        )
        .await;
        assert_eq!(r.status, 200, "want {id}: {}", r.body);
    }
    let downloads = items(addr, "/api/v1/downloads").await;
    assert_eq!(
        downloads.len(),
        5,
        "one cart, one bad dump, three disc files"
    );
    std::fs::write(home.join("CORENAME"), "MENU").expect("corename");
    t.mark("titles wanted");

    probe
        .wait(
            "every download to settle",
            Duration::from_secs(180),
            || async {
                download_states(addr)
                    .await
                    .iter()
                    .all(|(_, s)| s == "done" || s == "bad" || s == "failed")
            },
        )
        .await;
    let settled = items(addr, "/api/v1/downloads").await;
    for d in &settled {
        let want = if d["title_name"] == set::BAD_DUMP {
            "bad"
        } else {
            "done"
        };
        assert_eq!(d["state"], want, "{d}");
    }
    sse.until_count(r#""state":"importing""#, 1, Duration::from_secs(10))
        .await;
    sse.until_count("event: import.done", 4, Duration::from_secs(10))
        .await;
    t.mark("imports done");

    let pick = layout
        .entries
        .iter()
        .find(|e| e.name == set::CLONE_PICK)
        .expect("pick");
    let placed = std::fs::read(paths.games.join(format!("NES/{}.nes", set::CLONE_PICK)))
        .expect("the pick is placed under its canonical name");
    assert_eq!(
        placed,
        [&set::ines_header(pick.size)[..], &set::rom_bytes(pick)].concat()
    );
    let nes: Vec<_> = std::fs::read_dir(paths.games.join("NES"))
        .expect("games/NES")
        .map(|e| e.expect("entry").file_name())
        .collect();
    assert_eq!(nes, [format!("{}.nes", set::CLONE_PICK).as_str()]);
    let disc_dir = paths.games.join("PSX").join(set::DISC_GAME);
    for rom in [
        "Disc Example (USA).cue",
        "Disc Example (USA) (Track 1).bin",
        "Disc Example (USA) (Track 2).bin",
    ] {
        let staged = std::fs::read(layout.disc_dir.join(set::DISC_GAME).join(rom)).expect("source");
        assert_eq!(
            std::fs::read(disc_dir.join(rom)).expect(rom),
            staged,
            "{rom}"
        );
    }
    let bad_name = format!("{}.nes", set::BAD_DUMP);
    assert!(!paths.games.join("NES").join(&bad_name).exists());
    let quarantine = paths
        .staging()
        .join("quarantine")
        .join(hex(infohash(&cart)));
    assert!(quarantine.join(&bad_name).is_file(), "quarantined file");
    let report =
        std::fs::read_to_string(quarantine.join(format!("{bad_name}.report.txt"))).expect("report");
    assert!(report.contains("Expected:"), "{report}");
    let log = items(addr, "/api/v1/imports").await;
    let actions = |a: &str| log.iter().filter(|e| e["action"] == a).count();
    assert_eq!(
        (actions("placed"), actions("quarantined")),
        (4, 1),
        "{log:?}"
    );

    probe
        .wait(
            "both torrents removed from the client",
            Duration::from_secs(60),
            || async {
                items(addr, "/api/v1/sources")
                    .await
                    .iter()
                    .all(|s| s["client_id"].is_null())
            },
        )
        .await;
    for meta in [&cart, &disc] {
        let id = ClientTorrentId::new(hex(infohash(meta)));
        let gone = fetching.status(&id).await;
        assert!(matches!(gone, Err(ClientError::NotFound)), "{gone:?}");
    }
    t.mark("torrents removed");

    let r = request(addr, "POST", "/api/v1/system/scan", &[], Some("{}")).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(r.json()["job_id"].is_i64(), "{}", r.body);
    probe
        .wait("the scan to finish", Duration::from_secs(60), || async {
            !items(addr, "/api/v1/system/jobs")
                .await
                .iter()
                .any(|j| j["kind"] == "scan")
        })
        .await;
    let expected = vec![
        (set::CART_PLATFORM.to_owned(), 1, 0),
        (set::DISC_PLATFORM.to_owned(), 1, 0),
    ];
    assert_eq!(counts(addr).await, expected);
    assert!(roms_verified(addr, wanted[0]).await);
    assert!(roms_verified(addr, wanted[2]).await);
    t.mark("scan verified");

    let before = (
        download_states(addr).await,
        items(addr, "/api/v1/imports").await.len(),
        items(addr, "/api/v1/sources").await,
    );
    drop(sse);
    running.shutdown().await.expect("shutdown");
    let running = start(&config, &home).await;
    let addr = running.addr;
    let after = (
        download_states(addr).await,
        items(addr, "/api/v1/imports").await.len(),
        items(addr, "/api/v1/sources").await,
    );
    assert_eq!(before, after);
    assert_eq!(counts(addr).await, expected);
    assert!(roms_verified(addr, wanted[0]).await);
    let wizard = json(addr, "/api/v1/system/wizard").await;
    assert_eq!(
        (wizard["dats"].clone(), wizard["sources"].clone()),
        (Value::Bool(true), Value::Bool(true))
    );
    running.shutdown().await.expect("shutdown");
    t.mark("restart kept state");

    drop(leecher);
    drop(seeder);
    let report: Vec<String> = t
        .marks
        .iter()
        .map(|(what, at)| format!("{what} at {:.1}s", at.as_secs_f64()))
        .collect();
    eprintln!("e2e against {}: {}", kind.binary(), report.join(", "));
}
