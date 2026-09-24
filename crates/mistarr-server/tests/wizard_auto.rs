//! Automatic scans: on a DAT finishing loading and once the wizard
//! completes. See `docs/ARCHITECTURE.md` "Library scan".

mod common;

use std::io::{Cursor, Write as _};
use std::net::SocketAddr;
use std::time::Duration;

use common::{boot_with, config_in, eventually, request, Booted};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

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

fn zip_of(members: &[(&str, &str)]) -> Vec<u8> {
    let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, body) in members {
        z.start_file(*name, zip::write::SimpleFileOptions::default())
            .expect("start");
        z.write_all(body.as_bytes()).expect("write");
    }
    z.finish().expect("finish").into_inner()
}

/// Posts one file to `/dats/upload` as `multipart/form-data`.
async fn upload_dat(addr: SocketAddr, name: &str, bytes: &[u8]) -> common::Response {
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
        "POST /api/v1/dats/upload HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\
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

/// `scan` jobs recorded so far (any state) whose payload is `want`.
async fn scan_job_count(b: &Booted, want: serde_json::Value) -> i64 {
    b.running
        .app
        .db
        .read(move |c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM jobs WHERE kind = 'scan' AND payload = ?1",
                [want.to_string()],
                |r| r.get(0),
            )?)
        })
        .await
        .expect("count")
}

async fn scan_count_for(b: &Booted, platform: &str) -> i64 {
    scan_job_count(b, serde_json::json!({ "platform_id": platform })).await
}

async fn full_scan_count(b: &Booted) -> i64 {
    scan_job_count(b, serde_json::json!({ "platform_id": null })).await
}

async fn dat_bound_to(addr: SocketAddr, platform: &str) -> bool {
    let dats = request(addr, "GET", "/api/v1/dats?limit=10&offset=0", &[], None)
        .await
        .json();
    dats["items"][0]["platform_id"] == platform
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dat_that_finishes_loading_queues_a_scan_for_its_platform_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let games = dir.path().join("games");
    std::fs::create_dir_all(games.join("GAMEBOY")).expect("mkdir");

    let mut config = config_in(dir.path());
    config.paths.games = games;
    let booted = boot_with(dir, config).await;
    let addr = booted.addr();

    // Two DAT members whose header names both bind to the platform `gb`: a
    // zipped pack must still queue only one scan for it.
    let a = dat(
        "Maker - Game Boy",
        "1",
        &[("Example Quest (USA)", &["a.gb"])],
    );
    let b = dat(
        "Second - Game Boy",
        "1",
        &[("Example Tale (USA)", &["b.gb"])],
    );
    let pack = zip_of(&[("a.dat", &a), ("b.dat", &b)]);
    let r = upload_dat(addr, "pack.zip", &pack).await;
    assert_eq!(r.status, 202, "{}", r.body);

    eventually("a scan queued for gb", || async {
        scan_count_for(&booted, "gb").await > 0
    })
    .await;
    // Give a lagging duplicate enqueue a chance to land before counting.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        scan_count_for(&booted, "gb").await,
        1,
        "one DAT pack for one platform queues one scan"
    );

    booted.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dat_platform_with_no_games_directory_queues_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let games = dir.path().join("games");
    let mut config = config_in(dir.path());
    config.paths.games = games;
    let booted = boot_with(dir, config).await;
    let addr = booted.addr();

    let xml = dat(
        "Maker - Game Boy",
        "1",
        &[("Example Quest (USA)", &["a.gb"])],
    );
    let r = upload_dat(addr, "a.dat", xml.as_bytes()).await;
    assert_eq!(r.status, 202, "{}", r.body);

    eventually("the dat to finish loading", || async {
        dat_bound_to(addr, "gb").await
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        scan_count_for(&booted, "gb").await,
        0,
        "the GB games directory does not exist yet"
    );

    booted.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wizard_completion_queues_one_full_scan() {
    let dir = tempfile::tempdir().expect("tempdir");
    let games = dir.path().join("games");
    std::fs::create_dir_all(&games).expect("mkdir");

    let mut config = config_in(dir.path());
    config.paths.games = games;
    let booted = boot_with(dir, config).await;
    let addr = booted.addr();

    // Client detection already ran at boot (config_in sets a client kind), and
    // the games directory exists: only `dats` and `sources` are missing.
    let w = request(addr, "GET", "/api/v1/system/wizard", &[], None)
        .await
        .json();
    assert_eq!(w["client"], true);
    assert_eq!(w["paths"], true);
    assert_eq!(w["dats"], false);
    assert_eq!(w["sources"], false);

    let xml = dat(
        "Maker - Game Boy",
        "1",
        &[("Example Quest (USA)", &["a.gb"])],
    );
    let r = upload_dat(addr, "a.dat", xml.as_bytes()).await;
    assert_eq!(r.status, 202, "{}", r.body);
    eventually("the dat to finish loading", || async {
        dat_bound_to(addr, "gb").await
    })
    .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        full_scan_count(&booted).await,
        0,
        "sources is still missing, so the wizard is not complete yet"
    );

    let magnet = "magnet:?xt=urn:btih:0000000000000000000000000000000000000000";
    let r = request(
        addr,
        "POST",
        "/api/v1/sources/upload",
        &[],
        Some(&format!(r#"{{"magnet":"{magnet}"}}"#)),
    )
    .await;
    assert_eq!(r.status, 202, "{}", r.body);

    eventually("a full scan once the wizard completes", || async {
        full_scan_count(&booted).await > 0
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(full_scan_count(&booted).await, 1, "fires only once");

    booted.running.shutdown().await.expect("shutdown");
}
