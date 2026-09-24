//! The arcade catalogue from MRA files and the Neo Geo romset report, over real sockets.

mod common;

use std::io::Write as _;
use std::net::SocketAddr;
use std::path::Path;

use common::{boot_with, config_in, eventually, get, request, Booted};
use mistarr_core::hash::Md5Stream;
use serde_json::Value;

fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, bytes).expect("write");
}

fn write_zip(path: &Path, members: &[(&str, &[u8])]) {
    let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, body) in members {
        z.start_file(*name, zip::write::SimpleFileOptions::default())
            .expect("start");
        z.write_all(body).expect("write");
    }
    write(path, &z.finish().expect("finish").into_inner());
}

fn md5_of(parts: &[&[u8]]) -> String {
    let mut m = Md5Stream::new();
    for p in parts {
        m.update(p);
    }
    m.finish()
}

fn mra(name: &str, setname: &str, roms: &str) -> Vec<u8> {
    format!(
        "<?xml version=\"1.0\"?>\n<misterromdescription>\n  <name>{name}</name>\n  \
         <setname>{setname}</setname>\n  <rbf>excore</rbf>\n{roms}\n</misterromdescription>\n"
    )
    .into_bytes()
}

async fn json_of(addr: SocketAddr, path: &str) -> Value {
    let r = get(addr, path).await;
    assert_eq!(r.status, 200, "{path}: {}", r.body);
    r.json()
}

async fn post(addr: SocketAddr, path: &str, body: &str) -> Value {
    let r = request(addr, "POST", path, &[], Some(body)).await;
    assert_eq!(r.status, 200, "{path}: {}", r.body);
    r.json()
}

/// Browse rows of the arcade platform as `(name, have_verified)`.
async fn arcade_rows(addr: SocketAddr) -> Vec<(String, u64)> {
    json_of(addr, "/api/v1/platforms/arcade/titles").await["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|i| {
            (
                i["name"].as_str().expect("name").to_owned(),
                i["have_verified"].as_u64().expect("have"),
            )
        })
        .collect()
}

async fn wait_rows(addr: SocketAddr, want: &[(&str, u64)]) {
    let want: Vec<(String, u64)> = want.iter().map(|(n, h)| ((*n).to_owned(), *h)).collect();
    let mut last = Vec::new();
    for _ in 0..250 {
        last = arcade_rows(addr).await;
        if last == want {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("arcade rows stayed {last:?}, wanted {want:?}");
}

fn variant<'a>(detail: &'a Value, name: &str) -> &'a Value {
    detail["variants"]
        .as_array()
        .expect("variants")
        .iter()
        .find(|v| v["name"] == name)
        .unwrap_or_else(|| panic!("no variant {name} in {detail}"))
}

async fn detail_of(addr: SocketAddr, platform: &str, name: &str) -> Value {
    let page = json_of(
        addr,
        &format!("/api/v1/platforms/{platform}/titles?limit=1000"),
    )
    .await;
    let id = page["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|i| i["name"] == name)
        .unwrap_or_else(|| panic!("no {name} in {page}"))["parent_id"]
        .as_i64()
        .expect("id");
    json_of(addr, &format!("/api/v1/titles/{id}")).await
}

/// An `_Arcade` tree with a plain MRA, an interleaved one in `_alternatives`,
/// one with no md5 and one reading from `games/hbmame`.
fn arcade_tree(root: &Path) {
    let arcade = root.join("_Arcade");
    let games = root.join("games");
    write(&arcade.join("cores/excore_20240101.rbf"), b"");
    let blaster_md5 = md5_of(&[b"CPU0", b"SND", b"\x00\x01"]);
    write(
        &arcade.join("Example Blaster.mra"),
        &mra(
            "Example Blaster",
            "exblast",
            &format!(
                r#"<rom index="0" zip="exblast.zip|exparent.zip" md5="{blaster_md5}">
                     <part name="cpu.bin" offset="0x0" length="4"/>
                     <part name="snd.bin"/>
                     <part>00 01</part>
                   </rom>"#
            ),
        ),
    );
    let alt_md5 = md5_of(&[b"ACEG", b"BDFH"]);
    write(
        &arcade.join("_alternatives/_Example Blaster/Example Blaster (set 2).mra"),
        &mra(
            "Example Blaster (set 2)",
            "exblast2",
            &format!(
                r#"<rom index="0" zip="exblast2.zip|exparent.zip" md5="{alt_md5}">
                     <interleave output="16">
                       <part name="even.bin" map="01"/>
                       <part name="odd.bin" map="10"/>
                     </interleave>
                     <patch offset="0">FF</patch>
                   </rom>"#
            ),
        ),
    );
    write(
        &arcade.join("Example Quest.mra"),
        &mra(
            "Example Quest",
            "exquest",
            r#"<rom index="0" zip="exquest.zip"><part name="q.bin"/></rom>"#,
        ),
    );
    write(
        &arcade.join("Example Homebrew.mra"),
        &mra(
            "Example Homebrew",
            "exhb",
            r#"<rom index="0" zip="/hbmame/exhb.zip"><part name="h.bin"/></rom>"#,
        ),
    );
    write_zip(
        &games.join("mame/exblast.zip"),
        &[("cpu.bin", b"CPU0more"), ("snd.bin", b"SND")],
    );
    write_zip(&games.join("mame/exparent.zip"), &[("x.bin", b"X")]);
    write_zip(
        &games.join("mame/exblast2.zip"),
        &[("even.bin", b"ACEG"), ("odd.bin", b"BDFX")],
    );
    write_zip(&games.join("hbmame/exhb.zip"), &[("h.bin", b"H")]);
}

async fn boot_arcade() -> Booted {
    let dir = tempfile::tempdir().expect("tempdir");
    arcade_tree(dir.path());
    let config = config_in(dir.path());
    boot_with(dir, config).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mra_files_become_the_arcade_catalogue() {
    let b = boot_arcade().await;
    let addr = b.addr();
    wait_rows(
        addr,
        &[
            ("Example Blaster", 1),
            ("Example Homebrew", 1),
            ("Example Quest", 0),
        ],
    )
    .await;

    let blaster = detail_of(addr, "arcade", "Example Blaster").await;
    assert_eq!(blaster["variants"].as_array().map(Vec::len), Some(2));
    let v = variant(&blaster, "Example Blaster");
    assert_eq!(v["source"], "mra");
    assert_eq!(v["mra"]["setname"], "exblast");
    assert_eq!(v["mra"]["rbf"], "excore");
    assert_eq!(v["mra"]["path"], "Example Blaster.mra");
    assert_eq!(v["mra"]["md5_check"], "match");
    assert_eq!(v["mra"]["missing_zips"], serde_json::json!([]));
    let roms: Vec<&str> = v["roms"]
        .as_array()
        .expect("roms")
        .iter()
        .map(|r| r["name"].as_str().expect("name"))
        .collect();
    assert_eq!(roms, ["exblast.zip", "exparent.zip"]);
    assert!(v["roms"][0]["md5"].is_string());

    let v = variant(&blaster, "Example Blaster (set 2)");
    assert_eq!(v["mra"]["md5_check"], "mismatch");
    assert!(v["mra"]["path"]
        .as_str()
        .is_some_and(|p| p.starts_with("_alternatives/")));

    let quest = detail_of(addr, "arcade", "Example Quest").await;
    let v = &quest["variants"][0];
    assert_eq!(
        v["mra"]["missing_zips"],
        serde_json::json!(["mame/exquest.zip"])
    );
    assert!(v["mra"]["md5_check"].is_null());
    assert!(v["roms"][0]["md5"].is_null());

    let platforms = json_of(addr, "/api/v1/platforms").await;
    let arcade = platforms["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|p| p["id"] == "arcade")
        .expect("arcade")
        .clone();
    assert_eq!(arcade["core_present"], true);
    assert_eq!(arcade["counts"]["titles"], 3);
    assert_eq!(arcade["counts"]["have"], 2);

    let dats = json_of(addr, "/api/v1/dats").await;
    assert_eq!(dats["total"], 0);
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rescans_follow_mra_and_zip_changes() {
    let b = boot_arcade().await;
    let addr = b.addr();
    let root = b.dir.path().to_path_buf();
    eventually("the first catalogue", || async {
        arcade_rows(addr).await.len() == 3
    })
    .await;
    let set2 = |d: &Value| variant(d, "Example Blaster (set 2)")["mra"]["md5_check"].clone();
    assert_eq!(
        set2(&detail_of(addr, "arcade", "Example Blaster").await),
        "mismatch"
    );
    let quest_id = detail_of(addr, "arcade", "Example Quest").await["parent_id"].clone();

    std::fs::remove_file(root.join("_Arcade/Example Quest.mra")).expect("rm");
    write_zip(
        &root.join("games/mame/exblast2.zip"),
        &[("even.bin", b"ACEG"), ("odd.bin", b"BDFH")],
    );
    let r = post(addr, "/api/v1/system/scan", r#"{"platform_id":"arcade"}"#).await;
    assert!(r["arcade_job_id"].is_i64(), "{r}");
    wait_rows(addr, &[("Example Blaster", 2), ("Example Homebrew", 1)]).await;
    eventually("the set 2 recheck", || async {
        set2(&detail_of(addr, "arcade", "Example Blaster").await) == "match"
    })
    .await;
    let quest = json_of(addr, &format!("/api/v1/titles/{quest_id}")).await;
    assert_eq!(quest["variants"][0]["retired"], true);

    write(
        &root.join("_Arcade/Example Quest.mra"),
        &mra(
            "Example Quest",
            "exquest",
            r#"<rom index="0" zip="exquest.zip"><part name="q.bin"/></rom>"#,
        ),
    );
    write_zip(&root.join("games/mame/exquest.zip"), &[("q.bin", b"Q")]);
    let r = post(addr, "/api/v1/system/cores", "").await;
    assert!(r["platforms"]
        .as_array()
        .expect("platforms")
        .iter()
        .any(|p| p == "arcade"));
    assert!(r["arcade_job_id"].is_i64(), "{r}");
    wait_rows(
        addr,
        &[
            ("Example Blaster", 2),
            ("Example Homebrew", 1),
            ("Example Quest", 1),
        ],
    )
    .await;
    let again = json_of(addr, &format!("/api/v1/titles/{quest_id}")).await;
    assert_eq!(again["variants"][0]["retired"], false);

    let other = post(addr, "/api/v1/system/scan", r#"{"platform_id":"nes"}"#).await;
    assert!(other.get("arcade_job_id").is_none());
    b.running.shutdown().await.expect("shutdown");
}

/// A manual scan of `arcade` queues only the catalogue: no library scan walks
/// `games/mame` as if every zip were a cartridge, and every arcade row has a rom.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_manual_arcade_scan_queues_only_the_catalogue() {
    let b = boot_arcade().await;
    let addr = b.addr();
    eventually("the first catalogue", || async {
        arcade_rows(addr).await.len() == 3
    })
    .await;

    let r = post(addr, "/api/v1/system/scan", r#"{"platform_id":"arcade"}"#).await;
    assert!(r["job_id"].is_null(), "{r}");
    assert!(r["arcade_job_id"].is_i64(), "{r}");

    let scan_jobs: i64 = b
        .running
        .app
        .db
        .read_blocking(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM jobs WHERE kind = 'scan' AND payload = ?1",
                [serde_json::json!({ "platform_id": "arcade" }).to_string()],
                |r| r.get(0),
            )?)
        })
        .expect("count");
    assert_eq!(
        scan_jobs, 0,
        "no generic scan job is ever queued for arcade"
    );

    // Every row the boot-time catalogue's presence pass wrote is linked to a rom.
    let orphan_rows: i64 = b
        .running
        .app
        .db
        .read_blocking(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM files WHERE platform_id = 'arcade' AND rom_id IS NULL",
                [],
                |r| r.get(0),
            )?)
        })
        .expect("count");
    assert_eq!(orphan_rows, 0, "no unmatched member ever gets a row");
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_missing_part_is_reported_not_sourced() {
    let dir = tempfile::tempdir().expect("tempdir");
    let md5 = md5_of(&[b"CPU0"]);
    write(
        &dir.path().join("_Arcade/Example Blaster.mra"),
        &mra(
            "Example Blaster",
            "exblast",
            &format!(
                r#"<rom index="0" zip="exblast.zip" md5="{md5}"><part name="gone.bin"/></rom>"#
            ),
        ),
    );
    write_zip(
        &dir.path().join("games/mame/exblast.zip"),
        &[("cpu.bin", b"CPU0")],
    );
    let config = config_in(dir.path());
    let b = boot_with(dir, config).await;
    let addr = b.addr();
    wait_rows(addr, &[("Example Blaster", 0)]).await;
    let d = detail_of(addr, "arcade", "Example Blaster").await;
    let m = &d["variants"][0]["mra"];
    assert_eq!(m["md5_check"], "missing_part");
    assert!(m["md5_detail"]
        .as_str()
        .is_some_and(|s| s.contains("gone.bin")));
    assert_eq!(m["missing_zips"], serde_json::json!([]));
    let listed: Vec<String> = std::fs::read_dir(b.dir.path().join("games/mame"))
        .expect("dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(listed, ["exblast.zip"]);
    b.running.shutdown().await.expect("shutdown");
}

const NEO_DAT: &str = r#"<?xml version="1.0"?>
<datafile>
<header><name>SNK - Neo Geo</name><version>1</version></header>
<game name="examplequest"><rom name="eq-p1.p1" size="4" crc="0a0b0c0d"/></game>
<game name="exblast"><rom name="eb-p1.p1" size="4" crc="0a0b0c0e"/></game>
<game name="exunlisted"><rom name="eu-p1.p1" size="4" crc="0a0b0c0f"/></game>
</datafile>
"#;

const ROMSETS: &str = r#"<!--
Place this file in the NeoGeo directory. Files that must be present:

   exbios.rom
   ex-lo.lo

-->
<romsets>
  <romset name="examplequest" altname="Example Quest"/>
  <romset name="exblast" altname="Example Blaster"/>
</romsets>
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn neogeo_detail_reports_romsets_and_bios_presence() {
    let dir = tempfile::tempdir().expect("tempdir");
    let neo = dir.path().join("games/NeoGeo");
    write(&neo.join("romsets.xml"), ROMSETS.as_bytes());
    write(&neo.join("examplequest/eq-p1.p1"), b"data");
    write(&neo.join("exbios.rom"), b"");
    let config = config_in(dir.path());
    let b = boot_with(dir, config).await;
    let addr = b.addr();
    let dats = b.dir.path().join("data/dats");
    std::fs::create_dir_all(&dats).expect("mkdir");
    write(&dats.join("neo.dat"), NEO_DAT.as_bytes());
    eventually("the Neo Geo DAT", || async {
        json_of(addr, "/api/v1/platforms/neogeo/titles").await["total"] == 3
    })
    .await;

    let state = |d: &Value| d["variants"][0]["romset"].clone();
    let quest = detail_of(addr, "neogeo", "examplequest").await;
    assert_eq!(
        state(&quest),
        serde_json::json!({ "listed": true, "present": true })
    );
    let bios: Vec<(String, bool)> = quest["bios"]
        .as_array()
        .expect("bios")
        .iter()
        .map(|b| {
            (
                b["name"].as_str().expect("name").to_owned(),
                b["present"].as_bool().expect("present"),
            )
        })
        .collect();
    assert_eq!(
        bios,
        [
            ("exbios.rom".to_owned(), true),
            ("ex-lo.lo".to_owned(), false)
        ]
    );
    let blast = detail_of(addr, "neogeo", "exblast").await;
    assert_eq!(
        state(&blast),
        serde_json::json!({ "listed": true, "present": false })
    );
    let unlisted = detail_of(addr, "neogeo", "exunlisted").await;
    assert_eq!(
        state(&unlisted),
        serde_json::json!({ "listed": false, "present": false })
    );

    std::fs::remove_file(neo.join("romsets.xml")).expect("rm");
    let bare = detail_of(addr, "neogeo", "examplequest").await;
    assert_eq!(
        state(&bare),
        serde_json::json!({ "listed": null, "present": true })
    );
    assert!(bare.get("bios").is_none());
    b.running.shutdown().await.expect("shutdown");
}
