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

async fn download_of(b: &Booted, title: i64) -> Value {
    let r = get(b.addr(), "/api/v1/downloads").await.json();
    r["items"]
        .as_array()
        .and_then(|items| items.iter().find(|d| d["title_id"] == title))
        .cloned()
        .unwrap_or_else(|| panic!("no download of {title}: {r}"))
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

/// The Alt is placed and verified, the parent's download names it, and
/// `nova.nes` is no longer offered for the parent.
async fn assert_alt_placed(b: &Booted, w: &Wanted) {
    eventually("both downloads settle", || async {
        let (p, a) = (download_of(b, w.parent).await, download_of(b, w.alt).await);
        p["state"] == "bad" && a["state"] == "done"
    })
    .await;
    let p = download_of(b, w.parent).await;
    assert_eq!(
        p["error"],
        format!("The file in this source is a different version: {ALT}.")
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
