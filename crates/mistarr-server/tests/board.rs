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
