//! `POST /fetch`: one URL fetched once into `dats/` or `sources/`; `docs/API.md` "Fetching a URL".

mod common;

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use common::{boot, boot_with_options, config_in, options_in, request, Booted};
use mistarr_clients::fake::{FileRoute, FileServer};
use serde_json::{json, Value};

const DAT: &str = r#"<?xml version="1.0"?>
<!-- carried-comment -->
<datafile><header><name>Nintendo - Nintendo Entertainment System</name><version>20240101</version></header>
<carried-element><![CDATA[carried-cdata]]></carried-element>
<game name="Example Quest (World)"><rom name="Example Quest (World).nes" size="4" crc="0a0b0c0d"/></game>
</datafile>"#;

const NOT_ACCEPTED: &str = "This isn't a DAT, DAT pack or torrent file.";

/// Info-level log lines of every test in this binary.
fn logs() -> Arc<Mutex<Vec<u8>>> {
    static LOGS: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();
    Arc::clone(LOGS.get_or_init(|| {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&buf);
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .with_writer(move || Sink(Arc::clone(&sink)))
            .finish();
        tracing::subscriber::set_global_default(subscriber).expect("one subscriber");
        buf
    }))
}

struct Sink(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Sink {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("logs").extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
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

/// A synthetic torrent of one small file, listing `web_seed` when given.
fn torrent(web_seed: Option<&str>) -> Vec<u8> {
    let dir = tempfile::tempdir().expect("tempdir");
    let set = dir.path().join("Example Set");
    std::fs::create_dir_all(&set).expect("mkdir");
    std::fs::write(set.join("Example Quest (World).nes"), b"NES\x1adata").expect("write");
    mistarr_fixture::torrent::build(&set, "http://tracker.invalid/announce", web_seed)
        .expect("torrent")
}

fn data(b: &Booted) -> PathBuf {
    b.dir.path().join("data")
}

/// Posts `url` to `/fetch` and returns the status and body.
async fn fetch(b: &Booted, url: &str) -> (u16, Value) {
    let body = json!({ "url": url }).to_string();
    let r = request(b.addr(), "POST", "/api/v1/fetch", &[], Some(&body)).await;
    let v = if r.body.is_empty() {
        Value::Null
    } else {
        r.json()
    };
    (r.status, v)
}

/// Waits for `url_fetch` job `id` to finish and returns its state and progress.
async fn finished(b: &Booted, id: &Value) -> (String, Value) {
    for _ in 0..500 {
        let r = request(b.addr(), "GET", "/api/v1/system/jobs/recent", &[], None).await;
        let items = r.json()["items"].as_array().cloned().unwrap_or_default();
        if let Some(job) = items.iter().find(|j| &j["id"] == id) {
            return (
                job["state"].as_str().unwrap_or("").to_owned(),
                job["progress"].clone(),
            );
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("fetch {id} did not finish");
}

/// Fetches `url` and waits for it: its state and progress.
async fn fetched(b: &Booted, url: &str) -> (String, Value) {
    let (status, started) = fetch(b, url).await;
    assert_eq!(status, 202, "{started}");
    assert!(started["token"].is_u64(), "{started}");
    finished(b, &started["job_id"]).await
}

fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|d| {
            d.flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// Every byte of the database and its WAL.
fn db_bytes(b: &Booted) -> Vec<u8> {
    let mut all = Vec::new();
    for name in ["mistarr.db", "mistarr.db-wal"] {
        if let Ok(bytes) = std::fs::read(data(b).join(name)) {
            all.extend(bytes);
        }
    }
    all
}

fn contains(hay: &[u8], needle: &str) -> bool {
    hay.windows(needle.len()).any(|w| w == needle.as_bytes())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dat_is_fetched_once_placed_and_never_stored() {
    let logs = logs();
    let b = boot().await;
    let server = FileServer::start().await.expect("bind");
    let path = "/private-7f3a/Example.dat?token=q9v8";
    server.route(path, FileRoute::ok(DAT.as_bytes().to_vec()));
    let url = server.url(path);
    let (state, progress) = fetched(&b, &url).await;
    assert_eq!(state, "done", "{progress}");
    assert_eq!(progress["target"], "dats");
    assert_eq!(progress["file"], "Example.dat");
    assert_eq!(progress["bytes_received"], DAT.len());
    assert_eq!(progress["placed"]["file"], "Example.dat");
    assert_eq!(server.hits(), [path]);
    let dats = data(&b).join("dats");
    common::eventually("the DAT to load", || async {
        names_in(&dats.join("loaded")) == ["Example.dat"]
    })
    .await;
    assert!(names_in(&data(&b).join("tmp")).is_empty());
    let placed = std::fs::read(dats.join("loaded").join("Example.dat")).expect("placed");
    assert!(
        !contains(&placed, "carried"),
        "only mistarr's rewrite is placed"
    );
    assert!(contains(&placed, "Example Quest (World).nes"));
    let stored = db_bytes(&b);
    for needle in [
        url.as_str(),
        "private-7f3a",
        "q9v8",
        &server.addr().to_string(),
    ] {
        assert!(!contains(&stored, needle), "{needle} is in the database");
    }
    let logged = logs.lock().expect("logs").clone();
    assert!(!logged.is_empty(), "info logs are captured");
    for needle in [url.as_str(), "private-7f3a", &server.addr().to_string()] {
        assert!(!contains(&logged, needle), "{needle} is in the info log");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dat_pack_is_placed_and_a_mixed_zip_refused() {
    let b = boot().await;
    let server = FileServer::start().await.expect("bind");
    let pack = zip_of(&[("a.dat", DAT.as_bytes()), ("b.xml", DAT.as_bytes())]);
    server.route(
        "/get",
        FileRoute::ok(pack).with_header("Content-Disposition", "attachment; filename=\"Pack.zip\""),
    );
    let mixed = zip_of(&[("a.dat", DAT.as_bytes()), ("Example Quest.nes", b"NES\x1a")]);
    server.route("/mixed.zip", FileRoute::ok(mixed));
    let (state, progress) = fetched(&b, &server.url("/get")).await;
    assert_eq!(
        (state.as_str(), &progress["file"]),
        ("done", &json!("Pack.zip"))
    );
    let (state, progress) = fetched(&b, &server.url("/mixed.zip")).await;
    assert_eq!(state, "failed");
    assert_eq!(progress["error"], "This zip holds files other than DATs.");
    let dats = data(&b).join("dats");
    assert!(!names_in(&dats).contains(&"mixed.zip".to_owned()));
    assert!(!names_in(&dats.join("loaded")).contains(&"mixed.zip".to_owned()));
    assert!(names_in(&data(&b).join("tmp")).is_empty());

    let hidden = b"NES bytes no member lists";
    let mut padded = zip_of(&[("c.dat", DAT.as_bytes())]);
    padded.extend_from_slice(hidden);
    server.route("/Padded.zip", FileRoute::ok(padded));
    let (state, progress) = fetched(&b, &server.url("/Padded.zip")).await;
    assert_eq!(state, "done", "{progress}");
    let placed = [
        dats.join("Padded.zip"),
        dats.join("loaded").join("Padded.zip"),
    ]
    .into_iter()
    .find_map(|p| std::fs::read(p).ok())
    .expect("the placed pack");
    assert!(!placed.windows(hidden.len()).any(|w| w == hidden));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_torrent_goes_to_sources_and_its_web_seeds_are_never_fetched() {
    let b = boot().await;
    let server = FileServer::start().await.expect("bind");
    let seeded = torrent(Some(&server.url("/seed/")));
    server.route("/t/set.torrent", FileRoute::ok(seeded));
    let (state, progress) = fetched(&b, &server.url("/t/set.torrent")).await;
    assert_eq!(state, "done", "{progress}");
    assert_eq!(
        (&progress["target"], &progress["file"]),
        (&json!("sources"), &json!("set.torrent"))
    );
    common::eventually("the source to load", || async {
        let r = request(b.addr(), "GET", "/api/v1/sources", &[], None).await;
        r.json()["total"] == 1
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        server.hits(),
        ["/t/set.torrent"],
        "fetched once, web seeds untouched"
    );
    let (state, progress) = fetched(&b, &server.url("/t/set.torrent")).await;
    assert_eq!(state, "failed");
    assert_eq!(
        progress["error"],
        "A source with the same content is already loaded."
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pages_binaries_and_oversized_bodies_are_refused() {
    let b = boot().await;
    let server = FileServer::start().await.expect("bind");
    server.route(
        "/page",
        FileRoute::ok(b"<!DOCTYPE html><html><body>hi</body></html>".to_vec()),
    );
    let noise: Vec<u8> = (0..4096u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    server.route(
        "/game.nes",
        FileRoute::ok([&b"NES\x1a"[..], &noise].concat()),
    );
    let mut big = b"d8:announce".to_vec();
    big.resize(17 << 20, b'x');
    server.route("/big.torrent", FileRoute::ok(big).without_length());
    let mut claimed = FileRoute::ok(DAT.as_bytes().to_vec());
    claimed.length = Some(600 << 20);
    server.route("/claimed.dat", claimed);
    for path in ["/page", "/game.nes"] {
        let (state, progress) = fetched(&b, &server.url(path)).await;
        assert_eq!(
            (state.as_str(), &progress["error"]),
            ("failed", &json!(NOT_ACCEPTED)),
            "{path}"
        );
    }
    let (state, progress) = fetched(&b, &server.url("/big.torrent")).await;
    assert_eq!(state, "failed");
    assert_eq!(
        progress["error"],
        "The file is larger than 16 MiB, the most a torrent may be."
    );
    let (state, progress) = fetched(&b, &server.url("/claimed.dat")).await;
    assert_eq!(state, "failed");
    assert_eq!(
        progress["error"],
        "The file is larger than 512 MiB, the most a DAT or DAT pack may be."
    );
    assert!(names_in(&data(&b).join("dats")).is_empty());
    assert!(names_in(&data(&b).join("sources")).is_empty());
    assert!(names_in(&data(&b).join("tmp")).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn five_redirects_are_followed_and_six_refused() {
    let b = boot().await;
    let server = FileServer::start().await.expect("bind");
    for i in 0..6 {
        server.route(
            &format!("/r{i}"),
            FileRoute::redirect(302, &format!("/r{}", i + 1)),
        );
    }
    server.route("/r6", FileRoute::ok(DAT.as_bytes().to_vec()));
    let (state, progress) = fetched(&b, &server.url("/r1")).await;
    assert_eq!(
        (state.as_str(), &progress["file"]),
        ("done", &json!("r6.dat"))
    );
    let (state, progress) = fetched(&b, &server.url("/r0")).await;
    assert_eq!(state, "failed");
    assert_eq!(
        progress["error"],
        "The server redirected more than 5 times."
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_fetch_leaves_no_partial_file() {
    let b = boot().await;
    let server = FileServer::start().await.expect("bind");
    let slow = FileRoute::ok(DAT.repeat(200).into_bytes()).paced(512, Duration::from_millis(50));
    server.route("/slow.dat", slow);
    let (status, started) = fetch(&b, &server.url("/slow.dat")).await;
    assert_eq!(status, 202);
    let id = started["job_id"].clone();
    common::eventually("bytes to arrive", || async {
        let r = request(b.addr(), "GET", "/api/v1/system/jobs", &[], None).await;
        let items = r.json()["items"].as_array().cloned().unwrap_or_default();
        items.iter().any(|j| {
            j["id"] == id
                && j["lane"] == "fetch"
                && j["progress"]["bytes_received"].as_u64() > Some(0)
        })
    })
    .await;
    let token = started["token"].as_u64().expect("token");
    let r = request(
        b.addr(),
        "DELETE",
        &format!("/api/v1/fetch/{token}"),
        &[],
        None,
    )
    .await;
    assert_eq!(r.status, 204);
    let (state, progress) = finished(&b, &id).await;
    assert_eq!(
        (state.as_str(), &progress["error"]),
        ("failed", &json!("Cancelled."))
    );
    assert!(names_in(&data(&b).join("tmp")).is_empty());
    assert!(names_in(&data(&b).join("dats")).is_empty());
    let r = request(
        b.addr(),
        "DELETE",
        &format!("/api/v1/fetch/{token}"),
        &[],
        None,
    )
    .await;
    assert_eq!(r.status, 404);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn magnets_are_placed_at_once_and_bad_links_refused() {
    let b = boot().await;
    let magnet = "magnet:?xt=urn:btih:a94a8fe5ccb19ba61c4c0873d391e987982fbbd3&dn=Example+Set";
    let (status, placed) = fetch(&b, magnet).await;
    assert_eq!(status, 202, "{placed}");
    assert_eq!(placed["target"], "sources");
    assert_eq!(placed["file"]["file"], "Example Set.magnet");
    assert!(placed["token"].is_null());
    let sources = data(&b).join("sources");
    assert!(
        names_in(&sources).contains(&"Example Set.magnet".to_owned())
            || names_in(&sources.join("loaded")).contains(&"Example Set.magnet".to_owned())
    );
    for bad in [
        "magnet:?xt=urn:btih:nothex",
        "ftp://example.invalid/a.dat",
        "http://user:secret@example.invalid/a.dat",
        "example.invalid/a.dat",
        "",
    ] {
        let (status, body) = fetch(&b, bad).await;
        assert_eq!(status, 400, "{bad}: {body}");
        let message = body["error"]["message"].as_str().unwrap_or("");
        assert!(
            bad.is_empty() || !message.contains("example.invalid"),
            "{message}"
        );
        assert!(!message.contains("secret"), "{message}");
    }
}

/// A server config for a self-signed certificate of `127.0.0.1`, and that certificate as PEM.
fn self_signed() -> (rustls::ServerConfig, String) {
    let key = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_owned()]).expect("cert");
    let der = rustls::pki_types::PrivateKeyDer::Pkcs8(key.signing_key.serialize_der().into());
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("versions")
        .with_no_client_auth()
        .with_single_cert(vec![key.cert.der().clone()], der)
        .expect("server config");
    (config, key.cert.pem())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn https_uses_the_given_roots_and_refuses_a_downgrade() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (config, pem) = self_signed();
    let ca = dir.path().join("ca.pem");
    std::fs::write(&ca, pem).expect("write");
    let mut options = options_in(dir.path());
    options.ca_file = Some(ca);
    let cfg = config_in(dir.path());
    let b = boot_with_options(dir, cfg, options).await;
    let tls = FileServer::start_tls(config).await.expect("bind");
    let plain = FileServer::start().await.expect("bind");
    plain.route("/a.dat", FileRoute::ok(DAT.as_bytes().to_vec()));
    tls.route("/a.dat", FileRoute::ok(DAT.as_bytes().to_vec()));
    tls.route("/down", FileRoute::redirect(302, &plain.url("/a.dat")));
    plain.route("/up", FileRoute::redirect(301, &tls.url("/a.dat")));
    let (state, progress) = fetched(&b, &tls.url("/a.dat")).await;
    assert_eq!(state, "done", "{progress}");
    let (state, progress) = fetched(&b, &plain.url("/up")).await;
    assert_eq!(
        state, "done",
        "an http link may redirect to https: {progress}"
    );
    let (state, progress) = fetched(&b, &tls.url("/down")).await;
    assert_eq!(state, "failed");
    assert_eq!(
        progress["error"],
        "A redirect from https to http was refused."
    );
    assert_eq!(
        plain.hits(),
        vec!["/up".to_owned()],
        "the downgrade was never followed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn parts_a_restart_broke_off_are_swept_at_startup() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data = dir.path().join("data");
    let left = [
        data.join("dats").join(".upload-1-1.part"),
        data.join("sources").join(".upload-1-2.part"),
        data.join("tmp").join("fetch-1-3.part"),
    ];
    for p in &left {
        std::fs::create_dir_all(p.parent().expect("dir")).expect("mkdir");
        std::fs::write(p, b"partial").expect("write");
    }
    let cfg = config_in(dir.path());
    let options = options_in(dir.path());
    let _b = boot_with_options(dir, cfg, options).await;
    for p in &left {
        assert!(!p.exists(), "{} was swept", p.display());
    }
}
