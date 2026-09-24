//! The watched `dats/` flow and the Platforms, Catalog and DATs routes over real sockets.

mod common;

use std::io::{Cursor, Write as _};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::{boot, eventually, get, request, Booted};
use mistarr_server::events::{Event, EventKind};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::broadcast;

/// A Logiqx DAT; each game is `(name, roms)`.
fn dat(name: &str, version: &str, games: &[(&str, &[&str])]) -> String {
    let mut xml = format!(
        "<?xml version=\"1.0\"?>\n<datafile>\n<header><name>{name}</name><version>{version}</version></header>\n"
    );
    let mut n = 0u32;
    for (game, roms) in games {
        xml.push_str(&format!("<game name=\"{game}\">\n"));
        for rom in *roms {
            n += 1;
            xml.push_str(&format!(
                "  <rom name=\"{rom}\" size=\"{n}\" crc=\"{n:08x}\" sha1=\"{n:040x}\"/>\n"
            ));
        }
        xml.push_str("</game>\n");
    }
    xml.push_str("</datafile>\n");
    xml
}

fn gb_dat(version: &str, games: &[(&str, &[&str])]) -> String {
    dat("Maker - Game Boy", version, games)
}

fn pack(members: &[(&str, &str)]) -> Vec<u8> {
    let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, body) in members {
        z.start_file(*name, zip::write::SimpleFileOptions::default())
            .expect("start");
        z.write_all(body.as_bytes()).expect("write");
    }
    z.finish().expect("finish").into_inner()
}

fn dats_dir(b: &Booted) -> std::path::PathBuf {
    b.dir.path().join("data/dats")
}

/// Writes under a dotfile name and renames, as a copy tool that finishes would.
fn drop_file(dir: &Path, name: &str, bytes: &[u8]) {
    let part = dir.join(format!(".{name}.part"));
    std::fs::write(&part, bytes).expect("write");
    std::fs::rename(part, dir.join(name)).expect("rename");
}

/// Waits for an event of `kind` whose data contains `needle`.
async fn wait_event(
    rx: &mut broadcast::Receiver<Arc<Event>>,
    kind: EventKind,
    needle: &str,
) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let e = tokio::time::timeout_at(deadline, rx.recv())
            .await
            .unwrap_or_else(|_| panic!("no {kind:?} with {needle:?}"))
            .expect("event");
        if e.kind == kind && e.data.contains(needle) {
            return serde_json::from_str(&e.data).expect("json");
        }
    }
}

async fn json_of(addr: SocketAddr, path: &str) -> Value {
    let r = get(addr, path).await;
    assert_eq!(r.status, 200, "{path}: {}", r.body);
    r.json()
}

async fn send(addr: SocketAddr, method: &str, path: &str, body: &str) -> common::Response {
    request(addr, method, path, &[], Some(body)).await
}

fn names(page: &Value) -> Vec<String> {
    page["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|i| i["base_name"].as_str().expect("name").to_owned())
        .collect()
}

fn variant<'a>(detail: &'a Value, name: &str) -> &'a Value {
    detail["variants"]
        .as_array()
        .expect("variants")
        .iter()
        .find(|v| v["name"] == name)
        .unwrap_or_else(|| panic!("no variant {name}"))
}

/// Posts one file as `multipart/form-data`.
async fn upload(addr: SocketAddr, name: &str, bytes: &[u8]) -> common::Response {
    let boundary = "mistarr-test-boundary";
    let mut body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\n\
         Content-Type: application/octet-stream\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    let head = format!(
        "POST /api/v1/dats/upload HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nX-Mistarr: 1\r\n\
         Content-Type: multipart/form-data; boundary={boundary}\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await.expect("head");
    stream.write_all(&body).await.expect("body");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.expect("read");
    let text = String::from_utf8_lossy(&raw);
    let (head, body) = text.split_once("\r\n\r\n").expect("response");
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .expect("status");
    common::Response {
        status,
        headers: Vec::new(),
        body: body.to_owned(),
    }
}

const QUEST_USA: &str = "Example Quest (USA)";
const QUEST_JAPAN: &str = "Example Quest (Japan)";
const QUEST_REV: &str = "Example Quest (USA) (Rev 1)";
const BIOS: &str = "[BIOS] Example System (World)";
const BETA: &str = "Example Puzzle (USA) (Beta)";
const SAGA: &str = "Example Saga (USA)";

fn quest_games() -> Vec<(&'static str, &'static [&'static str])> {
    vec![
        (QUEST_USA, &["Example Quest (USA).gb"]),
        (QUEST_JAPAN, &["Example Quest (Japan).gb"]),
        (QUEST_REV, &["Example Quest (USA) (Rev 1).gb"]),
        (BIOS, &["Example System (World).bin"]),
        (BETA, &["Example Puzzle (USA) (Beta).gb"]),
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dropped_pack_loads_and_the_catalog_answers() {
    let booted = boot().await;
    let addr = booted.addr();
    let mut events = booted.running.app.events.subscribe(None).live;
    let saga = dat(
        "Maker - PlayStation",
        "2",
        &[(
            SAGA,
            &[
                "Example Saga (USA).cue",
                "Example Saga (USA) (Track 1).bin",
                "Example Saga (USA) (Track 2).bin",
            ],
        )],
    );
    let unbound = dat(
        "Test Console",
        "1",
        &[("Example Toy (World)", &["toy.bin"])],
    );
    let bytes = pack(&[
        ("Maker - Game Boy.dat", &gb_dat("1", &quest_games())),
        ("Maker - PlayStation.xml", &saga),
        ("Test Console.dat", &unbound),
        ("readme.txt", "synthetic"),
    ]);
    drop_file(&dats_dir(&booted), "daily.zip", &bytes);
    let mut loaded = Vec::new();
    for _ in 0..3 {
        loaded.push(wait_event(&mut events, EventKind::DatLoaded, "daily.zip").await);
    }
    assert!(dats_dir(&booted).join("loaded/daily.zip").is_file());
    assert!(!dats_dir(&booted).join("daily.zip").exists());
    let platforms: Vec<Value> = loaded.iter().map(|e| e["platform_id"].clone()).collect();
    assert_eq!(platforms, [json!("gb"), json!("psx"), Value::Null]);

    let dats = json_of(addr, "/api/v1/dats").await;
    assert_eq!(dats["total"], 3);
    let unbound_row = dats["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|d| d["dat_name"] == "Test Console")
        .expect("unbound")
        .clone();
    assert!(unbound_row["platform_id"].is_null());
    assert_eq!(unbound_row["game_count"], 1);
    assert_eq!(unbound_row["source_file"], "daily.zip");

    let all = json_of(addr, "/api/v1/platforms").await;
    let gb = all["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|p| p["id"] == "gb")
        .expect("gb");
    assert_eq!(
        gb["counts"],
        json!({"titles": 1, "have": 0, "wanted": 0, "unmatched_files": 0,
               "failing_check": 0, "partial": 0})
    );
    assert_eq!(gb["core_dir"], "GAMEBOY");

    // Browse: BIOS and beta entries are hidden until asked for.
    let page = json_of(addr, "/api/v1/platforms/gb/titles").await;
    assert_eq!(
        (names(&page), page["total"].clone()),
        (vec!["Example Quest".to_owned()], json!(1))
    );
    let row = &page["items"][0];
    assert_eq!(row["pick_name"], QUEST_REV);
    assert_eq!(
        (row["variants"].clone(), row["has_pick"].clone()),
        (json!(3), json!(true))
    );
    assert_eq!(
        row["art"]["boxart"],
        "https://thumbnails.libretro.com/Nintendo%20-%20Game%20Boy/Named_Boxarts/Example%20Quest%20%28USA%29%20%28Rev%201%29.png"
    );
    for (query, expected) in [
        ("flags=bios", vec![]),
        ("flags=bios&hidden=show", vec!["Example System"]),
        (
            "hidden=show",
            vec!["Example Puzzle", "Example Quest", "Example System"],
        ),
        ("flags=beta", vec![]),
        ("flags=beta&hidden=show", vec!["Example Puzzle"]),
        ("region=japan", vec!["Example Quest"]),
        ("region=Europe", vec![]),
        ("q=QUEST", vec!["Example Quest"]),
        ("q=zzz", vec![]),
        ("have=yes", vec![]),
        ("have=no", vec!["Example Quest"]),
        ("wanted=yes", vec![]),
        ("sort=recent&flags=beta&hidden=show", vec!["Example Puzzle"]),
        ("limit=1&offset=1", vec![]),
        ("hidden=nope", vec![]),
    ] {
        if query == "hidden=nope" {
            assert_eq!(
                get(addr, "/api/v1/platforms/gb/titles?hidden=nope")
                    .await
                    .status,
                400
            );
            continue;
        }
        let page = json_of(addr, &format!("/api/v1/platforms/gb/titles?{query}")).await;
        assert_eq!(names(&page), expected, "{query}");
    }
    assert_eq!(
        get(addr, "/api/v1/platforms/gb/titles?sort=size")
            .await
            .status,
        400
    );
    assert_eq!(get(addr, "/api/v1/platforms/nope/titles").await.status, 404);

    // Title detail: the whole group, its roms and the pick's art.
    let parent = row["parent_id"].as_i64().expect("parent");
    let detail = json_of(addr, &format!("/api/v1/titles/{parent}")).await;
    assert_eq!(detail["base_name"], "Example Quest");
    assert_eq!(detail["variants"].as_array().expect("variants").len(), 3);
    let rev = variant(&detail, QUEST_REV);
    assert_eq!(detail["pick_variant_id"], rev["id"]);
    assert_eq!(rev["regions"], json!(["USA"]));
    assert_eq!(rev["revision"], "Rev 1");
    assert_eq!(rev["roms"][0]["name"], "Example Quest (USA) (Rev 1).gb");
    assert!(rev["roms"][0]["file_state"].is_null());
    assert!(detail["art"]["snap"].as_str().is_some_and(
        |s| s.ends_with("/Named_Snaps/Example%20Quest%20%28USA%29%20%28Rev%201%29.png")
    ));
    let japan = variant(&detail, QUEST_JAPAN)["id"].as_i64().expect("id");
    let via_clone = json_of(addr, &format!("/api/v1/titles/{japan}")).await;
    assert_eq!(via_clone["parent_id"], detail["parent_id"]);
    assert_eq!(get(addr, "/api/v1/titles/99999").await.status, 404);

    let psx = json_of(addr, "/api/v1/platforms/psx/titles").await;
    assert_eq!(names(&psx), ["Example Saga"]);
    let saga = json_of(
        addr,
        &format!("/api/v1/titles/{}", psx["items"][0]["parent_id"]),
    )
    .await;
    assert_eq!(
        variant(&saga, SAGA)["roms"].as_array().expect("roms").len(),
        3
    );
    assert!(saga["art"]["title"]
        .as_str()
        .is_some_and(|s| s.contains("/Sony%20-%20PlayStation/Named_Titles/")));

    // Want and unwant.
    let r = send(addr, "POST", &format!("/api/v1/titles/{japan}/want"), "").await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        variant(&r.json(), QUEST_REV)["wanted"],
        true,
        "the pick is wanted"
    );
    let body = json!({ "variant_id": japan }).to_string();
    let r = send(
        addr,
        "POST",
        &format!("/api/v1/titles/{parent}/want"),
        &body,
    )
    .await;
    assert_eq!(variant(&r.json(), QUEST_JAPAN)["wanted"], true);
    let wanted = json_of(addr, "/api/v1/platforms/gb/titles?wanted=yes").await;
    assert_eq!(wanted["items"][0]["wanted"], 2);
    let bios_page = json_of(addr, "/api/v1/platforms/gb/titles?flags=bios&hidden=show").await;
    let bios_id = bios_page["items"][0]["parent_id"].as_i64().expect("bios");
    let r = send(
        addr,
        "POST",
        &format!("/api/v1/titles/{bios_id}/want"),
        &json!({ "variant_id": bios_id }).to_string(),
    )
    .await;
    assert_eq!(r.status, 400, "{}", r.body);
    let r = send(
        addr,
        "POST",
        &format!("/api/v1/titles/{bios_id}/want"),
        "{}",
    )
    .await;
    assert_eq!(r.status, 400, "a BIOS group has no pick");
    let r = send(
        addr,
        "POST",
        &format!("/api/v1/titles/{parent}/want"),
        &json!({ "variant_id": bios_id }).to_string(),
    )
    .await;
    assert_eq!(r.status, 400, "variant from another group");
    let r = send(addr, "DELETE", &format!("/api/v1/titles/{parent}/want"), "").await;
    assert_eq!(r.status, 200);
    let after = r.json();
    assert!(after["variants"]
        .as_array()
        .expect("variants")
        .iter()
        .all(|v| v["wanted"] == false));
    let r = send(
        addr,
        "POST",
        &format!("/api/v1/titles/{parent}/rename"),
        "{}",
    )
    .await;
    assert_eq!(r.status, 400, "rename needs a file_id");
    let r = send(
        addr,
        "POST",
        &format!("/api/v1/titles/{parent}/rename"),
        r#"{"file_id":999}"#,
    )
    .await;
    assert_eq!(r.status, 404);

    // Platform switches.
    let r = send(addr, "PUT", "/api/v1/platforms/gb", r#"{"enabled":false}"#).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        (
            r.json()["enabled"].clone(),
            r.json()["counts"]["titles"].clone()
        ),
        (json!(false), json!(1))
    );
    assert_eq!(
        send(addr, "PUT", "/api/v1/platforms/nope", r#"{"enabled":true}"#)
            .await
            .status,
        404
    );
    assert_eq!(
        send(addr, "PUT", "/api/v1/platforms/gb", r#"{"on":true}"#)
            .await
            .status,
        400
    );

    // Binding the unbound version loads its titles for the chosen platform.
    let id = unbound_row["id"].as_i64().expect("id");
    let body = json!({ "dat_version_id": id }).to_string();
    let r = send(addr, "POST", "/api/v1/platforms/nes/dat", &body).await;
    assert_eq!(r.status, 202, "{}", r.body);
    wait_event(
        &mut events,
        EventKind::DatLoaded,
        &format!("\"dat_version_id\":{id}"),
    )
    .await;
    let nes = json_of(addr, "/api/v1/platforms/nes/titles").await;
    assert_eq!(names(&nes), ["Example Toy"]);
    assert_eq!(
        send(addr, "POST", "/api/v1/platforms/nes/dat", &body)
            .await
            .status,
        400
    );
    assert_eq!(
        send(addr, "POST", "/api/v1/platforms/nope/dat", &body)
            .await
            .status,
        404
    );
    let missing = json!({ "dat_version_id": 9999 }).to_string();
    assert_eq!(
        send(addr, "POST", "/api/v1/platforms/nes/dat", &missing)
            .await
            .status,
        404
    );

    // Changing preferences recomputes the picks.
    let r = send(
        addr,
        "PUT",
        "/api/v1/system/settings",
        r#"{"prefs":{"regions":["Japan"]}}"#,
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    eventually("the Japanese variant to become the pick", || async {
        let page = get(addr, "/api/v1/platforms/gb/titles").await.json();
        page["items"][0]["pick_name"] == QUEST_JAPAN
    })
    .await;
    booted.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_version_supersedes_the_first() {
    let booted = boot().await;
    let addr = booted.addr();
    let mut events = booted.running.app.events.subscribe(None).live;
    drop_file(
        &dats_dir(&booted),
        "gb.dat",
        gb_dat("20240101", &quest_games()).as_bytes(),
    );
    let first = wait_event(&mut events, EventKind::DatLoaded, "gb.dat").await;
    let page = json_of(addr, "/api/v1/platforms/gb/titles").await;
    let parent = page["items"][0]["parent_id"].as_i64().expect("parent");
    let before = json_of(addr, &format!("/api/v1/titles/{parent}")).await;
    let japan = variant(&before, QUEST_JAPAN)["id"].clone();
    let body = json!({ "variant_id": japan }).to_string();
    assert_eq!(
        send(
            addr,
            "POST",
            &format!("/api/v1/titles/{parent}/want"),
            &body
        )
        .await
        .status,
        200
    );

    // The new version drops the unrevised release and adds a European one.
    let games: Vec<_> = quest_games()
        .into_iter()
        .filter(|(n, _)| *n != QUEST_USA)
        .chain([("Example Quest (Europe)", &["Example Quest (Europe).gb"][..])])
        .collect();
    drop_file(
        &dats_dir(&booted),
        "gb.dat",
        gb_dat("20240201", &games).as_bytes(),
    );
    let second = wait_event(&mut events, EventKind::DatLoaded, "gb.dat").await;
    assert_ne!(first["dat_version_id"], second["dat_version_id"]);
    assert!(dats_dir(&booted).join("loaded/gb.dat").is_file());
    assert!(dats_dir(&booted).join("loaded/gb (1).dat").is_file());

    let dats = json_of(addr, "/api/v1/dats").await;
    let old = dats["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|d| d["id"] == first["dat_version_id"])
        .expect("old");
    assert_eq!(old["superseded_by"], second["dat_version_id"]);

    let after = json_of(addr, &format!("/api/v1/titles/{parent}")).await;
    assert_eq!(after["variants"].as_array().expect("variants").len(), 4);
    let usa = variant(&after, QUEST_USA);
    assert_eq!(
        (usa["retired"].clone(), usa["dat_version_id"].clone()),
        (json!(true), first["dat_version_id"].clone())
    );
    let kept = variant(&after, QUEST_JAPAN);
    assert_eq!(kept["id"], japan, "a kept entry keeps its id");
    assert_eq!(kept["wanted"], true, "and its wanted mark");
    assert_eq!(kept["dat_version_id"], second["dat_version_id"]);
    assert_eq!(variant(&after, QUEST_REV)["is_1g1r_pick"], true);
    let page = json_of(addr, "/api/v1/platforms/gb/titles").await;
    assert_eq!(
        page["items"][0]["variants"], 3,
        "retired variants leave the counts"
    );
    let r = send(
        addr,
        "POST",
        &format!("/api/v1/titles/{parent}/want"),
        &json!({ "variant_id": usa["id"] }).to_string(),
    )
    .await;
    assert_eq!(r.status, 400, "retired entries cannot be wanted");

    // Retiring the current version empties the catalog but keeps the rows.
    let id = second["dat_version_id"].as_i64().expect("id");
    assert_eq!(
        send(addr, "DELETE", &format!("/api/v1/dats/{id}"), "")
            .await
            .status,
        204
    );
    assert_eq!(
        send(addr, "DELETE", "/api/v1/dats/9999", "").await.status,
        404
    );
    let page = json_of(addr, "/api/v1/platforms/gb/titles").await;
    assert_eq!(page["total"], 0);
    let dats = json_of(addr, "/api/v1/dats?limit=1").await;
    assert_eq!(
        (dats["total"].clone(), dats["items"][0]["retired"].clone()),
        (json!(2), json!(true))
    );
    assert_eq!(
        json_of(addr, &format!("/api/v1/titles/{parent}")).await["variants"]
            .as_array()
            .expect("v")
            .len(),
        4
    );
    booted.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_files_are_rejected_and_uploads_are_imported() {
    let booted = boot().await;
    let addr = booted.addr();
    let mut events = booted.running.app.events.subscribe(None).live;
    let dir = dats_dir(&booted);
    drop_file(
        &dir,
        "broken.dat",
        b"<datafile><game name=\"A\"><rom name=\"a\" size=\"x\"/></game></datafile>",
    );
    let e = wait_event(&mut events, EventKind::DatRejected, "broken.dat").await;
    assert!(
        e["reason"].as_str().is_some_and(|r| r.contains("size")),
        "{e}"
    );
    assert!(dir.join("rejected/broken.dat").is_file());
    let reason =
        std::fs::read_to_string(dir.join("rejected/broken.dat.reason.txt")).expect("reason");
    assert!(reason.contains("size"));
    drop_file(&dir, "notes.txt", b"synthetic");
    wait_event(&mut events, EventKind::DatRejected, "notes.txt").await;
    drop_file(&dir, "empty.zip", &pack(&[("readme.txt", "synthetic")]));
    wait_event(&mut events, EventKind::DatRejected, "empty.zip").await;
    assert!(dir.join("rejected/empty.zip.reason.txt").is_file());
    assert_eq!(json_of(addr, "/api/v1/dats").await["total"], 0);

    let r = upload(addr, "gb.dat", gb_dat("1", &quest_games()).as_bytes()).await;
    assert_eq!(r.status, 202, "{}", r.body);
    assert_eq!(r.json()["file"], "gb.dat");
    wait_event(&mut events, EventKind::DatLoaded, "gb.dat").await;
    assert_eq!(
        json_of(addr, "/api/v1/platforms/gb/titles").await["total"],
        1
    );
    let r = upload(addr, "notes.txt", b"synthetic").await;
    assert_eq!(r.status, 400);
    let w = json_of(addr, "/api/v1/system/wizard").await;
    assert_eq!(w["dats"], true);
    booted.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_file_whose_import_failed_is_imported_again() {
    let booted = boot().await;
    let addr = booted.addr();
    let app = Arc::clone(&booted.running.app);
    let dir = dats_dir(&booted);
    let loaded = dir.join("loaded");
    std::fs::remove_dir(&loaded).expect("rmdir");
    // A dangling link where loaded/ should be fails the move and is not a file the watcher lists.
    std::os::unix::fs::symlink(dir.join("missing"), &loaded).expect("block");
    let mut events = app.events.subscribe(None).live;
    drop_file(&dir, "gb.dat", gb_dat("1", &quest_games()).as_bytes());
    wait_event(&mut events, EventKind::JobProgress, "\"state\":\"failed\"").await;
    assert!(dir.join("gb.dat").is_file(), "the file stays for a retry");
    std::fs::remove_file(&loaded).expect("unblock");
    std::fs::create_dir(&loaded).expect("mkdir");
    wait_event(&mut events, EventKind::DatLoaded, "gb.dat").await;
    assert!(loaded.join("gb.dat").is_file());
    assert_eq!(
        json_of(addr, "/api/v1/platforms/gb/titles").await["total"],
        1
    );
    booted.running.shutdown().await.expect("shutdown");
}
