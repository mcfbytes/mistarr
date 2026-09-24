//! The importer against a booted server: synthetic entries, staged files and
//! download rows handed over in `importing`. See `docs/ARCHITECTURE.md` "Import".

mod common;

use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use common::{boot, boot_with, config_in, eventually, get, request, Booted, Sse};
use mistarr_clients::fake::{FakeResponse, FakeServer};
use mistarr_clients::SeedPolicy;
use mistarr_core::hash::{hash_reader, HeaderRule};
use mistarr_core::{HashSet, PlatformId};
use mistarr_server::db::downloads_import::{self as downloads, state, DownloadId};
use mistarr_server::db::files::{self, FileId, FileState, Hashed};
use mistarr_server::db::imports::{self, ImportAction};
use mistarr_server::db::sources::{self, NewSource, SourceId, SourceState};
use mistarr_server::events::EventKind;
use serde_json::{json, Value};

/// A synthetic infohash.
fn infohash() -> String {
    "0a".repeat(20)
}

/// The DAT header of a synthetic 32 KiB NROM cartridge.
const INES: &str = "4E 45 53 1A 02 01 01 00 00 00 00 00 00 00 00 00";

fn ines_bytes() -> Vec<u8> {
    let mut h = b"NES\x1a".to_vec();
    h.extend_from_slice(&[2, 1, 1]);
    h.resize(16, 0);
    h
}

fn payload(seed: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| u8::try_from((i * 31 + usize::from(seed)) % 251).expect("byte"))
        .collect()
}

fn hash_of(data: &[u8]) -> HashSet {
    hash_reader(Cursor::new(data), HeaderRule::None, None).expect("hash")
}

fn staging(b: &Booted) -> PathBuf {
    b.running.app.config().paths.staging()
}

fn games(b: &Booted) -> PathBuf {
    b.running.app.config().paths.games
}

fn write(path: &Path, data: &[u8]) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, data).expect("write");
}

fn source(b: &Booted, client_id: Option<&str>) -> SourceId {
    let hash = infohash();
    b.running
        .app
        .db
        .write_blocking(|c| {
            let id = sources::insert(
                c,
                &NewSource {
                    infohash: &hash,
                    display_name: "Synthetic Set",
                    origin_file: "set.torrent",
                    state: SourceState::Bound,
                    reason: None,
                    added_at: 1,
                },
            )?;
            sources::set_client_id(c, id, client_id)?;
            Ok(id)
        })
        .expect("source")
}

/// Seeds one entry with one rom and returns `(title_id, rom_id)`.
fn entry(b: &Booted, platform: &str, game: &str, rom: &str, hashes: &HashSet) -> (i64, i64) {
    let (pid, game, rom, hashes) = (
        PlatformId(platform.into()),
        game.to_owned(),
        rom.to_owned(),
        hashes.clone(),
    );
    b.running
        .app
        .db
        .write_blocking(move |c| {
            let rom_id = files::seed_rom_fixture(c, &pid, &game, &rom, &hashes, "good")?;
            let title = imports::title_of_rom(c, rom_id)?.expect("title");
            Ok((title.0, rom_id))
        })
        .expect("seed")
}

fn set_header(b: &Booted, rom_id: i64, header: &str) {
    let header = header.to_owned();
    b.running
        .app
        .db
        .write_blocking(move |c| {
            c.execute(
                "UPDATE roms SET header = ?2 WHERE id = ?1",
                rusqlite::params![rom_id, header],
            )?;
            Ok(())
        })
        .expect("header");
}

/// Stages `data` under `staging/<infohash>/Synthetic Set/<rel>`.
fn stage(b: &Booted, rel: &str, data: &[u8]) -> PathBuf {
    let path = staging(b).join(infohash()).join("Synthetic Set").join(rel);
    write(&path, data);
    path
}

/// Inserts a download in `importing` without announcing it.
fn insert(b: &Booted, rom_id: i64, src: SourceId, index: u32, path: &Path) -> DownloadId {
    let staged = path.to_string_lossy().into_owned();
    b.running
        .app
        .db
        .write_blocking(move |c| {
            downloads::insert_fixture(c, rom_id, src, index, state::IMPORTING, Some(&staged))
        })
        .expect("download")
}

/// Publishes `download.changed` as the poller does on entering `importing`.
fn announce(b: &Booted, id: DownloadId) {
    b.running.app.events.publish(
        EventKind::DownloadChanged,
        &json!({ "download_id": id.0, "state": "importing", "progress": 1.0 }),
    );
}

fn hand_off(b: &Booted, rom_id: i64, src: SourceId, index: u32, path: &Path) -> DownloadId {
    let id = insert(b, rom_id, src, index, path);
    announce(b, id);
    id
}

fn download(b: &Booted, id: DownloadId) -> downloads::ImportRow {
    b.running
        .app
        .db
        .read_blocking(move |c| downloads::get(c, id))
        .expect("read")
        .expect("row")
}

async fn settled(b: &Booted, id: DownloadId, want: &str) -> downloads::ImportRow {
    eventually(&format!("download {id} {want}"), || async {
        download(b, id).state == want
    })
    .await;
    download(b, id)
}

fn log(b: &Booted) -> Vec<imports::LogRow> {
    b.running
        .app
        .db
        .read_blocking(|c| imports::list(c, 100, 0).map(|(items, _)| items))
        .expect("log")
}

fn file_at(b: &Booted, platform: &str, rel: &str) -> Option<files::FileRow> {
    let (pid, rel) = (PlatformId(platform.into()), rel.to_owned());
    b.running
        .app
        .db
        .read_blocking(move |c| files::find_by_path(c, &pid, &rel))
        .expect("files")
}

fn existing_file(b: &Booted, rel: &str, data: &[u8], rom_id: Option<i64>, st: FileState) -> FileId {
    write(&games(b).join(rel), data);
    let h = hash_of(data);
    let (rel, size) = (rel.to_owned(), i64::try_from(data.len()).expect("size"));
    b.running
        .app
        .db
        .write_blocking(move |c| {
            let hashed = Hashed {
                crc32: Some(&h.crc32),
                md5: Some(&h.md5),
                sha1: Some(&h.sha1),
                header_rule: Some("ines"),
            };
            files::upsert(
                c,
                &PlatformId("nes".into()),
                &rel,
                size,
                1,
                &hashed,
                rom_id,
                st,
                1,
            )
        })
        .expect("file")
}

const NES_TARGET: &str = "NES/Example Quest (USA).nes";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn headerless_nes_is_placed_with_the_dat_header() {
    let b = boot().await;
    let body = payload(1, 32 * 1024);
    let (title, rom) = entry(
        &b,
        "nes",
        "Example Quest (USA)",
        "Example Quest (USA).nes",
        &hash_of(&body),
    );
    set_header(&b, rom, INES);
    let src = source(&b, None);
    let mut sse = Sse::open(b.addr(), "/api/v1/events", &[]).await;
    let staged = stage(&b, "NES/example.nes", &body);
    let id = hand_off(&b, rom, src, 0, &staged);
    settled(&b, id, state::DONE).await;
    sse.until("event: import.done").await;
    sse.until(r#""action":"placed""#).await;
    assert!(sse.text.contains(&format!(r#""title_id":{title}"#)));

    let placed = std::fs::read(games(&b).join(NES_TARGET)).expect("placed");
    assert_eq!(placed[..16], ines_bytes()[..]);
    assert_eq!(placed[16..], body[..]);
    assert!(!staged.exists());
    let row = file_at(&b, "nes", NES_TARGET).expect("files row");
    assert_eq!((row.state, row.rom_id), (FileState::Verified, Some(rom)));
    assert_eq!(row.sha1.as_deref(), Some(hash_of(&body).sha1.as_str()));
    let entries = log(&b);
    assert_eq!(entries[0].action, "placed");
    assert_eq!(entries[0].download_id, Some(id.0));
    assert_eq!(entries[0].file_id, Some(row.id.0));
    let detail = get(b.addr(), &format!("/api/v1/titles/{title}"))
        .await
        .json();
    assert_eq!(detail["variants"][0]["roms"][0]["file_state"], "verified");
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn zipped_input_is_unzipped() {
    let b = boot().await;
    let body = payload(2, 4096);
    let (_, rom) = entry(
        &b,
        "gba",
        "Example Quest (USA)",
        "Example Quest (USA).gba",
        &hash_of(&body),
    );
    let src = source(&b, None);
    let mut buf = Vec::new();
    let mut z = zip::ZipWriter::new(Cursor::new(&mut buf));
    z.start_file(
        "Example Quest (USA).gba",
        zip::write::SimpleFileOptions::default(),
    )
    .expect("start");
    z.write_all(&body).expect("write");
    z.finish().expect("finish");
    let staged = stage(&b, "GBA/Example Quest (USA).zip", &buf);
    let id = hand_off(&b, rom, src, 0, &staged);
    settled(&b, id, state::DONE).await;
    let target = games(&b).join("GBA/Example Quest (USA).gba");
    assert_eq!(std::fs::read(target).expect("placed"), body);
    assert!(!staged.exists(), "the consumed zip is removed");
    assert!(!staged.with_file_name("Example Quest (USA).gba").exists());
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn byte_swapped_n64_is_normalised() {
    let b = boot().await;
    let mut big = vec![0x80, 0x37, 0x12, 0x40];
    big.extend(payload(3, 4092));
    let swapped: Vec<u8> = big.chunks(2).flat_map(|p| [p[1], p[0]]).collect();
    let (_, rom) = entry(
        &b,
        "n64",
        "Example Quest (USA)",
        "Example Quest (USA).z64",
        &hash_of(&big),
    );
    let src = source(&b, None);
    let staged = stage(&b, "N64/Example Quest (USA).v64", &swapped);
    let id = hand_off(&b, rom, src, 0, &staged);
    settled(&b, id, state::DONE).await;
    let placed = std::fs::read(games(&b).join("N64/Example Quest (USA).z64")).expect("placed");
    assert_eq!(placed, big);
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mismatch_is_quarantined_with_a_report() {
    let b = boot().await;
    let (_, rom) = entry(
        &b,
        "nes",
        "Example Quest (USA)",
        "Example Quest (USA).nes",
        &hash_of(&payload(4, 64)),
    );
    let src = source(&b, None);
    let wrong = payload(5, 64);
    let staged = stage(&b, "NES/Example Quest (USA).nes", &wrong);
    let id = hand_off(&b, rom, src, 0, &staged);
    let row = settled(&b, id, state::BAD).await;
    assert_eq!(row.state, state::BAD);
    assert!(!staged.exists());
    let dir = staging(&b).join("quarantine").join(infohash());
    assert_eq!(
        std::fs::read(dir.join("Example Quest (USA).nes")).expect("kept"),
        wrong
    );
    let report =
        std::fs::read_to_string(dir.join("Example Quest (USA).nes.report.txt")).expect("report");
    assert!(
        report.contains("Expected: Example Quest (USA).nes (64 bytes)"),
        "{report}"
    );
    assert!(report.contains(&hash_of(&wrong).sha1), "{report}");
    assert!(!games(&b).join(NES_TARGET).exists());
    let entries = log(&b);
    assert_eq!(entries[0].action, "quarantined");
    assert_eq!(entries[0].detail["expected"]["id"], json!(rom));
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_existing_verified_target_is_kept() {
    let b = boot().await;
    let body = payload(6, 256);
    let (_, rom) = entry(
        &b,
        "nes",
        "Example Quest (USA)",
        "Example Quest (USA).nes",
        &hash_of(&body),
    );
    let mut on_disk = ines_bytes();
    on_disk.extend_from_slice(&body);
    let kept = existing_file(&b, NES_TARGET, &on_disk, Some(rom), FileState::Verified);
    let src = source(&b, None);
    let staged = stage(&b, "NES/Example Quest (USA).nes", &on_disk);
    let id = hand_off(&b, rom, src, 0, &staged);
    settled(&b, id, state::DONE).await;
    assert!(!staged.exists(), "the new copy is discarded");
    assert_eq!(
        std::fs::read(games(&b).join(NES_TARGET)).expect("kept"),
        on_disk
    );
    let entries = log(&b);
    assert_eq!(entries[0].action, "skipped_existing");
    assert_eq!(entries[0].file_id, Some(kept.0));
    assert_eq!(file_at(&b, "nes", NES_TARGET).expect("row").mtime, 1);
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unverified_target_is_replaced() {
    let b = boot().await;
    let body = payload(7, 256);
    let (_, rom) = entry(
        &b,
        "nes",
        "Example Quest (USA)",
        "Example Quest (USA).nes",
        &hash_of(&body),
    );
    let before = existing_file(
        &b,
        NES_TARGET,
        b"not the entry",
        None,
        FileState::Unverified,
    );
    let mut good = ines_bytes();
    good.extend_from_slice(&body);
    let src = source(&b, None);
    let staged = stage(&b, "NES/Example Quest (USA).nes", &good);
    let id = hand_off(&b, rom, src, 0, &staged);
    settled(&b, id, state::DONE).await;
    assert_eq!(
        std::fs::read(games(&b).join(NES_TARGET)).expect("placed"),
        good
    );
    let row = file_at(&b, "nes", NES_TARGET).expect("row");
    assert_eq!((row.id, row.state), (before, FileState::Verified));
    let entries = log(&b);
    assert_eq!(entries[0].action, "replaced");
    assert_eq!(entries[0].detail["previous"]["state"], "unverified");
    assert_eq!(entries[0].detail["previous"]["rel_path"], NES_TARGET);
    b.running.shutdown().await.expect("shutdown");
}

/// Seeds a three-track disc entry and stages its tracks; returns the rom ids.
fn disc(b: &Booted) -> (i64, Vec<(i64, PathBuf)>) {
    let names = [
        "Example Disc (USA).cue",
        "Example Disc (USA) (Track 1).bin",
        "Example Disc (USA) (Track 2).bin",
    ];
    let data: Vec<Vec<u8>> = (0u8..3)
        .map(|i| payload(10 + i, 512 + usize::from(i)))
        .collect();
    let pid = PlatformId("psx".into());
    let hashes: Vec<HashSet> = data.iter().map(|d| hash_of(d)).collect();
    let (title, roms) = b
        .running
        .app
        .db
        .write_blocking(move |c| {
            let title = files::seed_title_fixture(c, &pid, "Example Disc (USA)")?;
            let mut roms = Vec::new();
            for (name, h) in names.iter().zip(&hashes) {
                roms.push(files::seed_rom_for_title_fixture(
                    c, title, name, h, "good",
                )?);
            }
            Ok((title, roms))
        })
        .expect("seed");
    let staged = roms
        .into_iter()
        .zip(names.iter().zip(&data))
        .map(|(rom, (name, d))| (rom, stage(b, &format!("Example Disc (USA)/{name}"), d)))
        .collect();
    (title, staged)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_three_track_disc_is_placed_as_a_directory() {
    let b = boot().await;
    let (_, tracks) = disc(&b);
    let src = source(&b, None);
    let ids: Vec<DownloadId> = tracks
        .iter()
        .zip(0u32..)
        .map(|((rom, path), i)| insert(&b, *rom, src, i, path))
        .collect();
    for id in &ids {
        announce(&b, *id);
    }
    for id in &ids {
        settled(&b, *id, state::DONE).await;
    }
    let dir = games(&b).join("PSX/Example Disc (USA)");
    for (_, path) in &tracks {
        assert!(!path.exists());
        let name = path.file_name().expect("name");
        assert!(dir.join(name).is_file(), "{}", name.to_string_lossy());
    }
    let placed = log(&b).into_iter().filter(|l| l.action == "placed").count();
    assert_eq!(placed, 3);
    let cue = file_at(&b, "psx", "PSX/Example Disc (USA)/Example Disc (USA).cue").expect("row");
    assert_eq!(cue.state, FileState::Verified);
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_partial_disc_stays_in_staging() {
    let b = boot().await;
    let (_, tracks) = disc(&b);
    let src = source(&b, None);
    let ids: Vec<DownloadId> = tracks[..2]
        .iter()
        .zip(0u32..)
        .map(|((rom, path), i)| insert(&b, *rom, src, i, path))
        .collect();
    for id in &ids {
        announce(&b, *id);
    }
    for id in &ids {
        let row = settled(&b, *id, state::FAILED).await;
        assert_eq!(row.state, state::FAILED);
    }
    let error: Option<String> = b
        .running
        .app
        .db
        .read_blocking(|c| {
            Ok(
                c.query_row("SELECT error FROM downloads ORDER BY id LIMIT 1", [], |r| {
                    r.get(0)
                })?,
            )
        })
        .expect("error");
    assert!(error.expect("reason").contains("only 2 of the 3 tracks"));
    assert!(tracks[..2].iter().all(|(_, p)| p.exists()));
    assert!(!games(&b).join("PSX/Example Disc (USA)").exists());
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bios_entry_is_refused() {
    let b = boot().await;
    let body = payload(8, 128);
    let (title, rom) = entry(
        &b,
        "nes",
        "[BIOS] Example System (World)",
        "[BIOS] Example System (World).nes",
        &hash_of(&body),
    );
    b.running
        .app
        .db
        .write_blocking(move |c| {
            c.execute(
                "UPDATE titles SET flags = '[\"bios\"]' WHERE id = ?1",
                [title],
            )?;
            Ok(())
        })
        .expect("flag");
    let src = source(&b, None);
    let staged = stage(&b, "NES/system.nes", &body);
    let id = hand_off(&b, rom, src, 0, &staged);
    settled(&b, id, state::FAILED).await;
    let error: Option<String> = b
        .running
        .app
        .db
        .read_blocking(move |c| {
            Ok(
                c.query_row("SELECT error FROM downloads WHERE id = ?1", [id.0], |r| {
                    r.get(0)
                })?,
            )
        })
        .expect("error");
    assert_eq!(error.as_deref(), Some("BIOS entries are never imported"));
    assert!(staged.exists());
    assert!(!games(&b).join("NES").exists());
    assert!(log(&b).is_empty());
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rename_gives_a_misnamed_file_its_canonical_name() {
    let b = boot().await;
    let body = payload(9, 256);
    let (title, rom) = entry(
        &b,
        "nes",
        "Example Quest (USA)",
        "Example Quest (USA).nes",
        &hash_of(&body),
    );
    let mut data = ines_bytes();
    data.extend_from_slice(&body);
    let file = existing_file(
        &b,
        "NES/example quest.nes",
        &data,
        Some(rom),
        FileState::Misnamed,
    );
    let path = format!("/api/v1/titles/{title}/rename");
    let body_json = json!({ "file_id": file.0 }).to_string();
    let r = request(b.addr(), "POST", &path, &[], Some(&body_json)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["variants"][0]["roms"][0]["file_path"], NES_TARGET);
    assert!(!games(&b).join("NES/example quest.nes").exists());
    assert_eq!(
        std::fs::read(games(&b).join(NES_TARGET)).expect("renamed"),
        data
    );
    let row = file_at(&b, "nes", NES_TARGET).expect("row");
    assert_eq!((row.id, row.state), (file, FileState::Verified));
    let entries = log(&b);
    assert_eq!(entries[0].action, "renamed");
    assert_eq!(entries[0].detail["from"], "NES/example quest.nes");
    let again = request(b.addr(), "POST", &path, &[], Some(&body_json)).await;
    assert_eq!(again.status, 400, "{}", again.body);
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn imports_are_listed_newest_first_with_paging() {
    let b = boot().await;
    b.running
        .app
        .db
        .write_blocking(|c| {
            for (i, action) in [
                ImportAction::Placed,
                ImportAction::Replaced,
                ImportAction::SkippedExisting,
            ]
            .into_iter()
            .enumerate()
            {
                imports::log(c, 100, None, None, action, &json!({ "n": i }))?;
            }
            Ok(())
        })
        .expect("log");
    let page = get(b.addr(), "/api/v1/imports?limit=2").await;
    assert_eq!(page.status, 200);
    let page = page.json();
    assert_eq!(page["total"], 3);
    let actions: Vec<&str> = page["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|i| i["action"].as_str().expect("action"))
        .collect();
    assert_eq!(actions, ["skipped_existing", "replaced"]);
    let rest = get(b.addr(), "/api/v1/imports?limit=2&offset=2")
        .await
        .json();
    assert_eq!(rest["items"][0]["action"], "placed");
    assert_eq!(rest["items"][0]["detail"], json!({ "n": 0 }));
    assert_eq!(get(b.addr(), "/api/v1/imports?limit=x").await.status, 400);
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_finished_torrent_without_seeding_leaves_the_client() {
    let fake = FakeServer::start().await.expect("fake");
    fake.push(FakeResponse::success(json!({ "version": "4.0.5" })));
    fake.push(FakeResponse::success(json!({ "torrents": [{ "id": 1 }] })));
    fake.push(FakeResponse::success(json!({})));
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = config_in(dir.path());
    config.client.url = fake.url();
    let b = boot_with(dir, config).await;
    assert!(b.running.app.client().is_some());
    let body = payload(12, 512);
    let (_, rom) = entry(
        &b,
        "gba",
        "Example Quest (USA)",
        "Example Quest (USA).gba",
        &hash_of(&body),
    );
    let hash = infohash();
    let src = source(&b, Some(&hash));
    b.running
        .app
        .db
        .write_blocking(move |c| sources::set_seed_policy(c, src, &SeedPolicy::None))
        .expect("policy");
    let staged = stage(&b, "GBA/Example Quest (USA).gba", &body);
    let id = hand_off(&b, rom, src, 0, &staged);
    settled(&b, id, state::DONE).await;
    eventually("the torrent removed", || async {
        fake.bodies()
            .iter()
            .any(|v| v["method"] == "torrent-remove")
    })
    .await;
    let remove: Vec<Value> = fake
        .bodies()
        .into_iter()
        .filter(|v| v["method"] == "torrent-remove")
        .collect();
    assert_eq!(remove[0]["arguments"]["delete-local-data"], false);
    assert_eq!(remove[0]["arguments"]["ids"], json!([hash]));
    let staging_dir = staging(&b).join(infohash());
    eventually("the staging directory cleaned", || async {
        !staging_dir.exists()
    })
    .await;
    let client_id = b
        .running
        .app
        .db
        .read_blocking(move |c| sources::get(c, src))
        .expect("source")
        .expect("row")
        .client_id;
    assert_eq!(client_id, None);
    b.running.shutdown().await.expect("shutdown");
}
