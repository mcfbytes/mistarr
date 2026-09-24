//! Behaviour seen on a real board: work while a core runs, restarts, No-Intro-shaped DATs.

mod common;

use std::fmt::Write as _;
use std::io::{Cursor, Write as _};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::{boot_with, config_in, eventually, get, request, Booted};
use mistarr_server::events::{Event, EventKind};
use serde_json::Value;
use tokio::sync::broadcast;

const SYSTEM: &str = "Nintendo - Super Nintendo Entertainment System";

/// A DAT shaped like a No-Intro export: schema attributes on the root, an `<id>`,
/// long legal elements in the header, `id`/`cloneofid` on games and `sha256` on roms.
fn no_intro_dat(version: &str, games: usize) -> String {
    let legal = "Synthetic notice text for the fixture only. ".repeat(80);
    let mut xml = format!(
        "<?xml version=\"1.0\"?>\n\
         <!DOCTYPE datafile PUBLIC \"-//Logiqx//DTD ROM Management Datafile//EN\" \"https://example.invalid/datafile.dtd\">\n\
         <datafile xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" \
         xsi:schemaLocation=\"https://example.invalid/schema https://example.invalid/schema/datfile_v3.xsd\">\n\
         \t<header>\n\t\t<id>49</id>\n\t\t<name>{SYSTEM}</name>\n\t\t<description>{SYSTEM}</description>\n\
         \t\t<version>{version}</version>\n\t\t<author>fixture &amp; tester</author>\n\
         \t\t<homepage>Example group</homepage>\n\t\t<url>https://example.invalid/</url>\n\
         \t\t<trademarks>{legal}</trademarks>\n\t\t<piracy>{legal}</piracy>\n\t</header>\n"
    );
    for i in 0..games {
        let clone = if i % 2 == 1 {
            format!(" cloneofid=\"{:04}\"", i - 1)
        } else {
            String::new()
        };
        let _ = write!(
            xml,
            "\t<game name=\"Example Title {i} (USA)\" id=\"{i:04}\"{clone}>\n\
             \t\t<description>Example Title {i} (USA)</description>\n\
             \t\t<rom name=\"Example Title {i} (USA).sfc\" size=\"{size}\" crc=\"{i:08x}\" \
             sha1=\"{i:040x}\" sha256=\"{i:064x}\" status=\"verified\" serial=\"SYN-{i:04}\"/>\n\t</game>\n",
            size = 1024 + i,
        );
    }
    xml.push_str("</datafile>\n");
    xml
}

fn zipped(name: &str, body: &str) -> Vec<u8> {
    let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
    z.start_file(name, zip::write::SimpleFileOptions::default())
        .expect("start");
    z.write_all(body.as_bytes()).expect("write");
    z.finish().expect("finish").into_inner()
}

fn drop_file(dir: &Path, name: &str, bytes: &[u8]) {
    let part = dir.join(format!(".{name}.part"));
    std::fs::write(&part, bytes).expect("write");
    std::fs::rename(part, dir.join(name)).expect("rename");
}

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

async fn json_of(b: &Booted, path: &str) -> Value {
    let r = get(b.addr(), path).await;
    assert_eq!(r.status, 200, "{path}: {}", r.body);
    r.json()
}

/// Boots with a core loaded and an `_Arcade` directory, so the arcade
/// catalogue is queued on the heavy lane and held from the start.
async fn boot_with_core(dir: tempfile::TempDir) -> Booted {
    std::fs::write(dir.path().join("CORENAME"), "NES").expect("corename");
    std::fs::create_dir_all(dir.path().join("_Arcade")).expect("arcade");
    let config = config_in(dir.path());
    boot_with(dir, config).await
}

fn arcade_rows(waiting: &Value) -> usize {
    waiting
        .as_array()
        .expect("list")
        .iter()
        .filter(|j| j["kind"] == "arcade_catalog")
        .count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dats_load_while_a_core_holds_the_heavy_lane() {
    let booted = boot_with_core(tempfile::tempdir().expect("tempdir")).await;
    let app = Arc::clone(&booted.running.app);
    let mut events = app.events.subscribe(None).live;
    let status = json_of(&booted, "/api/v1/system/status").await;
    assert_eq!(status["paused"], true, "{status}");
    assert_eq!(status["corename"], "NES");
    assert_eq!(arcade_rows(&status["waiting"]), 1, "{status}");

    let jobs = json_of(&booted, "/api/v1/system/jobs").await;
    let arcade = jobs["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|j| j["kind"] == "arcade_catalog")
        .expect("arcade job listed");
    assert_eq!(arcade["state"], "queued");
    assert_eq!(arcade["lane"], "heavy");
    assert_eq!(arcade["reason"], "Paused while NES is running");

    let dats = booted.dir.path().join("data/dats");
    let xml = no_intro_dat("20260101-000000", 40);
    drop_file(
        &dats,
        &format!("{SYSTEM} (20260101-000000).dat"),
        xml.as_bytes(),
    );
    let loaded = wait_event(&mut events, EventKind::DatLoaded, ".dat").await;
    assert_eq!(loaded["platform_id"], "snes", "{loaded}");
    let member = format!("{SYSTEM} (20260101-000000).dat");
    drop_file(
        &dats,
        &format!("{SYSTEM} (20260101-000000).zip"),
        &zipped(&member, &xml),
    );
    let loaded = wait_event(&mut events, EventKind::DatLoaded, ".zip").await;
    assert_eq!(loaded["platform_id"], "snes", "{loaded}");
    let titles = json_of(&booted, "/api/v1/platforms/snes/titles").await;
    assert_eq!(titles["total"], 40);
    let wizard = json_of(&booted, "/api/v1/system/wizard").await;
    assert_eq!(wizard["dats"], true);

    let r = request(booted.addr(), "POST", "/api/v1/system/resume", &[], None).await;
    assert_eq!(r.status, 200);
    eventually("the held queue to drain and the override to end", || {
        let app = Arc::clone(&app);
        async move {
            let s = app.gate.state();
            s.manual.is_none() && s.paused()
        }
    })
    .await;
    let status = json_of(&booted, "/api/v1/system/status").await;
    assert_eq!(status["override"], Value::Null);
    assert_eq!(arcade_rows(&status["waiting"]), 0, "{status}");
    booted.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_takes_over_held_jobs_instead_of_adding_more() {
    let booted = boot_with_core(tempfile::tempdir().expect("tempdir")).await;
    let first = json_of(&booted, "/api/v1/system/status").await["waiting"][0]["id"].clone();
    let r = request(booted.addr(), "POST", "/api/v1/system/cores", &[], None).await;
    assert_eq!(r.status, 200);
    assert_eq!(
        r.json()["arcade_job_id"],
        first,
        "joins the queued catalogue"
    );
    let Booted { dir, running } = booted;
    running.shutdown().await.expect("shutdown");

    let config = config_in(dir.path());
    let again = boot_with(dir, config).await;
    let status = json_of(&again, "/api/v1/system/status").await;
    assert_eq!(arcade_rows(&status["waiting"]), 1, "{status}");
    assert_eq!(status["waiting"][0]["id"], first, "the same row, re-queued");
    again.running.shutdown().await.expect("shutdown");
}

fn bstr(s: &str) -> String {
    format!("{}:{s}", s.len())
}

/// A set torrent named like a board user's: `Example_Archive/No-Intro/<system>/<title>.zip`.
fn set_torrent(games: usize) -> Vec<u8> {
    let mut list = String::from("l");
    for i in 0..games {
        let leaf = format!("Example Title {i} (USA).zip");
        list.push_str(&format!(
            "d6:lengthi{}e4:pathl{}{}{}ee",
            100 + i,
            bstr("No-Intro"),
            bstr(SYSTEM),
            bstr(&leaf)
        ));
    }
    list.push('e');
    let info = format!(
        "d5:files{list}4:name{}12:piece lengthi16384e6:pieces0:e",
        bstr("Example_Archive")
    );
    let announce = bstr("http://tracker.invalid/announce");
    format!("d8:announce{announce}4:info{info}e").into_bytes()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_set_torrent_waits_for_its_dat_and_then_binds() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = config_in(dir.path());
    let booted = boot_with(dir, config).await;
    let mut events = booted.running.app.events.subscribe(None).live;
    let data = booted.dir.path().join("data");
    let name = format!("Example_Archive - No-Intro - {SYSTEM}.torrent");
    drop_file(&data.join("sources"), &name, &set_torrent(12));
    wait_event(&mut events, EventKind::SourceChanged, "unbound").await;
    let source = &json_of(&booted, "/api/v1/sources").await["items"][0];
    assert_eq!(source["suggested_platform_id"], "snes", "{source}");
    assert_eq!(source["platform_id"], Value::Null);
    assert!(
        source["reason"]
            .as_str()
            .is_some_and(|r| r.starts_with("Looks like Super Nintendo Entertainment System.")),
        "{source}"
    );

    let xml = no_intro_dat("20260101-000000", 12);
    drop_file(&data.join("dats"), "system.dat", xml.as_bytes());
    wait_event(&mut events, EventKind::SourceChanged, "\"bound\"").await;
    let source = &json_of(&booted, "/api/v1/sources").await["items"][0];
    assert_eq!(source["platform_id"], "snes", "{source}");
    assert_eq!(source["matched_count"], 12);
    assert_eq!(source["reason"], Value::Null);
    booted.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_wizard_opens_until_dismissed_and_settings_keep_its_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = config_in(dir.path());
    let booted = boot_with(dir, config).await;
    let addr = booted.addr();
    let wizard = json_of(&booted, "/api/v1/system/wizard").await;
    assert_eq!(wizard["open_on_start"], true);

    let mut settings = json_of(&booted, "/api/v1/system/settings").await;
    let put = |body: String| async move {
        request(addr, "PUT", "/api/v1/system/settings", &[], Some(&body)).await
    };
    settings["client"]["remote_path_map"] = serde_json::json!([{ "remote": "", "local": "" }]);
    let r = put(settings.to_string()).await;
    assert_eq!(r.status, 400, "a blank mapping is refused: {}", r.body);
    settings["client"]["remote_path_map"] =
        serde_json::json!([{ "remote": "/downloads", "local": "staging" }]);
    let r = put(settings.to_string()).await;
    assert_eq!(
        r.status, 400,
        "a relative local path is refused: {}",
        r.body
    );
    let map = serde_json::json!([
        { "remote": "/downloads", "local": "/media/fat/mistarr/staging" },
        { "remote": "C:\\Torrents", "local": "/media/fat/mistarr/staging" }
    ]);
    settings["client"]["remote_path_map"] = map.clone();
    assert_eq!(put(settings.to_string()).await.status, 200);
    let saved = json_of(&booted, "/api/v1/system/settings").await;
    assert_eq!(saved["client"]["remote_path_map"], map);
    settings["client"]["remote_path_map"] = serde_json::json!([]);
    assert_eq!(put(settings.to_string()).await.status, 200);
    let saved = json_of(&booted, "/api/v1/system/settings").await;
    assert_eq!(saved["client"]["remote_path_map"], serde_json::json!([]));
    assert_eq!(json_of(&booted, "/api/v1/system/wizard").await, wizard);

    let r = request(addr, "POST", "/api/v1/system/wizard/done", &[], None).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["open_on_start"], false);
    assert_eq!(put(settings.to_string()).await.status, 200);
    let Booted { dir, running } = booted;
    running.shutdown().await.expect("shutdown");
    let config = config_in(dir.path());
    let again = boot_with(dir, config).await;
    let wizard = json_of(&again, "/api/v1/system/wizard").await;
    assert_eq!(
        wizard["open_on_start"], false,
        "dismissal survives a restart"
    );
    assert_eq!(wizard["dats"], false, "steps still report what is missing");
    again.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_transmission_runs_the_opt_in_service() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = config_in(dir.path());
    let booted = boot_with(dir, config).await;
    let root = booted.dir.path();
    let start = |kind: &'static str| {
        let addr = booted.addr();
        async move {
            let body = format!("{{\"kind\":\"{kind}\"}}");
            request(
                addr,
                "POST",
                "/api/v1/system/client/start",
                &[],
                Some(&body),
            )
            .await
        }
    };
    let r = start("transmission").await;
    assert_eq!(r.status, 400, "nothing installed: {}", r.body);
    let status = json_of(&booted, "/api/v1/system/status").await;
    assert_eq!(status["client"]["transmission_service"], false);

    let init = root.join("init.d");
    std::fs::create_dir_all(&init).expect("mkdir");
    let marker = root.join("started");
    let script = format!("#!/bin/sh\necho \"$1\" > '{}'\n", marker.display());
    std::fs::write(init.join("S92transmission"), script).expect("write");
    let mode = <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755);
    std::fs::set_permissions(init.join("S92transmission"), mode).expect("chmod");
    let r = start("transmission").await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(std::fs::read_to_string(&marker).expect("ran"), "start\n");
    assert!(root.join("linux/transmission").is_dir(), "opted in");
    let body = r.json();
    assert_eq!(body["client"]["transmission_service"], true);
    assert_eq!(body["client"]["transmission_opt_in"], true);
    assert_eq!(start("rtorrent").await.status, 400);

    let slow = format!(
        "#!/bin/sh\n/bin/sleep 1\necho \"$1\" > '{}'\n",
        marker.display()
    );
    std::fs::write(init.join("S92transmission"), slow).expect("write");
    let (a, b) = tokio::join!(start("transmission"), start("transmission"));
    let mut codes = [a.status, b.status];
    codes.sort_unstable();
    assert_eq!(codes, [200, 409], "{} / {}", a.body, b.body);
    let busy = if a.status == 409 { a } else { b };
    assert_eq!(busy.json()["error"]["code"], "busy");
    booted.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rejected_dat_can_be_retried_after_a_fix_or_deleted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = config_in(dir.path());
    let booted = boot_with(dir, config).await;
    let mut events = booted.running.app.events.subscribe(None).live;
    let dats = booted.dir.path().join("data/dats");
    drop_file(&dats, "fixed.dat", b"<softwarelist/>");
    drop_file(&dats, "junk.xml", b"<other/>");
    wait_event(&mut events, EventKind::DatRejected, "fixed.dat").await;
    wait_event(&mut events, EventKind::DatRejected, "junk.xml").await;

    let path = "/api/v1/dats/rejected/fixed.dat/retry";
    let refused = common::request_plain(booted.addr(), "POST", path, &[], None).await;
    assert_eq!(refused.status, 403, "{}", refused.body);
    std::fs::write(dats.join("rejected/fixed.dat"), no_intro_dat("1", 2)).expect("fix");
    let r = request(booted.addr(), "POST", path, &[], None).await;
    assert_eq!(r.status, 202, "{}", r.body);
    assert_eq!(r.json()["file"], "fixed.dat");
    assert!(r.json()["job_id"].is_number());
    wait_event(&mut events, EventKind::DatLoaded, "fixed.dat").await;
    assert!(dats.join("loaded/fixed.dat").is_file());
    assert!(!dats.join("rejected/fixed.dat.reason.txt").exists());

    let gone = request(booted.addr(), "POST", path, &[], None).await;
    assert_eq!(gone.status, 404, "{}", gone.body);
    let bad = request(
        booted.addr(),
        "DELETE",
        "/api/v1/dats/rejected/..%2Fx",
        &[],
        None,
    )
    .await;
    assert_eq!(bad.status, 400, "{}", bad.body);
    let del = "/api/v1/dats/rejected/junk.xml";
    let r = request(booted.addr(), "DELETE", del, &[], None).await;
    assert_eq!(r.status, 204, "{}", r.body);
    assert!(!dats.join("rejected/junk.xml").exists());
    assert!(!dats.join("rejected/junk.xml.reason.txt").exists());
    let listed = json_of(&booted, "/api/v1/dats/incoming").await;
    assert_eq!(listed["total"], 0, "{listed}");

    let loaded = json_of(&booted, "/api/v1/dats").await;
    let id = loaded["items"][0]["id"].as_i64().expect("id");
    let remove = format!("/api/v1/dats/{id}");
    let refused = common::request_plain(booted.addr(), "DELETE", &remove, &[], None).await;
    assert_eq!(refused.status, 403, "{}", refused.body);
    let r = request(booted.addr(), "DELETE", &remove, &[], None).await;
    assert_eq!(r.status, 204, "{}", r.body);
    let after = json_of(&booted, "/api/v1/dats").await;
    assert_eq!(after["items"][0]["retired"], true, "{after}");
    assert!(after["items"][0]["reason"].is_string(), "{after}");
    assert!(dats.join("loaded/fixed.dat").is_file(), "the file stays");
    let missing = request(booted.addr(), "DELETE", "/api/v1/dats/999", &[], None).await;
    assert_eq!(missing.status, 404, "{}", missing.body);
    booted.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn incoming_files_show_why_they_are_not_loaded() {
    let booted = boot_with_core(tempfile::tempdir().expect("tempdir")).await;
    let mut events = booted.running.app.events.subscribe(None).live;
    let data = booted.dir.path().join("data");
    drop_file(&data.join("dats"), "notes.txt", b"synthetic");
    wait_event(&mut events, EventKind::DatRejected, "notes.txt").await;
    drop_file(&data.join("sources"), "broken.torrent", b"not bencode");
    wait_event(&mut events, EventKind::JobProgress, "rejected").await;

    let dats = json_of(&booted, "/api/v1/dats/incoming").await;
    assert_eq!(dats["total"], 1, "{dats}");
    let item = &dats["items"][0];
    assert_eq!(item["file"], "notes.txt");
    assert_eq!(item["state"], "rejected");
    assert!(
        item["reason"]
            .as_str()
            .is_some_and(|r| r.contains("not a DAT")),
        "{item}"
    );
    let sources = json_of(&booted, "/api/v1/sources/incoming").await;
    let item = &sources["items"][0];
    assert_eq!(item["state"], "rejected", "{sources}");
    assert!(
        item["reason"]
            .as_str()
            .is_some_and(|r| r.contains(".torrent")),
        "{item}"
    );

    std::fs::write(data.join("dats/pending.dat"), b"<datafile>").expect("write");
    let dats = json_of(&booted, "/api/v1/dats/incoming?limit=1").await;
    assert_eq!(dats["total"], 2);
    assert_eq!(dats["items"][0]["file"], "pending.dat");
    assert!(matches!(
        dats["items"][0]["state"].as_str(),
        Some("waiting" | "importing")
    ));
    booted.running.shutdown().await.expect("shutdown");
}
