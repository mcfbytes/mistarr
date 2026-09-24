//! Importing zips an MRA names: the md5, DAT and unverified paths, zips arriving in
//! either order, refusals and placement under `games/hbmame`. See `docs/ARCHITECTURE.md` "Import".

mod common;

use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use common::{boot_with, config_in, eventually, get, request, Booted};
use mistarr_core::hash::{hash_reader, HeaderRule, Md5Stream};
use mistarr_core::PlatformId;
use mistarr_server::db::downloads::{self, DownloadId, DownloadState};
use mistarr_server::db::downloads_import;
use mistarr_server::db::files::{self, FileRow, FileState};
use mistarr_server::db::imports;
use mistarr_server::db::sources::{self, NewSource, SourceId, SourceState};
use mistarr_server::events::EventKind;
use serde_json::{json, Value};

fn infohash() -> String {
    "0b".repeat(20)
}

fn write(path: &Path, data: &[u8]) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, data).expect("write");
}

fn zip_bytes(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, body) in members {
        z.start_file(*name, zip::write::SimpleFileOptions::default())
            .expect("start");
        z.write_all(body).expect("write");
    }
    z.finish().expect("finish").into_inner()
}

fn md5_of(parts: &[&[u8]]) -> String {
    let mut m = Md5Stream::new();
    for p in parts {
        m.update(p);
    }
    m.finish()
}

fn mra(name: &str, roms: &str) -> Vec<u8> {
    format!(
        "<misterromdescription><name>{name}</name><setname>exblast</setname>\
         <rbf>excore</rbf>{roms}</misterromdescription>"
    )
    .into_bytes()
}

/// Boots with the given `_Arcade` files and waits for their titles and 1G1R picks:
/// the catalogue job stores titles per batch but sets picks in a later, separate
/// write, so a `want` right after titles appear can still race it.
async fn boot_arcade(mras: &[(&str, Vec<u8>)], games: &[(&str, Vec<u8>)]) -> Booted {
    let dir = tempfile::tempdir().expect("tempdir");
    for (rel, body) in mras {
        write(&dir.path().join("_Arcade").join(rel), body);
    }
    for (rel, body) in games {
        write(&dir.path().join("games").join(rel), body);
    }
    let config = config_in(dir.path());
    let b = boot_with(dir, config).await;
    let want = mras.len();
    eventually("the arcade catalogue and its 1G1R picks", || async {
        let rows = browse(&b).await;
        rows.len() == want && rows.iter().all(|(_, _, has_pick)| *has_pick)
    })
    .await;
    b
}

/// Browse rows of the arcade platform as `(name, have_verified, has_pick)`.
async fn browse(b: &Booted) -> Vec<(String, u64, bool)> {
    get(b.addr(), "/api/v1/platforms/arcade/titles")
        .await
        .json()["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|i| {
            let name = i["name"].as_str().expect("name").to_owned();
            (
                name,
                i["have_verified"].as_u64().expect("have"),
                i["has_pick"].as_bool().expect("has_pick"),
            )
        })
        .collect()
}

async fn have(b: &Booted, name: &str) -> u64 {
    browse(b)
        .await
        .into_iter()
        .find(|(n, _, _)| n == name)
        .map_or_else(|| panic!("no {name}"), |(_, h, _)| h)
}

fn title_id(b: &Booted, name: &str) -> i64 {
    let name = name.to_owned();
    b.running
        .app
        .db
        .read_blocking(move |c| {
            Ok(c.query_row(
                "SELECT id FROM titles WHERE source = 'mra' AND name = ?1",
                [name],
                |r| r.get(0),
            )?)
        })
        .expect("title")
}

async fn mra_block(b: &Booted, name: &str) -> Value {
    let id = title_id(b, name);
    let detail = get(b.addr(), &format!("/api/v1/titles/{id}")).await.json();
    detail["variants"][0]["mra"].clone()
}

fn zip_rom(b: &Booted, zip: &str) -> i64 {
    let zip = zip.to_owned();
    b.running
        .app
        .db
        .read_blocking(move |c| {
            Ok(c.query_row(
                "SELECT r.id FROM roms r JOIN titles t ON t.id = r.title_id
                 WHERE t.source = 'mra' AND r.name = ?1",
                [zip],
                |r| r.get(0),
            )?)
        })
        .expect("rom")
}

fn source(b: &Booted) -> SourceId {
    let hash = infohash();
    b.running
        .app
        .db
        .write_blocking(|c| {
            sources::insert(
                c,
                &NewSource {
                    infohash: &hash,
                    display_name: "Synthetic Set",
                    origin_file: "set.torrent",
                    state: SourceState::Bound,
                    reason: None,
                    added_at: 1,
                },
            )
        })
        .expect("source")
}

fn staging(b: &Booted) -> PathBuf {
    b.running.app.config().paths.staging()
}

fn games(b: &Booted) -> PathBuf {
    b.running.app.config().paths.games
}

fn stage(b: &Booted, name: &str, data: &[u8]) -> PathBuf {
    let path = staging(b).join(infohash()).join("Synthetic Set").join(name);
    write(&path, data);
    path
}

fn announce(b: &Booted, id: DownloadId) {
    b.running.app.events.publish(
        EventKind::DownloadChanged,
        &json!({ "download_id": id.0, "state": "importing", "progress": 1.0 }),
    );
}

/// Hands `path` to the importer as the transfer of `rom_id`.
fn hand_off(b: &Booted, rom_id: i64, src: SourceId, index: u32, path: &Path) -> DownloadId {
    let staged = path.to_string_lossy().into_owned();
    let id = b
        .running
        .app
        .db
        .write_blocking(move |c| {
            downloads_import::insert_fixture(c, rom_id, src, index, "importing", Some(&staged))
        })
        .expect("download");
    announce(b, id);
    id
}

async fn settled(b: &Booted, id: DownloadId, want: DownloadState) -> downloads::DownloadRow {
    let read = || {
        b.running
            .app
            .db
            .read_blocking(move |c| downloads::get(c, id))
            .expect("read")
            .expect("row")
    };
    eventually(&format!("download {id} {want}"), || async {
        read().state == want
    })
    .await;
    read()
}

fn rows(b: &Booted, zip_rel: &str) -> Vec<FileRow> {
    let zip_rel = zip_rel.to_owned();
    b.running
        .app
        .db
        .read_blocking(move |c| files::zip_member_rows(c, &PlatformId("arcade".into()), &zip_rel))
        .expect("rows")
}

fn states(rows: &[FileRow]) -> Vec<(String, FileState, Option<i64>)> {
    rows.iter()
        .map(|r| (r.rel_path.clone(), r.state, r.rom_id))
        .collect()
}

fn log(b: &Booted) -> Vec<imports::LogRow> {
    b.running
        .app
        .db
        .read_blocking(|c| imports::list(c, 100, 0).map(|(items, _)| items))
        .expect("log")
}

/// Triggers the arcade catalogue, which now also runs the presence pass over
/// `games/mame` and `games/hbmame`. The caller polls for the effect it wants
/// with `eventually`, since the run may join one already queued or running.
async fn rerun_catalogue(b: &Booted) {
    let r = request(
        b.addr(),
        "POST",
        "/api/v1/system/scan",
        &[],
        Some(r#"{"platform_id":"arcade"}"#),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_md5_covered_zip_is_verified_by_assembly_and_placed_whole() {
    let md5 = md5_of(&[b"CPU0", b"SND"]);
    let roms = format!(
        r#"<rom index="0" zip="exblast.zip" md5="{md5}"><part name="cpu.bin"/><part name="snd.bin"/></rom>"#
    );
    let b = boot_arcade(
        &[("Example Blaster.mra", mra("Example Blaster", &roms))],
        &[],
    )
    .await;
    let rom = zip_rom(&b, "exblast.zip");
    let src = source(&b);
    let body = zip_bytes(&[
        ("cpu.bin", b"CPU0"),
        ("snd.bin", b"SND"),
        ("notes.txt", b"n"),
    ]);
    let staged = stage(&b, "arcade/exblast-download.zip", &body);
    let id = hand_off(&b, rom, src, 0, &staged);
    settled(&b, id, DownloadState::Done).await;

    let placed = games(&b).join("mame/exblast.zip");
    assert_eq!(std::fs::read(&placed).expect("placed"), body);
    assert!(!staged.exists());
    assert!(!games(&b).join("mame/cpu.bin").exists());
    assert_eq!(
        states(&rows(&b, "mame/exblast.zip")),
        [
            (
                "mame/exblast.zip#cpu.bin".into(),
                FileState::Verified,
                Some(rom)
            ),
            (
                "mame/exblast.zip#notes.txt".into(),
                FileState::Unverified,
                Some(rom)
            ),
            (
                "mame/exblast.zip#snd.bin".into(),
                FileState::Verified,
                Some(rom)
            ),
        ]
    );
    let entries = log(&b);
    assert!(entries
        .iter()
        .all(|e| e.detail["verification"] == "mra_md5"));
    assert_eq!(entries.len(), 3);
    assert_eq!(have(&b, "Example Blaster").await, 1);
    let m = mra_block(&b, "Example Blaster").await;
    assert_eq!(m["md5_check"], "match");
    assert_eq!(m["missing_zips"], json!([]));
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_an_md5_a_loaded_dat_verifies_member_by_member() {
    let roms = r#"<rom index="0" zip="exblast.zip"><part name="cpu.bin"/></rom>"#;
    let b = boot_arcade(
        &[("Example Blaster.mra", mra("Example Blaster", roms))],
        &[],
    )
    .await;
    let hash = |d: &[u8]| hash_reader(Cursor::new(d), HeaderRule::None, None).expect("hash");
    let (hc, hs) = (hash(b"CPU0"), hash(b"SND"));
    let dat_roms = b
        .running
        .app
        .db
        .write_blocking(move |c| {
            let t = files::seed_title_fixture(c, &PlatformId("arcade".into()), "exblast")?;
            Ok([
                files::seed_rom_for_title_fixture(c, t, "cpu.bin", &hc, "good")?,
                files::seed_rom_for_title_fixture(c, t, "snd.bin", &hs, "good")?,
            ])
        })
        .expect("dat");
    let rom = zip_rom(&b, "exblast.zip");
    let src = source(&b);
    let good = stage(
        &b,
        "arcade/exblast.zip",
        &zip_bytes(&[("snd.bin", b"SND"), ("cpu.bin", b"CPU0")]),
    );
    let id = hand_off(&b, rom, src, 0, &good);
    settled(&b, id, DownloadState::Done).await;
    assert_eq!(
        states(&rows(&b, "mame/exblast.zip")),
        [
            (
                "mame/exblast.zip#cpu.bin".into(),
                FileState::Verified,
                Some(dat_roms[0])
            ),
            (
                "mame/exblast.zip#snd.bin".into(),
                FileState::Verified,
                Some(dat_roms[1])
            ),
        ]
    );
    let entries = log(&b);
    assert!(entries.iter().all(|e| e.detail["verification"] == "dat"));
    assert_eq!(entries[0].detail["dat_entry"], "exblast");
    assert_eq!(have(&b, "Example Blaster").await, 1);
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_zip_the_dat_disagrees_with_is_quarantined() {
    let roms = r#"<rom index="0" zip="exblast.zip"><part name="cpu.bin"/></rom>"#;
    let b = boot_arcade(
        &[("Example Blaster.mra", mra("Example Blaster", roms))],
        &[],
    )
    .await;
    let hc = hash_reader(Cursor::new(b"CPU0"), HeaderRule::None, None).expect("hash");
    b.running
        .app
        .db
        .write_blocking(move |c| {
            let t = files::seed_title_fixture(c, &PlatformId("arcade".into()), "exblast")?;
            files::seed_rom_for_title_fixture(c, t, "cpu.bin", &hc, "good")
        })
        .expect("dat");
    let rom = zip_rom(&b, "exblast.zip");
    let src = source(&b);
    let staged = stage(
        &b,
        "arcade/exblast.zip",
        &zip_bytes(&[("cpu.bin", b"CPUX")]),
    );
    let id = hand_off(&b, rom, src, 0, &staged);
    let row = settled(&b, id, DownloadState::Bad).await;
    assert!(
        row.error
            .as_deref()
            .is_some_and(|e| e.contains("DAT entry exblast")),
        "{:?}",
        row.error
    );
    assert!(!games(&b).join("mame/exblast.zip").exists());
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_no_hash_source_the_zip_is_placed_unverified() {
    let roms = r#"<rom index="0" zip="exblast.zip"><part name="cpu.bin"/></rom>"#;
    let b = boot_arcade(
        &[("Example Blaster.mra", mra("Example Blaster", roms))],
        &[],
    )
    .await;
    let rom = zip_rom(&b, "exblast.zip");
    let src = source(&b);
    let staged = stage(
        &b,
        "arcade/exblast.zip",
        &zip_bytes(&[("cpu.bin", b"CPU0")]),
    );
    let id = hand_off(&b, rom, src, 0, &staged);
    settled(&b, id, DownloadState::Done).await;
    assert_eq!(
        states(&rows(&b, "mame/exblast.zip")),
        [(
            "mame/exblast.zip#cpu.bin".into(),
            FileState::Unverified,
            Some(rom)
        )]
    );
    let entry = &log(&b)[0];
    assert_eq!(entry.action, "placed");
    assert_eq!(entry.detail["verification"], "none");
    assert_eq!(entry.detail["reason"], "no hash source");
    assert_eq!(have(&b, "Example Blaster").await, 1);
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_zip_read_from_hbmame_is_placed_there() {
    let roms = r#"<rom index="0" zip="/hbmame/examplequest.zip"><part name="q.bin"/></rom>"#;
    let b = boot_arcade(&[("Example Quest.mra", mra("Example Quest", roms))], &[]).await;
    let rom = zip_rom(&b, "examplequest.zip");
    let src = source(&b);
    let body = zip_bytes(&[("q.bin", b"Q")]);
    let staged = stage(&b, "hb/examplequest.zip", &body);
    let id = hand_off(&b, rom, src, 0, &staged);
    settled(&b, id, DownloadState::Done).await;
    assert_eq!(
        std::fs::read(games(&b).join("hbmame/examplequest.zip")).expect("placed"),
        body
    );
    assert!(!games(&b).join("mame").exists());
    assert_eq!(
        states(&rows(&b, "hbmame/examplequest.zip")),
        [(
            "hbmame/examplequest.zip#q.bin".into(),
            FileState::Unverified,
            Some(rom)
        )]
    );
    assert_eq!(have(&b, "Example Quest").await, 1);
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_zip_without_the_members_the_mra_names_is_quarantined() {
    let roms = r#"<rom index="0" zip="exblast.zip"><part name="cpu.bin"/><part name="gone.bin"/><part name="lost.bin"/></rom>"#;
    let b = boot_arcade(
        &[("Example Blaster.mra", mra("Example Blaster", roms))],
        &[],
    )
    .await;
    let rom = zip_rom(&b, "exblast.zip");
    let src = source(&b);
    let staged = stage(
        &b,
        "arcade/exblast.zip",
        &zip_bytes(&[("cpu.bin", b"CPU0")]),
    );
    let id = hand_off(&b, rom, src, 0, &staged);
    let row = settled(&b, id, DownloadState::Bad).await;
    assert!(
        row.error
            .as_deref()
            .is_some_and(|e| e.contains("gone.bin, lost.bin")),
        "{:?}",
        row.error
    );
    let dir = staging(&b).join("quarantine").join(infohash());
    assert!(dir.join("exblast.zip").is_file());
    let report = std::fs::read_to_string(dir.join("exblast.zip.report.txt")).expect("report");
    assert!(report.contains("Missing: gone.bin, lost.bin"), "{report}");
    let entry = &log(&b)[0];
    assert_eq!(entry.action, "quarantined");
    assert_eq!(entry.detail["missing"], json!(["gone.bin", "lost.bin"]));
    assert!(!games(&b).join("mame/exblast.zip").exists());
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_mra_the_assembler_cannot_build_is_refused_with_its_reason() {
    let md5 = md5_of(&[b"CPU0"]);
    let roms = format!(
        r#"<rom index="0" zip="exblast.zip" md5="{md5}"><part name="cpu.bin" map="01"/></rom>"#
    );
    let b = boot_arcade(
        &[("Example Blaster.mra", mra("Example Blaster", &roms))],
        &[],
    )
    .await;
    let rom = zip_rom(&b, "exblast.zip");
    let src = source(&b);
    let staged = stage(
        &b,
        "arcade/exblast.zip",
        &zip_bytes(&[("cpu.bin", b"CPU0")]),
    );
    let id = hand_off(&b, rom, src, 0, &staged);
    let row = settled(&b, id, DownloadState::Failed).await;
    assert!(
        row.error
            .as_deref()
            .is_some_and(|e| e.contains("MRA content not supported: map outside <interleave>")),
        "{:?}",
        row.error
    );
    assert!(staged.is_file(), "a refused zip stays in staging");
    assert!(!games(&b).join("mame/exblast.zip").exists());
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wanting_creates_one_download_per_missing_zip() {
    let roms =
        r#"<rom index="0" zip="exblast.zip|exparent.zip|exsound.zip"><part name="a.bin"/></rom>"#;
    let b = boot_arcade(
        &[("Example Blaster.mra", mra("Example Blaster", roms))],
        &[("mame/exparent.zip", zip_bytes(&[("p.bin", b"P")]))],
    )
    .await;
    let id = title_id(&b, "Example Blaster");
    let r = request(
        b.addr(),
        "POST",
        &format!("/api/v1/titles/{id}/want"),
        &[],
        Some("{}"),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let listed = get(b.addr(), "/api/v1/downloads").await.json();
    let mut names: Vec<String> = listed["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|d| d["rom_name"].as_str().expect("name").to_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["exblast.zip", "exsound.zip"]);
    assert_eq!(have(&b, "Example Blaster").await, 0);
    b.running.shutdown().await.expect("shutdown");
}

/// Seeds a DAT entry `set` on the arcade platform with one rom and names its DAT `dat_name`.
fn dat_entry(b: &Booted, dat_name: &str, set: &str, rom: &str, data: &[u8], bios: bool) -> i64 {
    let hashes = hash_reader(Cursor::new(data), HeaderRule::None, None).expect("hash");
    let (dat_name, set, rom) = (dat_name.to_owned(), set.to_owned(), rom.to_owned());
    b.running
        .app
        .db
        .write_blocking(move |c| {
            let t = files::seed_title_fixture(c, &PlatformId("arcade".into()), &set)?;
            let id = files::seed_rom_for_title_fixture(c, t, &rom, &hashes, "good")?;
            let flags = if bios { r#"["bios"]"# } else { "[]" };
            c.execute(
                "UPDATE titles SET flags = ?2 WHERE id = ?1",
                rusqlite::params![t, flags],
            )?;
            c.execute(
                "UPDATE dat_versions SET dat_name = ?2
                 WHERE id = (SELECT dat_version_id FROM titles WHERE id = ?1)",
                rusqlite::params![t, dat_name],
            )?;
            Ok(id)
        })
        .expect("dat")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_md5_covered_zip_imports_whatever_a_dat_flags_it() {
    let md5 = md5_of(&[b"CPU0"]);
    let roms =
        format!(r#"<rom index="0" zip="exblast.zip" md5="{md5}"><part name="cpu.bin"/></rom>"#);
    let b = boot_arcade(
        &[("Example Blaster.mra", mra("Example Blaster", &roms))],
        &[],
    )
    .await;
    dat_entry(&b, "MAME", "exblast", "cpu.bin", b"CPU0", true);
    let rom = zip_rom(&b, "exblast.zip");
    let src = source(&b);
    let staged = stage(
        &b,
        "arcade/exblast.zip",
        &zip_bytes(&[("cpu.bin", b"CPU0")]),
    );
    let id = hand_off(&b, rom, src, 0, &staged);
    settled(&b, id, DownloadState::Done).await;
    assert_eq!(log(&b)[0].detail["verification"], "mra_md5");
    assert!(games(&b).join("mame/exblast.zip").is_file());
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dat_bios_entry_refuses_a_zip_only_it_would_verify() {
    let roms = r#"<rom index="0" zip="exblast.zip"><part name="cpu.bin"/></rom>"#;
    let b = boot_arcade(
        &[("Example Blaster.mra", mra("Example Blaster", roms))],
        &[],
    )
    .await;
    dat_entry(&b, "MAME", "exblast", "cpu.bin", b"CPU0", true);
    let rom = zip_rom(&b, "exblast.zip");
    let src = source(&b);
    let staged = stage(
        &b,
        "arcade/exblast.zip",
        &zip_bytes(&[("cpu.bin", b"CPU0")]),
    );
    let id = hand_off(&b, rom, src, 0, &staged);
    let row = settled(&b, id, DownloadState::Failed).await;
    assert!(
        row.error.as_deref().is_some_and(|e| e.contains("BIOS")),
        "{:?}",
        row.error
    );
    assert!(staged.is_file());
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hbmame_zips_are_verified_only_by_an_hbmame_dat() {
    let b = boot_arcade(
        &[
            (
                "Example Blaster.mra",
                mra(
                    "Example Blaster",
                    r#"<rom index="0" zip="/hbmame/exblast.zip"><part name="cpu.bin"/></rom>"#,
                ),
            ),
            (
                "Example Quest.mra",
                mra(
                    "Example Quest",
                    r#"<rom index="0" zip="/hbmame/examplequest.zip"><part name="q.bin"/></rom>"#,
                ),
            ),
        ],
        &[],
    )
    .await;
    dat_entry(&b, "MAME", "exblast", "cpu.bin", b"MAME", false);
    let hb_rom = dat_entry(&b, "HBMAME", "examplequest", "q.bin", b"Q", false);
    let src = source(&b);
    let blast = stage(
        &b,
        "hb/exblast.zip",
        &zip_bytes(&[("cpu.bin", b"HOMEBREW")]),
    );
    let id = hand_off(&b, zip_rom(&b, "exblast.zip"), src, 0, &blast);
    settled(&b, id, DownloadState::Done).await;
    assert_eq!(log(&b)[0].detail["verification"], "none");
    assert_eq!(
        rows(&b, "hbmame/exblast.zip")[0].state,
        FileState::Unverified
    );

    let quest = stage(&b, "hb/examplequest.zip", &zip_bytes(&[("q.bin", b"Q")]));
    let id = hand_off(&b, zip_rom(&b, "examplequest.zip"), src, 1, &quest);
    settled(&b, id, DownloadState::Done).await;
    assert_eq!(log(&b)[0].detail["verification"], "dat");
    assert_eq!(
        states(&rows(&b, "hbmame/examplequest.zip")),
        [(
            "hbmame/examplequest.zip#q.bin".into(),
            FileState::Verified,
            Some(hb_rom)
        )]
    );
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn members_read_by_a_failing_alternative_stay_unverified() {
    let (wrong, right) = (md5_of(&[b"nothing"]), md5_of(&[b"CPU0"]));
    let roms = format!(
        r#"<rom index="0" zip="exblast.zip" md5="{wrong}"><part name="old.bin"/></rom>
           <rom index="0" zip="exblast.zip" md5="{right}"><part name="cpu.bin"/></rom>"#
    );
    let b = boot_arcade(
        &[("Example Blaster.mra", mra("Example Blaster", &roms))],
        &[],
    )
    .await;
    let rom = zip_rom(&b, "exblast.zip");
    let src = source(&b);
    let body = zip_bytes(&[("cpu.bin", b"CPU0"), ("old.bin", b"OLD")]);
    let staged = stage(&b, "arcade/exblast.zip", &body);
    let id = hand_off(&b, rom, src, 0, &staged);
    settled(&b, id, DownloadState::Done).await;
    assert_eq!(
        states(&rows(&b, "mame/exblast.zip")),
        [
            (
                "mame/exblast.zip#cpu.bin".into(),
                FileState::Verified,
                Some(rom)
            ),
            (
                "mame/exblast.zip#old.bin".into(),
                FileState::Unverified,
                Some(rom)
            ),
        ]
    );
    b.running.shutdown().await.expect("shutdown");
}

/// A two-zip MRA whose one md5 spans both zips, imported with `first` landing first.
async fn two_zips_arrive(first: &str) {
    let md5 = md5_of(&[b"AAAA", b"BBBB"]);
    let roms = format!(
        r#"<rom index="0" zip="exblast.zip|exparent.zip" md5="{md5}">
             <part name="a.bin"/><part name="b.bin"/></rom>"#
    );
    let b = boot_arcade(
        &[("Example Blaster.mra", mra("Example Blaster", &roms))],
        &[],
    )
    .await;
    let title = title_id(&b, "Example Blaster");
    let r = request(
        b.addr(),
        "POST",
        &format!("/api/v1/titles/{title}/want"),
        &[],
        Some("{}"),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let wanted: Vec<(DownloadId, i64)> = b
        .running
        .app
        .db
        .read_blocking(|c| {
            let (rows, _) = downloads::list(c, &[DownloadState::Wanted], 10, 0)?;
            Ok(rows.into_iter().map(|r| (r.id, r.rom_id)).collect())
        })
        .expect("downloads");
    assert_eq!(wanted.len(), 2);
    let src = source(&b);
    let bodies = [
        (
            "exblast.zip",
            zip_bytes(&[("a.bin", b"AAAA"), ("readme.txt", b"r")]),
        ),
        ("exparent.zip", zip_bytes(&[("b.bin", b"BBBB")])),
    ];
    let mut order: Vec<&(&str, Vec<u8>)> = bodies.iter().collect();
    if first == "exparent.zip" {
        order.reverse();
    }
    let mut ids = Vec::new();
    for (index, (name, body)) in (0u32..).zip(&order) {
        let rom = zip_rom(&b, name);
        let (download, _) = *wanted.iter().find(|(_, r)| *r == rom).expect("download");
        let path = stage(&b, &format!("arcade/{name}"), body);
        let staged = path.to_string_lossy().into_owned();
        b.running
            .app
            .db
            .write_blocking(move |c| {
                c.execute(
                    "UPDATE downloads SET state = 'importing', source_id = ?2, file_index = ?3,
                       staged_path = ?4 WHERE id = ?1",
                    rusqlite::params![download.0, src.0, index, staged],
                )?;
                Ok(())
            })
            .expect("importing");
        ids.push((download, *name));
    }

    announce(&b, ids[0].0);
    settled(&b, ids[0].0, DownloadState::Done).await;
    let early = format!("mame/{first}");
    assert!(rows(&b, &early)
        .iter()
        .all(|r| r.state == FileState::Unverified));
    let waits = log(&b)[0].detail["reason"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(
        waits.starts_with("the md5 check waits for mame/"),
        "{waits}"
    );
    assert_eq!(have(&b, "Example Blaster").await, 0);

    announce(&b, ids[1].0);
    settled(&b, ids[1].0, DownloadState::Done).await;
    assert_eq!(have(&b, "Example Blaster").await, 1);
    assert_eq!(mra_block(&b, "Example Blaster").await["md5_check"], "match");
    let verified = |rel: &str| {
        rows(&b, rel)
            .into_iter()
            .filter(|r| r.state == FileState::Verified)
            .map(|r| r.rel_path)
            .collect::<Vec<_>>()
    };
    assert_eq!(verified("mame/exblast.zip"), ["mame/exblast.zip#a.bin"]);
    assert_eq!(verified("mame/exparent.zip"), ["mame/exparent.zip#b.bin"]);
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn zips_of_one_mra_arriving_main_first_are_checked_when_the_last_lands() {
    two_zips_arrive("exblast.zip").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn zips_of_one_mra_arriving_parent_first_are_checked_when_the_last_lands() {
    two_zips_arrive("exparent.zip").await;
}

/// Once an imported zip is deleted from disk, the next catalogue run's presence
/// pass prunes its `files` rows and the title drops out of `have`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_an_imported_zip_drops_have_once_the_catalogue_reruns() {
    let md5 = md5_of(&[b"CPU0", b"SND"]);
    let roms = format!(
        r#"<rom index="0" zip="exblast.zip" md5="{md5}"><part name="cpu.bin"/><part name="snd.bin"/></rom>"#
    );
    let b = boot_arcade(
        &[("Example Blaster.mra", mra("Example Blaster", &roms))],
        &[],
    )
    .await;
    let rom = zip_rom(&b, "exblast.zip");
    let src = source(&b);
    let body = zip_bytes(&[("cpu.bin", b"CPU0"), ("snd.bin", b"SND")]);
    let staged = stage(&b, "arcade/exblast-download.zip", &body);
    let id = hand_off(&b, rom, src, 0, &staged);
    settled(&b, id, DownloadState::Done).await;
    assert_eq!(have(&b, "Example Blaster").await, 1);

    std::fs::remove_file(games(&b).join("mame/exblast.zip")).expect("rm");
    rerun_catalogue(&b).await;
    eventually("presence to prune the rows", || async {
        rows(&b, "mame/exblast.zip").is_empty()
    })
    .await;

    assert_eq!(have(&b, "Example Blaster").await, 0);
    b.running.shutdown().await.expect("shutdown");
}

/// A zip the user places directly under `games/mame`, without a download, gets a row
/// from the presence pass that `verify_siblings` can later promote once its pair lands.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_user_placed_sibling_zip_is_promoted_once_its_pair_is_imported() {
    let md5 = md5_of(&[b"AAAA", b"BBBB"]);
    let roms = format!(
        r#"<rom index="0" zip="exblast.zip|exparent.zip" md5="{md5}">
             <part name="a.bin"/><part name="b.bin"/></rom>"#
    );
    let b = boot_arcade(
        &[("Example Blaster.mra", mra("Example Blaster", &roms))],
        &[],
    )
    .await;

    write(
        &games(&b).join("mame/exparent.zip"),
        &zip_bytes(&[("b.bin", b"BBBB")]),
    );
    rerun_catalogue(&b).await;
    eventually("the presence pass to record the sibling", || async {
        !rows(&b, "mame/exparent.zip").is_empty()
    })
    .await;
    let parent_rom = zip_rom(&b, "exparent.zip");
    assert_eq!(
        states(&rows(&b, "mame/exparent.zip")),
        [(
            "mame/exparent.zip#b.bin".into(),
            FileState::Unverified,
            Some(parent_rom)
        )],
        "the presence pass gives verify_siblings a row to promote"
    );

    let rom = zip_rom(&b, "exblast.zip");
    let src = source(&b);
    let staged = stage(&b, "arcade/exblast.zip", &zip_bytes(&[("a.bin", b"AAAA")]));
    let id = hand_off(&b, rom, src, 0, &staged);
    settled(&b, id, DownloadState::Done).await;

    assert_eq!(
        states(&rows(&b, "mame/exparent.zip")),
        [(
            "mame/exparent.zip#b.bin".into(),
            FileState::Verified,
            Some(parent_rom)
        )],
        "the user-placed sibling's member is promoted, not left unverified forever"
    );
    assert_eq!(have(&b, "Example Blaster").await, 1);
    b.running.shutdown().await.expect("shutdown");
}
