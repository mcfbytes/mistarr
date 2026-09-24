//! End-to-end tests of the assembled server over real sockets.

mod common;

use common::{boot, boot_with, config_in, eventually, get, request, Sse};
use mistarr_server::cli::Cli;
use mistarr_server::db::{self, migrate, Db};
use mistarr_server::events::EventKind;
use serde_json::json;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn migrations_apply_once_and_survive_restart() {
    let booted = boot().await;
    let db_path = booted.running.app.db.path().to_path_buf();
    let latest = migrate::MIGRATIONS.last().expect("migrations").version;
    let version = booted
        .running
        .app
        .db
        .read(migrate::current_version)
        .await
        .expect("version");
    assert_eq!(version, latest);
    let dir = booted.dir;
    booted.running.shutdown().await.expect("shutdown");

    let reopened = Db::open(&db_path).expect("reopen");
    let applied = reopened.write_blocking(migrate::apply).expect("apply");
    assert!(applied.is_empty());
    drop(reopened);

    let config = config_in(dir.path());
    let again = boot_with(dir, config).await;
    let r = get(again.addr(), "/api/v1/system/status").await;
    assert_eq!(r.status, 200);
    again.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_platform_is_seeded() {
    let booted = boot().await;
    let rows = booted
        .running
        .app
        .db
        .read(db::platforms::list)
        .await
        .expect("list");
    assert_eq!(rows.len(), mistarr_mister::platforms::PLATFORMS.len());
    for p in &mistarr_mister::platforms::PLATFORMS {
        let row = rows.iter().find(|r| r.id.0 == p.id).expect("row");
        assert_eq!(row.core_dir, p.core_dir);
        assert!(!row.core_present);
    }
    booted.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn installed_cores_are_marked_at_startup() {
    let dir = tempfile::tempdir().expect("tempdir");
    let console = dir.path().join("_Console");
    std::fs::create_dir_all(&console).expect("mkdir");
    std::fs::write(console.join("NES_20240101.rbf"), b"").expect("write");
    let config = config_in(dir.path());
    let booted = boot_with(dir, config).await;
    let rows = booted
        .running
        .app
        .db
        .read(db::platforms::list)
        .await
        .expect("list");
    assert!(rows.iter().any(|r| r.id.0 == "nes" && r.core_present));
    booted.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_has_the_documented_shape() {
    let booted = boot().await;
    let r = get(booted.addr(), "/api/v1/system/status").await;
    assert_eq!(r.status, 200);
    assert!(r
        .header("content-type")
        .is_some_and(|v| v.contains("application/json")));
    let s = r.json();
    assert_eq!(s["version"], env!("CARGO_PKG_VERSION"));
    assert!(s["uptime_secs"].is_u64());
    assert_eq!(s["client"]["kind"], "transmission");
    assert_eq!(s["client"]["reachable"], false);
    assert!(s["client"]["checked_at"].is_i64());
    assert!(s["client"].get("rtorrent_on_path").is_some());
    assert!(s["corename"].is_null());
    assert_eq!(s["paused"], false);
    assert!(s["pause_reason"].is_null());
    assert!(s["override"].is_null());
    assert!(s["disk_free_bytes"].is_u64());
    assert!(s["rss_bytes"].as_u64().is_some_and(|b| b > 0));
    booted.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_key_is_enforced_on_the_api_only() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = config_in(dir.path());
    config.server.api_key = "s3cret".into();
    let booted = boot_with(dir, config).await;
    let addr = booted.addr();

    let r = get(addr, "/api/v1/system/status").await;
    assert_eq!(r.status, 401);
    assert_eq!(r.json()["error"]["code"], "unauthorized");
    let r = request(
        addr,
        "GET",
        "/api/v1/system/status",
        &[("X-Api-Key", "wrong")],
        None,
    )
    .await;
    assert_eq!(r.status, 401);
    let r = request(
        addr,
        "GET",
        "/api/v1/system/status",
        &[("X-Api-Key", "s3cret")],
        None,
    )
    .await;
    assert_eq!(r.status, 200);
    let r = get(addr, "/api/v1/system/status?apikey=s3cret").await;
    assert_eq!(r.status, 200);
    let r = get(addr, "/").await;
    assert_eq!(r.status, 200);
    booted.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sse_replays_after_last_event_id_then_streams() {
    let booted = boot().await;
    let bus = &booted.running.app.events;
    let base = bus.latest_id();
    for i in 1..=3 {
        bus.publish(
            EventKind::DatLoaded,
            &json!({ "file": format!("f{i}.dat") }),
        );
    }
    let last = (base + 1).to_string();
    let mut sse = Sse::open(booted.addr(), "/api/v1/events", &[("Last-Event-ID", &last)]).await;
    sse.until(&format!("id: {}", base + 3)).await;
    sse.until("event: status").await;
    assert!(sse.text.contains("text/event-stream"));
    assert!(!sse.text.contains("f1.dat"));
    assert!(sse.text.contains("event: dat.loaded"));
    assert!(sse.text.contains("f2.dat") && sse.text.contains("f3.dat"));

    bus.publish(
        EventKind::SourceChanged,
        &json!({ "source_id": 7, "state": "bound" }),
    );
    sse.until("event: source.changed").await;
    assert!(sse.text.contains(&format!("id: {}", base + 4)));
    booted.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sse_without_last_event_id_gets_status_then_live_events() {
    let booted = boot().await;
    booted
        .running
        .app
        .events
        .publish(EventKind::DatRejected, &json!({ "file": "old.dat" }));
    let mut sse = Sse::open(booted.addr(), "/api/v1/events", &[]).await;
    sse.until("event: status").await;
    assert!(!sse.text.contains("old.dat"));
    booted.running.app.events.publish(
        EventKind::ImportDone,
        &json!({ "title_id": 1, "file_id": 2, "action": "placed" }),
    );
    sse.until("event: import.done").await;
    booted.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pause_and_resume_override_corename() {
    let booted = boot().await;
    let addr = booted.addr();
    std::fs::write(booted.corename(), "MENU\n").expect("write");
    eventually("corename read", || async {
        get(addr, "/api/v1/system/status").await.json()["corename"] == "MENU"
    })
    .await;

    let r = request(addr, "POST", "/api/v1/system/pause", &[], None).await;
    assert_eq!(r.status, 200);
    let s = r.json();
    assert_eq!(
        (s["paused"].clone(), s["pause_reason"].clone()),
        (json!(true), json!("manual"))
    );
    assert_eq!(s["override"], "paused");

    let r = request(addr, "POST", "/api/v1/system/resume", &[], None).await;
    let s = r.json();
    assert_eq!(s["paused"], false);
    assert_eq!(s["override"], "running");

    std::fs::write(booted.corename(), "SNES\n").expect("write");
    eventually("core pause", || async {
        let s = get(addr, "/api/v1/system/status").await.json();
        s["paused"] == true && s["pause_reason"] == "core" && s["override"].is_null()
    })
    .await;
    let r = request(addr, "POST", "/api/v1/system/resume", &[], None).await;
    assert_eq!(r.json()["paused"], false);
    std::fs::write(booted.corename(), "MENU\n").expect("write");
    eventually("menu resume", || async {
        let s = get(addr, "/api/v1/system/status").await.json();
        s["paused"] == false && s["override"].is_null()
    })
    .await;
    booted.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gate_changes_are_published_as_status() {
    let booted = boot().await;
    let mut sse = Sse::open(booted.addr(), "/api/v1/events", &[]).await;
    sse.until("event: status").await;
    let r = request(booted.addr(), "POST", "/api/v1/system/pause", &[], None).await;
    assert_eq!(r.status, 200);
    sse.until(r#""pause_reason":"manual""#).await;
    booted.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_from_file_are_editable_and_persist() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).expect("mkdir");
    let text = format!(
        "[server]\nlisten = \"127.0.0.1:1\"\n[paths]\nroot = {root:?}\ngames = {games:?}\n\
         [client]\nkind = \"transmission\"\nurl = {url:?}\n[limits]\ndown_kbps_core = 100\n",
        root = dir.path(),
        games = dir.path().join("games"),
        url = common::closed_url(),
    );
    std::fs::write(data.join("mistarr.toml"), text).expect("write");
    let data_arg = data.to_string_lossy().into_owned();
    let cli = <Cli as clap::Parser>::try_parse_from([
        "mistarr",
        "--data",
        &data_arg,
        "--listen",
        "127.0.0.1:0",
    ])
    .expect("cli");
    let config = cli.config().expect("config");
    assert_eq!(config.server.listen, "127.0.0.1:0");
    assert_eq!(config.limits.down_kbps_core, 100);
    assert_eq!(config.limits.up_kbps_core, 64);

    let booted = boot_with(dir, config.clone()).await;
    let addr = booted.addr();
    let r = get(addr, "/api/v1/system/settings").await;
    assert_eq!(r.status, 200);
    let s = r.json();
    assert_eq!(s["limits"]["down_kbps_core"], 100);
    assert_eq!(s["prefs"]["regions"][0], "USA");
    assert_eq!(s["client"]["kind"], "transmission");

    let body =
        r#"{"limits":{"down_kbps_core":5,"up_kbps_core":6,"down_kbps_menu":0,"up_kbps_menu":0}}"#;
    let r = request(addr, "PUT", "/api/v1/system/settings", &[], Some(body)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["limits"]["up_kbps_core"], 6);
    let r = request(
        addr,
        "PUT",
        "/api/v1/system/settings",
        &[],
        Some(r#"{"server":{}}"#),
    )
    .await;
    assert_eq!(r.status, 400);
    assert_eq!(r.json()["error"]["code"], "bad_request");

    let dir = booted.dir;
    booted.running.shutdown().await.expect("shutdown");
    let again = boot_with(dir, config).await;
    let s = get(again.addr(), "/api/v1/system/settings").await.json();
    assert_eq!(s["limits"]["down_kbps_core"], 5);
    again.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wizard_and_jobs_report_state() {
    let booted = boot().await;
    let addr = booted.addr();
    let w = get(addr, "/api/v1/system/wizard").await.json();
    assert_eq!(w["dats"], false);
    assert_eq!(w["sources"], false);
    assert_eq!(w["client"], true);
    assert_eq!(w["open_on_start"], true);
    assert!(w["paths"].is_boolean());

    let jobs = get(addr, "/api/v1/system/jobs?limit=10&offset=0")
        .await
        .json();
    assert_eq!(jobs["total"], 0);
    assert_eq!(jobs["items"], json!([]));
    let r = get(addr, "/api/v1/system/jobs?limit=x").await;
    assert_eq!(r.status, 400);

    let body = r#"{"client":{"kind":"rtorrent","url":"127.0.0.1:1"}}"#;
    let r = request(addr, "PUT", "/api/v1/system/settings", &[], Some(body)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    eventually("re-detection", || async {
        get(addr, "/api/v1/system/status").await.json()["client"]["kind"] == "rtorrent"
    })
    .await;
    booted.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unimplemented_routes_answer_501_and_unknown_ones_404() {
    let booted = boot().await;
    let addr = booted.addr();
    for (method, path) in [
        ("GET", "/api/v1/platforms"),
        ("PUT", "/api/v1/platforms/nes"),
        ("POST", "/api/v1/titles/1/want"),
        ("DELETE", "/api/v1/titles/1/want"),
        ("POST", "/api/v1/system/scan"),
        ("GET", "/api/v1/imports"),
    ] {
        let r = request(addr, method, path, &[], Some("{}")).await;
        assert_eq!(r.status, 501, "{method} {path}");
        assert_eq!(r.json()["error"]["code"], "not_implemented");
    }
    let r = get(addr, "/api/v1/nope").await;
    assert_eq!(r.status, 404);
    assert_eq!(r.json()["error"]["code"], "not_found");
    let r = request(addr, "DELETE", "/api/v1/system/status", &[], None).await;
    assert_eq!(r.status, 405);
    assert_eq!(r.json()["error"]["code"], "method_not_allowed");
    let r = get(addr, "/api/other").await;
    assert_eq!(r.status, 404);

    let r = get(addr, "/p/nes").await;
    assert_eq!(r.status, 200);
    assert!(r
        .header("content-type")
        .is_some_and(|v| v.starts_with("text/html")));
    assert!(r.body.contains("mistarr"));
    booted.running.shutdown().await.expect("shutdown");
}
