//! Want, transfer and poll against both fake download clients.

mod common;

use std::time::Duration;

use common::{boot_with, config_in, eventually, get, request, Booted};
use mistarr_clients::fake::{FakeResponse, FakeScgiServer, FakeServer, ScgiReply};
use mistarr_clients::xmlrpc::Value as Xml;
use mistarr_clients::InfoHash;
use mistarr_server::config::ClientChoice;
use mistarr_server::db::downloads::{self as rows, DownloadState};
use mistarr_server::db::sources::fixtures::seed_rom;
use mistarr_server::events::{Event, EventKind};
use mistarr_server::jobs::poll::{Cadence, Poller};
use mistarr_server::jobs::{Job, JobContext, Lane, Scheduler};
use serde_json::{json, Value};
use tokio::sync::broadcast::Receiver;

fn bstr(s: &str) -> String {
    format!("{}:{s}", s.len())
}

/// A synthetic multi-file `.torrent` whose files sit under `NES/`.
fn torrent(name: &str, files: &[(&str, u64)]) -> Vec<u8> {
    let mut list = String::from("l");
    for (file, len) in files {
        list.push_str(&format!(
            "d6:lengthi{len}e4:pathl{}{}ee",
            bstr("NES"),
            bstr(file)
        ));
    }
    list.push('e');
    let info = format!(
        "d5:files{list}4:name{}12:piece lengthi16384e6:pieces0:e",
        bstr(name)
    );
    let announce = bstr("http://tracker.invalid/announce");
    format!("d8:announce{announce}4:info{info}e").into_bytes()
}

const SET: &str = "Synthetic Set";
const FILES: [(&str, u64); 4] = [
    ("Example Quest (USA).nes", 40_976),
    ("readme.txt", 120),
    ("Second Try (Japan).nes", 24_592),
    ("Third Tale (Europe).nes", 65_552),
];

fn set_bytes() -> Vec<u8> {
    torrent(SET, &FILES)
}

fn set_hash() -> String {
    let meta = mistarr_sources::torrent::parse_torrent(&set_bytes()).expect("parse");
    InfoHash::from_bytes(meta.infohash).to_string()
}

/// Seeds the three roms of [`FILES`] and returns their title ids by file index 0, 2, 3.
fn seed_catalog(b: &Booted) -> [i64; 3] {
    b.running
        .app
        .db
        .write_blocking(|c| {
            let mut out = [0; 3];
            for (slot, (name, size)) in [FILES[0], FILES[2], FILES[3]].into_iter().enumerate() {
                let rom = seed_rom(c, "nes", name, size, "[]")?;
                out[slot] = c.query_row("SELECT title_id FROM roms WHERE id = ?1", [rom], |r| {
                    r.get(0)
                })?;
            }
            Ok(out)
        })
        .expect("seed")
}

async fn drop_source(b: &Booted) {
    let dir = b.running.app.config().paths.sources();
    std::fs::write(dir.join("set.torrent"), set_bytes()).expect("write");
    eventually("a bound source", || async {
        let r = get(b.addr(), "/api/v1/sources").await.json();
        r["items"][0]["state"] == "bound"
    })
    .await;
    // The bind's `source.changed` queues a transfer; let it pass before scripting the client.
    let app = &b.running.app;
    eventually("the bind's transfer", || async {
        app.db
            .read(|c| mistarr_server::db::jobs::count_kind(c, "transfer"))
            .await
            .expect("count")
            > 0
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
}

async fn want(b: &Booted, title: i64) -> common::Response {
    let body = json!({ "variant_id": title }).to_string();
    let path = format!("/api/v1/titles/{title}/want");
    request(b.addr(), "POST", &path, &[], Some(&body)).await
}

async fn unwant(b: &Booted, title: i64) {
    let path = format!("/api/v1/titles/{title}/want");
    let r = request(b.addr(), "DELETE", &path, &[], None).await;
    assert_eq!(r.status, 200, "{}", r.body);
}

async fn downloads(b: &Booted, query: &str) -> Vec<Value> {
    let r = get(b.addr(), &format!("/api/v1/downloads{query}")).await;
    assert_eq!(r.status, 200, "{}", r.body);
    r.json()["items"].as_array().cloned().unwrap_or_default()
}

/// The newest download of `title`.
async fn download_of(b: &Booted, title: i64) -> Value {
    downloads(b, "")
        .await
        .into_iter()
        .filter(|d| d["title_id"] == title)
        .max_by_key(|d| d["id"].as_i64())
        .unwrap_or_else(|| panic!("no download for title {title}"))
}

async fn wait_state(b: &Booted, title: i64, state: &str) {
    eventually(&format!("title {title} {state}"), || async {
        download_of(b, title).await["state"] == state
    })
    .await;
}

/// `download.changed` bodies published since the last drain.
fn changes(rx: &mut Receiver<std::sync::Arc<Event>>) -> Vec<Value> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        if ev.kind == EventKind::DownloadChanged {
            out.push(serde_json::from_str(&ev.data).expect("json"));
        }
    }
    out
}

async fn boot_transmission(fake: &FakeServer) -> Booted {
    fake.push(FakeResponse::success(json!({ "version": "4.0.5" })));
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = config_in(dir.path());
    config.client.url = fake.url();
    boot_with(dir, config).await
}

/// Occupies the heavy lane so the importer leaves `importing` rows alone
/// while a test observes the handoff; notify the returned handle to release it.
struct Hold(std::sync::Arc<tokio::sync::Notify>);

#[async_trait::async_trait]
impl Job for Hold {
    fn kind(&self) -> &'static str {
        "hold"
    }

    fn lane(&self) -> Lane {
        Lane::Heavy
    }

    async fn run(&self, _ctx: &JobContext) -> mistarr_server::Result<()> {
        self.0.notified().await;
        Ok(())
    }
}

async fn hold_imports(b: &Booted) -> std::sync::Arc<tokio::sync::Notify> {
    let release = std::sync::Arc::new(tokio::sync::Notify::new());
    let job = std::sync::Arc::new(Hold(std::sync::Arc::clone(&release)));
    Scheduler::enqueue(&b.running.app, job).await.expect("hold");
    release
}

fn ok() -> FakeResponse {
    FakeResponse::success(json!({}))
}

fn exists() -> FakeResponse {
    FakeResponse::success(json!({ "torrents": [{ "id": 1 }] }))
}

/// The replies to a fresh add with seed policy `none`, then start.
fn push_add(fake: &FakeServer, hash: &str) {
    fake.push(FakeResponse::success(json!({
        "torrent-added": { "id": 1, "name": SET, "hashString": hash }
    })));
    fake.push(ok());
    fake.push(exists());
    fake.push(ok());
}

/// The replies to extending the selection of a known torrent, then start.
fn push_extend(fake: &FakeServer, wanted: [bool; 4]) {
    fake.push(FakeResponse::success(
        json!({ "torrents": [{ "wanted": wanted }] }),
    ));
    fake.push(ok());
    fake.push(exists());
    fake.push(ok());
}

/// A `torrent-get` status reply with `status` and bytes done per file.
fn status(hash: &str, code: i64, done: [u64; 4], error: i64) -> FakeResponse {
    // The readme (index 1) and the third rom are never selected in these tests.
    let wanted = |i: usize| i % 2 == 0 && i < 3;
    let stats: Vec<Value> = done
        .iter()
        .enumerate()
        .map(|(i, d)| json!({ "bytesCompleted": d, "wanted": wanted(i) }))
        .collect();
    let left: u64 = FILES
        .iter()
        .zip(done)
        .enumerate()
        .filter(|(i, _)| wanted(*i))
        .map(|(_, ((_, len), d))| len - d)
        .sum();
    FakeResponse::success(json!({ "torrents": [{
        "id": 1, "hashString": hash, "status": code, "leftUntilDone": left,
        "error": error, "errorString": if error == 3 { "No space left" } else { "" },
        "fileStats": stats, "rateDownload": 0, "rateUpload": 0,
        "uploadRatio": 0.0, "isFinished": false
    }] }))
}

fn methods(fake: &FakeServer) -> Vec<String> {
    fake.bodies()
        .iter()
        .map(|b| b["method"].as_str().unwrap_or("").to_owned())
        .collect()
}

#[tokio::test]
async fn want_adds_selects_starts_extends_and_polls_through_transmission() {
    let fake = FakeServer::start().await.expect("fake");
    let b = boot_transmission(&fake).await;
    let hold = hold_imports(&b).await;
    let [quest, second, _] = seed_catalog(&b);
    drop_source(&b).await;
    let h = set_hash();
    let app = &b.running.app;
    let mut rx = app.events.subscribe(None).live;

    push_add(&fake, &h);
    let r = want(&b, quest).await;
    assert_eq!(r.status, 200, "{}", r.body);
    wait_state(&b, quest, "transferring").await;
    let bodies = fake.bodies();
    assert_eq!(
        methods(&fake)[1..],
        ["torrent-add", "torrent-set", "torrent-get", "torrent-start"]
    );
    let add = &bodies[1]["arguments"];
    assert_eq!(add["paused"], true);
    assert!(add["metainfo"].is_string());
    let staging = app.config().paths.staging().join(&h);
    assert_eq!(add["download-dir"], staging.to_string_lossy().as_ref());
    assert_eq!(add["files-unwanted"], json!([1, 2, 3]));
    assert_eq!(bodies[2]["arguments"]["seedRatioMode"], 2);
    assert_eq!(bodies[4]["arguments"]["ids"], json!([h]));
    let d = download_of(&b, quest).await;
    assert_eq!(
        (d["file_index"].clone(), d["progress"].clone()),
        (json!(0), json!(0.0))
    );
    let seen = changes(&mut rx);
    let states: Vec<&Value> = seen.iter().map(|c| &c["state"]).collect();
    assert_eq!(states, [&json!("queued"), &json!("transferring")]);
    let source = app
        .db
        .read(|c| mistarr_server::db::sources::list(c, 1, 0))
        .await
        .expect("sources")
        .0;
    assert_eq!(source[0].client_id.as_deref(), Some(h.as_str()));

    push_extend(&fake, [true, false, false, false]);
    let r = want(&b, second).await;
    assert_eq!(r.status, 200, "{}", r.body);
    wait_state(&b, second, "transferring").await;
    let bodies = fake.bodies();
    assert_eq!(
        methods(&fake)[5..],
        ["torrent-get", "torrent-set", "torrent-get", "torrent-start"]
    );
    assert_eq!(bodies[6]["arguments"]["files-wanted"], json!([0, 2]));
    assert_eq!(bodies[6]["arguments"]["files-unwanted"], json!([1, 3]));
    changes(&mut rx);

    let mut poller = Poller::new();
    fake.push(exists());
    fake.push(ok());
    fake.push(status(&h, 4, [20_488, 0, 0, 0], 0));
    assert_eq!(poller.tick(app).await.expect("tick"), Cadence::Active);
    let seen = changes(&mut rx);
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0]["progress"], 0.5);
    assert_eq!(seen[0]["state"], "transferring");

    fake.push(status(&h, 4, [20_488, 0, 0, 0], 0));
    poller.tick(app).await.expect("tick");
    assert!(changes(&mut rx).is_empty(), "nothing moved");

    fake.push(status(&h, 2, [40_976, 0, 6_148, 0], 0));
    poller.tick(app).await.expect("tick");
    let seen = changes(&mut rx);
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert_eq!(download_of(&b, quest).await["state"], "checking");
    assert_eq!(download_of(&b, second).await["progress"], 0.25);

    fake.push(status(&h, 4, [40_976, 0, 6_148, 0], 0));
    poller.tick(app).await.expect("tick");
    let d = download_of(&b, quest).await;
    assert_eq!(d["state"], "importing");
    let staged = staging.join(SET).join("NES").join(FILES[0].0);
    assert_eq!(d["staged_path"], staged.to_string_lossy().as_ref());
    assert_eq!(changes(&mut rx).len(), 1);

    fake.push(status(&h, 6, [40_976, 0, 24_592, 0], 0));
    fake.push(exists());
    fake.push(ok());
    assert_eq!(poller.tick(app).await.expect("tick"), Cadence::Idle);
    assert_eq!(download_of(&b, second).await["state"], "importing");
    let tail: Vec<String> = methods(&fake).into_iter().rev().take(3).collect();
    assert_eq!(tail, ["torrent-stop", "torrent-get", "torrent-get"]);
    assert_eq!(poller.failures(), 0);
    hold.notify_one();
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn unwant_before_and_after_start() {
    let fake = FakeServer::start().await.expect("fake");
    let b = boot_transmission(&fake).await;
    let [quest, _, _] = seed_catalog(&b);
    drop_source(&b).await;
    let h = set_hash();

    fake.push(FakeResponse::failure("disk is read-only"));
    assert_eq!(want(&b, quest).await.status, 200);
    eventually("the refused add", || async { fake.bodies().len() == 2 }).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(download_of(&b, quest).await["state"], "queued");
    unwant(&b, quest).await;
    assert_eq!(download_of(&b, quest).await["state"], "cancelled");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        fake.bodies().len(),
        2,
        "no client call for an unstarted download"
    );

    push_add(&fake, &h);
    assert_eq!(want(&b, quest).await.status, 200);
    wait_state(&b, quest, "transferring").await;
    fake.push(exists());
    fake.push(ok());
    fake.push(FakeResponse::success(
        json!({ "torrents": [{ "wanted": [true, false, false, false] }] }),
    ));
    fake.push(ok());
    unwant(&b, quest).await;
    assert_eq!(download_of(&b, quest).await["state"], "cancelled");
    eventually("the deselect", || async { fake.bodies().len() == 10 }).await;
    let bodies = fake.bodies();
    assert_eq!(
        methods(&fake)[6..],
        ["torrent-get", "torrent-stop", "torrent-get", "torrent-set"]
    );
    assert_eq!(
        bodies[9]["arguments"]["files-unwanted"],
        json!([0, 1, 2, 3])
    );
    let detail = get(b.addr(), &format!("/api/v1/titles/{quest}"))
        .await
        .json();
    assert_eq!(detail["variants"][0]["wanted"], false);
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn failed_downloads_retry_and_the_api_guards_states() {
    let fake = FakeServer::start().await.expect("fake");
    let b = boot_transmission(&fake).await;
    let [quest, _, _] = seed_catalog(&b);
    drop_source(&b).await;
    let h = set_hash();
    let app = &b.running.app;

    push_add(&fake, &h);
    assert_eq!(want(&b, quest).await.status, 200);
    wait_state(&b, quest, "transferring").await;
    let id = download_of(&b, quest).await["id"].clone();
    let retry = format!("/api/v1/downloads/{id}/retry");
    let r = request(b.addr(), "POST", &retry, &[], None).await;
    assert_eq!(r.status, 409, "{}", r.body);

    let mut poller = Poller::new();
    fake.push(exists());
    fake.push(ok());
    fake.push(status(&h, 0, [10, 0, 0, 0], 3));
    poller.tick(app).await.expect("tick");
    let d = download_of(&b, quest).await;
    assert_eq!(d["state"], "failed");
    assert!(d["error"]
        .as_str()
        .is_some_and(|e| e.contains("No space left")));
    assert_eq!(downloads(&b, "?state=failed").await.len(), 1);
    assert!(downloads(&b, "?state=queued,transferring").await.is_empty());
    let r = get(b.addr(), "/api/v1/downloads?state=lost").await;
    assert_eq!(r.status, 400);

    push_extend(&fake, [false, false, false, false]);
    let r = request(b.addr(), "POST", &retry, &[], None).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["state"], "queued");
    assert_eq!(r.json()["error"], Value::Null);
    wait_state(&b, quest, "transferring").await;
    assert_eq!(fake.bodies()[9]["arguments"]["files-wanted"], json!([0]));

    let missing = request(b.addr(), "POST", "/api/v1/downloads/999/retry", &[], None).await;
    assert_eq!(missing.status, 404);
    let missing = request(b.addr(), "DELETE", "/api/v1/downloads/999", &[], None).await;
    assert_eq!(missing.status, 404);
    fake.push(exists());
    fake.push(ok());
    fake.push(FakeResponse::success(
        json!({ "torrents": [{ "wanted": [true, false, false, false] }] }),
    ));
    fake.push(ok());
    let path = format!("/api/v1/downloads/{id}");
    let r = request(b.addr(), "DELETE", &path, &[], None).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["state"], "cancelled");
    let r = request(b.addr(), "DELETE", &path, &[], None).await;
    assert_eq!(r.status, 409);
    eventually("the deselect", || async { fake.bodies().len() == 16 }).await;
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn repeated_client_failures_back_off_and_recover() {
    let fake = FakeServer::start().await.expect("fake");
    let b = boot_transmission(&fake).await;
    let [quest, _, _] = seed_catalog(&b);
    drop_source(&b).await;
    let h = set_hash();
    let app = &b.running.app;
    push_add(&fake, &h);
    assert_eq!(want(&b, quest).await.status, 200);
    wait_state(&b, quest, "transferring").await;
    let reachable = || async {
        get(b.addr(), "/api/v1/system/status").await.json()["client"]["reachable"].clone()
    };
    assert_eq!(reachable().await, true);

    let mut poller = Poller::new();
    for n in 1..=3 {
        let next = poller.tick(app).await.expect("tick");
        assert_eq!(poller.failures(), n);
        let want_next = if n < 3 {
            Cadence::Active
        } else {
            Cadence::Backoff
        };
        assert_eq!(next, want_next);
    }
    assert_eq!(reachable().await, false);
    assert_eq!(download_of(&b, quest).await["state"], "transferring");

    fake.push(exists());
    fake.push(ok());
    fake.push(status(&h, 4, [1, 0, 0, 0], 0));
    assert_eq!(poller.tick(app).await.expect("tick"), Cadence::Active);
    assert_eq!(poller.failures(), 0);
    assert_eq!(reachable().await, true);
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn a_wanted_title_waits_for_a_source_to_bind() {
    let fake = FakeServer::start().await.expect("fake");
    let b = boot_transmission(&fake).await;
    let [quest, _, _] = seed_catalog(&b);
    let r = want(&b, quest).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let d = download_of(&b, quest).await;
    assert_eq!(d["state"], "wanted");
    assert_eq!(
        (d["source_id"].clone(), d["file_index"].clone()),
        (Value::Null, Value::Null)
    );
    assert_eq!(fake.bodies().len(), 1);

    push_add(&fake, &set_hash());
    drop_source(&b).await;
    wait_state(&b, quest, "transferring").await;
    let d = download_of(&b, quest).await;
    assert_eq!(d["file_index"], 0);
    assert_eq!(
        fake.bodies()[1]["arguments"]["files-unwanted"],
        json!([1, 2, 3])
    );
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn corename_switches_rate_limits_once_per_transition() {
    let fake = FakeServer::start().await.expect("fake");
    let b = boot_transmission(&fake).await;
    let app = &b.running.app;
    let sets = || {
        fake.bodies()
            .into_iter()
            .filter(|x| x["method"] == "session-set")
            .map(|x| x["arguments"].clone())
            .collect::<Vec<_>>()
    };
    std::fs::write(b.corename(), "MENU").expect("write");
    eventually("MENU read", || async {
        app.gate.state().corename.as_deref() == Some("MENU")
    })
    .await;
    fake.push(ok());
    fake.push(ok());
    std::fs::write(b.corename(), "SNES").expect("write");
    eventually("core limits", || async { sets().len() == 1 }).await;
    assert_eq!(
        sets()[0],
        json!({
            "speed-limit-down": 512, "speed-limit-down-enabled": true,
            "speed-limit-up": 64, "speed-limit-up-enabled": true
        })
    );
    std::fs::write(b.corename(), "N64").expect("write");
    eventually("N64 read", || async {
        app.gate.state().corename.as_deref() == Some("N64")
    })
    .await;
    std::fs::write(b.corename(), "MENU").expect("write");
    eventually("menu limits", || async { sets().len() == 2 }).await;
    assert_eq!(
        sets()[1],
        json!({ "speed-limit-down-enabled": false, "speed-limit-up-enabled": false })
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(sets().len(), 2);
    b.running.shutdown().await.expect("shutdown");
}

fn xml_ok() -> ScgiReply {
    ScgiReply::Value(Xml::Int(0))
}

/// The rtorrent status multicall reply: state, active, complete, checking,
/// hashing, then files of `(size, done chunks, chunks, priority)`.
fn rt_status(
    active: i64,
    complete: i64,
    checking: i64,
    files: &[(i64, i64, i64, i64)],
) -> ScgiReply {
    let rows = files
        .iter()
        .map(|&(s, d, c, p)| Xml::Array(vec![Xml::Int(s), Xml::Int(d), Xml::Int(c), Xml::Int(p)]))
        .collect();
    ScgiReply::multicall(vec![
        Xml::Int(1),
        Xml::Int(active),
        Xml::Int(complete),
        Xml::Int(checking),
        Xml::Int(0),
        Xml::Int(0),
        Xml::Int(0),
        Xml::Int(0),
        Xml::from(""),
        Xml::Int(0),
        Xml::Array(rows),
    ])
}

fn rt_names(fake: &FakeScgiServer) -> Vec<String> {
    fake.calls()
        .iter()
        .map(|(m, p)| match (m.as_str(), p.first()) {
            ("system.multicall", Some(Xml::Array(list))) => {
                let first = list.first().and_then(|c| match c {
                    Xml::Struct(fields) => fields.iter().find(|(k, _)| k == "methodName"),
                    _ => None,
                });
                match first {
                    Some((_, Xml::String(inner))) => format!("multi:{inner}"),
                    _ => "multi:?".to_owned(),
                }
            }
            _ => m.clone(),
        })
        .collect()
}

#[tokio::test]
async fn want_extend_and_poll_through_rtorrent() {
    let fake = FakeScgiServer::start().await.expect("fake");
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = config_in(dir.path());
    config.client.kind = ClientChoice::Rtorrent;
    config.client.url = fake.addr();
    let b = boot_with(dir, config).await;
    let hold = hold_imports(&b).await;
    let [quest, second, _] = seed_catalog(&b);
    drop_source(&b).await;
    let app = &b.running.app;

    fake.push(ScgiReply::fault(-501, "Could not find info-hash."));
    fake.push(xml_ok());
    fake.push(xml_ok());
    fake.push(ScgiReply::multicall(vec![Xml::Int(0); 4]));
    fake.push(xml_ok());
    fake.push(xml_ok());
    assert_eq!(want(&b, quest).await.status, 200);
    wait_state(&b, quest, "transferring").await;
    assert_eq!(
        rt_names(&fake),
        [
            "d.hash",
            "load.raw",
            "d.directory.set",
            "multi:f.priority.set",
            "d.update_priorities",
            "d.start"
        ]
    );
    let calls = fake.calls();
    let staging = app.config().paths.staging().join(set_hash());
    assert_eq!(calls[2].1[1], Xml::from(staging.to_string_lossy().as_ref()));
    let Xml::Array(prio) = &calls[3].1[0] else {
        panic!("multicall")
    };
    let flags: Vec<String> = prio
        .iter()
        .map(|c| match c {
            Xml::Struct(f) => format!("{:?}", f.iter().find(|(k, _)| k == "params")),
            _ => String::new(),
        })
        .collect();
    assert!(
        flags[0].contains("Int(1)") && flags[2].contains("Int(0)"),
        "{flags:?}"
    );

    fake.push(ScgiReply::multicall(vec![Xml::Int(0), Xml::Int(4)]));
    fake.push(ScgiReply::multicall(vec![Xml::Int(0); 4]));
    fake.push(xml_ok());
    fake.push(xml_ok());
    assert_eq!(want(&b, second).await.status, 200);
    wait_state(&b, second, "transferring").await;
    assert_eq!(
        rt_names(&fake)[6..],
        [
            "multi:d.is_meta",
            "multi:f.priority.set",
            "d.update_priorities",
            "d.start"
        ]
    );

    let mut poller = Poller::new();
    let files = [
        (40_976, 2, 3, 1),
        (120, 0, 1, 0),
        (24_592, 0, 2, 1),
        (65_552, 0, 5, 0),
    ];
    fake.push(xml_ok());
    fake.push(rt_status(1, 0, 1, &files));
    poller.tick(app).await.expect("tick");
    assert_eq!(download_of(&b, quest).await["state"], "transferring");
    let mut files = files;
    files[0].1 = 3;
    fake.push(rt_status(1, 0, 1, &files));
    poller.tick(app).await.expect("tick");
    assert_eq!(download_of(&b, quest).await["state"], "checking");
    fake.push(rt_status(1, 0, 0, &files));
    poller.tick(app).await.expect("tick");
    assert_eq!(download_of(&b, quest).await["state"], "importing");
    files[2].1 = 2;
    fake.push(rt_status(1, 0, 0, &files));
    fake.push(xml_ok());
    assert_eq!(poller.tick(app).await.expect("tick"), Cadence::Idle);
    assert_eq!(download_of(&b, second).await["state"], "importing");
    let tail: Vec<String> = rt_names(&fake).into_iter().skip(10).collect();
    assert_eq!(
        tail,
        [
            "d.hash",
            "multi:d.state",
            "multi:d.state",
            "multi:d.state",
            "multi:d.state",
            "d.stop"
        ]
    );
    let n = app
        .db
        .read(|c| rows::count_in(c, &[DownloadState::Importing]))
        .await
        .expect("count");
    assert_eq!(n, 2);
    hold.notify_one();
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn refused_rate_limits_are_retried_until_the_client_takes_them() {
    let fake = FakeServer::start().await.expect("fake");
    let b = boot_transmission(&fake).await;
    let sets = || {
        fake.bodies()
            .into_iter()
            .filter(|x| x["method"] == "session-set")
            .map(|x| x["arguments"]["speed-limit-down"].clone())
            .collect::<Vec<_>>()
    };
    fake.push(FakeResponse::failure("busy"));
    fake.push(ok());
    std::fs::write(b.corename(), "SNES").expect("write");
    eventually("the retried core limits", || async { sets().len() == 2 }).await;
    assert_eq!(sets(), [json!(512), json!(512)]);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(sets().len(), 2, "no call once the limits are applied");
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn a_source_with_only_finished_downloads_can_be_deleted() {
    let fake = FakeServer::start().await.expect("fake");
    let b = boot_transmission(&fake).await;
    let [quest, second, _] = seed_catalog(&b);
    drop_source(&b).await;
    let h = set_hash();
    let app = &b.running.app;
    push_add(&fake, &h);
    assert_eq!(want(&b, quest).await.status, 200);
    wait_state(&b, quest, "transferring").await;
    push_extend(&fake, [true, false, false, false]);
    assert_eq!(want(&b, second).await.status, 200);
    wait_state(&b, second, "transferring").await;

    let done = rows::DownloadId(download_of(&b, quest).await["id"].as_i64().expect("id"));
    app.db
        .write(move |c| {
            for to in [DownloadState::Importing, DownloadState::Done] {
                rows::move_all(c, &[done], to, None, 1)?;
            }
            Ok(())
        })
        .await
        .expect("finish");
    let source = get(b.addr(), "/api/v1/sources").await.json()["items"][0]["id"].clone();
    let path = format!("/api/v1/sources/{source}");
    let r = request(b.addr(), "DELETE", &path, &[], None).await;
    assert_eq!(r.status, 400, "{}", r.body);
    assert!(
        r.body
            .contains("queued, transferring, checking or importing"),
        "{}",
        r.body
    );

    fake.push(exists());
    fake.push(ok());
    fake.push(FakeResponse::success(
        json!({ "torrents": [{ "wanted": [false, false, true, false] }] }),
    ));
    fake.push(ok());
    let other = download_of(&b, second).await["id"].clone();
    let cancel = format!("/api/v1/downloads/{other}");
    let r = request(b.addr(), "DELETE", &cancel, &[], None).await;
    assert_eq!(r.status, 200, "{}", r.body);
    eventually("the deselect", || async { fake.bodies().len() == 13 }).await;

    fake.push(exists());
    fake.push(ok());
    let r = request(b.addr(), "DELETE", &path, &[], None).await;
    assert_eq!(r.status, 204, "{}", r.body);
    assert_eq!(
        methods(&fake).last().map(String::as_str),
        Some("torrent-remove")
    );
    let mut kept: Vec<(Value, Value)> = downloads(&b, "")
        .await
        .iter()
        .map(|d| (d["state"].clone(), d["source_id"].clone()))
        .collect();
    kept.sort_by_key(|(s, _)| s.to_string());
    assert_eq!(
        kept,
        [
            (json!("cancelled"), Value::Null),
            (json!("done"), Value::Null)
        ]
    );
    b.running.shutdown().await.expect("shutdown");
}
