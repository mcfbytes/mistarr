//! Fuzzy torrent-to-rom mapping end to end: a DAT with two versions of one
//! homebrew entry, a torrent whose only rom has a short name, want, import.

mod common;

use std::io::Cursor;

use common::{boot, eventually, get, request, Booted};
use mistarr_clients::InfoHash;
use mistarr_core::hash::{hash_reader, HeaderRule};
use mistarr_server::db::downloads::DownloadId;
use mistarr_server::events::EventKind;
use serde_json::{json, Value};

const SIZE: usize = 262_160;
const PARENT: &str = "Nova the Squirrel (World) (v1.0.6) (Homebrew)";
const ALT: &str = "Nova the Squirrel (World) (v1.0.6) (Homebrew) (Alt)";
const TORRENT: &str = "novathesquirrel1.0.6";
/// The nine files of the torrent; `nova.nes` is the only rom.
const FILES: [(&str, usize); 9] = [
    ("cover.png", SIZE),
    ("nova.nes", SIZE),
    ("shot1.png", 4_096),
    ("shot2.png", 4_100),
    ("box.jpg", 8_192),
    ("nova-src.tar.gz", 90_000),
    ("levels.sqlite", 12_288),
    ("manual.xml", 2_048),
    ("credits.png", 1_024),
];

/// Synthetic bytes of one version: an iNES header and a pseudo-random body.
fn version(seed: u8) -> Vec<u8> {
    let mut out = b"NES\x1a".to_vec();
    out.extend_from_slice(&[16, 0, 1, 0]);
    out.resize(16, 0);
    out.extend(
        (16..SIZE).map(|i| u8::try_from((i * 7 + usize::from(seed) * 13) % 251).unwrap_or(0)),
    );
    out
}

fn rom_xml(name: &str, data: &[u8]) -> String {
    let h = hash_reader(Cursor::new(data), HeaderRule::Ines, None).expect("hash");
    format!(
        "<game name=\"{name}\"><description>{name}</description>\
         <rom name=\"{name}.nes\" size=\"{}\" crc=\"{}\" md5=\"{}\" sha1=\"{}\"/></game>\n",
        data.len(),
        h.crc32,
        h.md5,
        h.sha1
    )
}

/// Both entries list the size of the file with its header and the hashes the iNES rule gives.
fn dat() -> String {
    let nes = mistarr_fixture::dat::platform("nes").expect("nes");
    let header = mistarr_fixture::dat::header_name(nes, "Homebrew Test");
    format!(
        "<?xml version=\"1.0\"?>\n<datafile><header><name>{header}</name>\
         <description>{header}</description><version>1</version></header>\n{}{}</datafile>\n",
        rom_xml(PARENT, &version(1)),
        rom_xml(ALT, &version(2)),
    )
}

fn bstr(s: &str) -> String {
    format!("{}:{s}", s.len())
}

fn torrent() -> Vec<u8> {
    let mut list = String::from("l");
    for (file, len) in FILES {
        list.push_str(&format!("d6:lengthi{len}e4:pathl{}ee", bstr(file)));
    }
    list.push('e');
    format!(
        "d4:infod5:files{list}4:name{}12:piece lengthi16384e6:pieces0:ee",
        bstr(TORRENT)
    )
    .into_bytes()
}

fn title_id(b: &Booted, name: &str) -> i64 {
    let name = name.to_owned();
    b.running
        .app
        .db
        .read_blocking(move |c| {
            Ok(
                c.query_row("SELECT id FROM titles WHERE name = ?1", [&name], |r| {
                    r.get(0)
                })?,
            )
        })
        .expect("title")
}

async fn detail(b: &Booted, title: i64) -> Value {
    let r = get(b.addr(), &format!("/api/v1/titles/{title}")).await;
    assert_eq!(r.status, 200, "{}", r.body);
    r.json()
}

fn variant(detail: &Value, title: i64) -> Value {
    detail["variants"]
        .as_array()
        .and_then(|v| v.iter().find(|v| v["id"] == title))
        .cloned()
        .unwrap_or_else(|| panic!("variant {title} in {detail}"))
}

async fn want(b: &Booted, title: i64) {
    let body = json!({ "variant_id": title }).to_string();
    let path = format!("/api/v1/titles/{title}/want");
    let r = request(b.addr(), "POST", &path, &[], Some(&body)).await;
    assert_eq!(r.status, 200, "{}", r.body);
}

async fn downloads_of(b: &Booted, title: i64) -> Vec<Value> {
    let r = get(b.addr(), "/api/v1/downloads").await.json();
    let mut out: Vec<Value> = r["items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter(|d| d["title_id"] == title)
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    out.sort_by_key(|d| d["id"].as_i64());
    out
}

/// The newest download of `title`.
async fn download_of(b: &Booted, title: i64) -> Value {
    downloads_of(b, title)
        .await
        .pop()
        .unwrap_or_else(|| panic!("no download of {title}"))
}

fn reason() -> String {
    format!("the file in this source is a different version: {ALT}")
}

/// The actions the import log holds for download `id`.
fn logged(b: &Booted, id: i64) -> Vec<String> {
    b.running
        .app
        .db
        .read_blocking(move |c| {
            let mut stmt =
                c.prepare("SELECT action FROM import_log WHERE download_id = ?1 ORDER BY id")?;
            let rows = stmt.query_map([id], |r| r.get(0))?;
            Ok(rows.collect::<rusqlite::Result<Vec<String>>>()?)
        })
        .expect("log")
}

/// Points a `wanted` download at `nova.nes` again, as a retry would.
fn onto_nova(b: &Booted, id: i64) {
    b.running
        .app
        .db
        .write_blocking(move |c| {
            c.execute(
                "UPDATE downloads SET source_id = 1, file_index = 1 WHERE id = ?1",
                [id],
            )?;
            Ok(())
        })
        .expect("retarget");
}

/// Moves a download to `importing` with `staged` and announces it, as the poller does.
fn hand_off(b: &Booted, id: i64, staged: &str) {
    let staged = staged.to_owned();
    b.running
        .app
        .db
        .write_blocking(move |c| {
            c.execute(
                "UPDATE downloads SET state = 'importing', progress = 1, staged_path = ?2
                 WHERE id = ?1",
                rusqlite::params![id, staged],
            )?;
            Ok(())
        })
        .expect("importing");
    b.running.app.events.publish(
        EventKind::DownloadChanged,
        &json!({ "download_id": DownloadId(id).0, "state": "importing", "progress": 1.0 }),
    );
}

/// The Nova titles, and the two queued downloads on `nova.nes` staged with the Alt bytes.
struct Wanted {
    parent: i64,
    alt: i64,
    parent_download: i64,
    alt_download: i64,
    staged: String,
}

/// Loads the DAT and the torrent, binds the source to NES as a user would,
/// checks both titles are offered `nova.nes`, and wants both.
async fn both_wanted(b: &Booted) -> Wanted {
    let config = b.running.app.config();
    std::fs::create_dir_all(config.paths.dats()).expect("dats");
    std::fs::write(config.paths.dats().join("homebrew.dat"), dat()).expect("dat");
    let app = &b.running.app;
    eventually("the DAT loads", || async {
        app.db
            .read(|c| {
                Ok(c.query_row(
                    "SELECT COUNT(*) FROM titles WHERE platform_id = 'nes' AND retired = 0",
                    [],
                    |r| r.get::<_, i64>(0),
                )?)
            })
            .await
            .expect("count")
            == 2
    })
    .await;
    let (parent, alt) = (title_id(b, PARENT), title_id(b, ALT));

    std::fs::create_dir_all(config.paths.sources()).expect("sources");
    std::fs::write(config.paths.sources().join("nova.torrent"), torrent()).expect("torrent");
    eventually("the source loads", || async {
        let r = get(b.addr(), "/api/v1/sources").await.json();
        r["items"][0]["state"] == "unbound"
    })
    .await;
    let body = Some(r#"{"platform_id":"nes"}"#);
    let r = request(b.addr(), "PUT", "/api/v1/sources/1", &[], body).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let s = r.json();
    assert_eq!(
        (s["state"].clone(), s["bind_score"].clone()),
        (json!("bound"), json!(0.0))
    );
    assert_eq!(s["matched_count"], 1, "only nova.nes: {s}");

    let d = detail(b, parent).await;
    for title in [parent, alt] {
        let v = variant(&d, title);
        assert_eq!(v["torrent_files_available"], 1, "{v}");
        let a = &v["availability"][0];
        assert_eq!(
            (
                a["path"].clone(),
                a["source_name"].clone(),
                a["confidence"].clone()
            ),
            (json!("nova.nes"), json!(TORRENT), json!("fuzzy"))
        );
    }

    want(b, parent).await;
    want(b, alt).await;
    let (pd, ad) = (download_of(b, parent).await, download_of(b, alt).await);
    for d in [&pd, &ad] {
        assert_eq!(
            (d["file_index"].clone(), d["state"].clone()),
            (json!(1), json!("queued"))
        );
    }

    let meta = mistarr_sources::torrent::parse_torrent(&torrent()).expect("parse");
    let staged = config
        .paths
        .staging()
        .join(InfoHash::from_bytes(meta.infohash).to_string())
        .join(TORRENT)
        .join("nova.nes");
    std::fs::create_dir_all(staged.parent().expect("parent")).expect("mkdir");
    std::fs::write(&staged, version(2)).expect("stage");
    Wanted {
        parent,
        alt,
        parent_download: pd["id"].as_i64().expect("id"),
        alt_download: ad["id"].as_i64().expect("id"),
        staged: staged.to_string_lossy().into_owned(),
    }
}

/// The Alt is placed and verified, the parent's download names it, the
/// parent is wanted again with that history, and `nova.nes` is no longer
/// offered for the parent.
async fn assert_alt_placed(b: &Booted, w: &Wanted) {
    eventually("both downloads settle", || async {
        let p = downloads_of(b, w.parent).await;
        let a = download_of(b, w.alt).await;
        p.len() == 2 && p[0]["state"] == "bad" && a["state"] == "done"
    })
    .await;
    let p = downloads_of(b, w.parent).await;
    assert_eq!(p[0]["id"], w.parent_download);
    assert_eq!(p[0]["error"], reason());
    assert_eq!(
        (p[1]["state"].clone(), p[1]["error"].clone()),
        (json!("wanted"), json!(reason())),
        "the parent is wanted again: {}",
        p[1]
    );
    let games = b.running.app.config().paths.games;
    let placed = games.join(format!("NES/{ALT}.nes"));
    assert_eq!(std::fs::read(&placed).expect("placed"), version(2));
    assert!(!games.join(format!("NES/{PARENT}.nes")).exists());

    let d = detail(b, w.parent).await;
    let a = variant(&d, w.alt);
    assert_eq!(a["roms"][0]["file_state"], "verified", "{a}");
    let pv = variant(&d, w.parent);
    assert_eq!(pv["roms"][0]["file_state"], Value::Null, "{pv}");
    assert_eq!(
        pv["torrent_files_available"], 0,
        "the pair is dropped: {pv}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_parent_download_places_the_alt_it_holds() {
    let b = boot().await;
    let w = both_wanted(&b).await;
    hand_off(&b, w.parent_download, &w.staged);
    hand_off(&b, w.alt_download, &w.staged);
    assert_alt_placed(&b, &w).await;
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_parent_download_after_the_alt_landed_says_so() {
    let b = boot().await;
    let w = both_wanted(&b).await;
    hand_off(&b, w.alt_download, &w.staged);
    eventually("the Alt lands", || async {
        download_of(&b, w.alt).await["state"] == "done"
    })
    .await;
    hand_off(&b, w.parent_download, &w.staged);
    assert_alt_placed(&b, &w).await;
    b.running.shutdown().await.expect("shutdown");
}

/// Stages the Alt bytes on `nova.nes` again and hands the parent's newest
/// download off to the importer on that file.
async fn stage_again(b: &Booted, w: &Wanted, write: bool) -> i64 {
    if write {
        let staged = std::path::Path::new(&w.staged);
        std::fs::create_dir_all(staged.parent().expect("parent")).expect("mkdir");
        std::fs::write(staged, version(2)).expect("stage");
    }
    let again = download_of(b, w.parent).await["id"].as_i64().expect("id");
    onto_nova(b, again);
    hand_off(b, again, &w.staged);
    again
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn later_downloads_of_the_file_see_the_version_it_is() {
    let b = boot().await;
    let w = both_wanted(&b).await;
    hand_off(&b, w.parent_download, &w.staged);
    hand_off(&b, w.alt_download, &w.staged);
    assert_alt_placed(&b, &w).await;

    // The staged file is gone: the bad download's placed log says what it was.
    let third = stage_again(&b, &w, false).await;
    eventually("the third download settles", || async {
        downloads_of(&b, w.parent).await.len() == 3
    })
    .await;
    let p = downloads_of(&b, w.parent).await;
    assert_eq!(
        (p[1]["id"].as_i64(), p[1]["error"].clone()),
        (Some(third), json!(reason()))
    );
    assert_eq!(p[1]["state"], "bad");
    assert_eq!(p[2]["state"], "wanted");

    // Staged anew, the Alt the library holds is kept, not written twice.
    let fourth = stage_again(&b, &w, true).await;
    eventually("the fourth download settles", || async {
        downloads_of(&b, w.parent).await.len() == 4
    })
    .await;
    let p = downloads_of(&b, w.parent).await;
    assert_eq!(p[2]["id"].as_i64(), Some(fourth));
    assert_eq!(
        (p[2]["state"].clone(), p[2]["error"].clone()),
        (json!("bad"), json!(reason()))
    );
    assert_eq!(logged(&b, fourth), ["skipped_existing"]);
    assert!(
        std::path::Path::new(&w.staged).exists(),
        "the staged copy stays"
    );
    let games = b.running.app.config().paths.games.join("NES");
    let placed: Vec<_> = std::fs::read_dir(&games)
        .expect("games")
        .filter_map(|e| e.ok().map(|e| e.file_name()))
        .collect();
    assert_eq!(placed.len(), 1, "{placed:?}");
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unverified_copy_of_the_version_is_never_replaced() {
    let b = boot().await;
    let w = both_wanted(&b).await;
    let games = b.running.app.config().paths.games.join("NES");
    std::fs::create_dir_all(&games).expect("games");
    let copy = games.join(format!("{ALT}.nes"));
    std::fs::write(&copy, b"unverified").expect("copy");
    hand_off(&b, w.parent_download, &w.staged);
    eventually("the parent settles", || async {
        downloads_of(&b, w.parent).await.len() == 2
    })
    .await;
    let p = downloads_of(&b, w.parent).await;
    assert_eq!(p[0]["state"], "bad");
    let error = p[0]["error"].as_str().unwrap_or_default().to_owned();
    assert!(error.starts_with(&reason()), "{error}");
    assert!(error.contains("quarantined"), "{error}");
    assert_eq!(p[1]["state"], "wanted");
    assert_eq!(logged(&b, w.parent_download), ["quarantined"]);
    assert_eq!(std::fs::read(&copy).expect("copy"), b"unverified");
    assert!(
        !std::path::Path::new(&w.staged).exists(),
        "moved to quarantine"
    );
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_guessed_file_that_is_no_version_wants_the_rom_again() {
    let b = boot().await;
    let w = both_wanted(&b).await;
    std::fs::write(&w.staged, version(9)).expect("stage");
    hand_off(&b, w.parent_download, &w.staged);
    eventually("the parent settles", || async {
        downloads_of(&b, w.parent).await.len() == 2
    })
    .await;
    let p = downloads_of(&b, w.parent).await;
    assert_eq!(
        (p[0]["state"].clone(), p[1]["state"].clone()),
        (json!("bad"), json!("wanted"))
    );
    assert_eq!(p[1]["error"], p[0]["error"], "the history carries over");
    assert_eq!(logged(&b, w.parent_download), ["quarantined"]);
    b.running.shutdown().await.expect("shutdown");
}
