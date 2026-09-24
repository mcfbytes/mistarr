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
use mistarr_server::db::downloads::{self, DownloadId, DownloadRow, DownloadState};
use mistarr_server::db::downloads_import;
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
            downloads_import::insert_fixture(c, rom_id, src, index, "importing", Some(&staged))
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

fn download(b: &Booted, id: DownloadId) -> DownloadRow {
    b.running
        .app
        .db
        .read_blocking(move |c| downloads::get(c, id))
        .expect("read")
        .expect("row")
}

async fn settled(b: &Booted, id: DownloadId, want: DownloadState) -> DownloadRow {
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
    settled(&b, id, DownloadState::Done).await;
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
    settled(&b, id, DownloadState::Done).await;
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
    settled(&b, id, DownloadState::Done).await;
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
    let row = settled(&b, id, DownloadState::Bad).await;
    assert_eq!(row.state, DownloadState::Bad);
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
async fn a_transfer_whose_dat_was_removed_is_quarantined_saying_so() {
    let b = boot().await;
    let good = payload(4, 64);
    let (title, rom) = entry(
        &b,
        "nes",
        "Example Quest (USA)",
        "Example Quest (USA).nes",
        &hash_of(&good),
    );
    b.running
        .app
        .db
        .write_blocking(move |c| {
            let version: i64 = c.query_row(
                "SELECT dat_version_id FROM titles WHERE id = ?1",
                [title],
                |r| r.get(0),
            )?;
            mistarr_server::db::dats::retire(
                c,
                mistarr_server::db::dats::DatVersionId(version),
                1,
            )?;
            Ok(())
        })
        .expect("remove");
    let src = source(&b, None);
    let staged = stage(&b, "NES/Example Quest (USA).nes", &good);
    let id = hand_off(&b, rom, src, 0, &staged);
    let row = settled(&b, id, DownloadState::Bad).await;
    assert!(
        row.error
            .as_deref()
            .is_some_and(|e| e.contains("was removed")),
        "{row:?}"
    );
    assert!(!games(&b).join(NES_TARGET).exists());
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
    settled(&b, id, DownloadState::Done).await;
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
    settled(&b, id, DownloadState::Done).await;
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
    let data: Vec<Vec<u8>> = (0u8..3)
        .map(|i| payload(10 + i, 512 + usize::from(i)))
        .collect();
    disc_with(b, data)
}

const DISC_NAMES: [&str; 3] = [
    "Example Disc (USA).cue",
    "Example Disc (USA) (Track 1).bin",
    "Example Disc (USA) (Track 2).bin",
];

/// Seeds a disc entry whose three tracks hold `data` and stages them.
fn disc_with(b: &Booted, data: Vec<Vec<u8>>) -> (i64, Vec<(i64, PathBuf)>) {
    let names = DISC_NAMES;
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
        settled(&b, *id, DownloadState::Done).await;
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
        let row = settled(&b, *id, DownloadState::Failed).await;
        assert_eq!(row.state, DownloadState::Failed);
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
    settled(&b, id, DownloadState::Failed).await;
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

fn row_error(b: &Booted, id: DownloadId) -> String {
    download(b, id).error.unwrap_or_default()
}

fn set_importing(b: &Booted, ids: &[DownloadId]) {
    let ids: Vec<i64> = ids.iter().map(|i| i.0).collect();
    b.running
        .app
        .db
        .write_blocking(move |c| {
            for id in ids {
                c.execute(
                    "UPDATE downloads SET state = 'importing', error = NULL WHERE id = ?1",
                    [id],
                )?;
            }
            Ok(())
        })
        .expect("reset");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn byte_identical_variants_place_the_wanted_one() {
    let b = boot().await;
    let body = payload(20, 512);
    let (_, europe) = entry(
        &b,
        "nes",
        "Example Quest (Europe)",
        "Example Quest (Europe).nes",
        &hash_of(&body),
    );
    set_header(&b, europe, INES);
    let (_, usa) = entry(
        &b,
        "nes",
        "Example Quest (USA)",
        "Example Quest (USA).nes",
        &hash_of(&body),
    );
    set_header(&b, usa, INES);
    let src = source(&b, None);
    let staged = stage(&b, "NES/quest.nes", &body);
    let id = hand_off(&b, europe, src, 0, &staged);
    settled(&b, id, DownloadState::Done).await;
    let rel = "NES/Example Quest (Europe).nes";
    assert!(games(&b).join(rel).is_file());
    assert!(!games(&b).join(NES_TARGET).exists());
    assert_eq!(file_at(&b, "nes", rel).expect("row").rom_id, Some(europe));
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_file_of_another_entry_is_quarantined_naming_it() {
    let b = boot().await;
    let (_, wanted) = entry(
        &b,
        "nes",
        "Example Quest (USA)",
        "Example Quest (USA).nes",
        &hash_of(&payload(21, 64)),
    );
    let other = payload(22, 64);
    entry(
        &b,
        "nes",
        "Other Quest (USA)",
        "Other Quest (USA).nes",
        &hash_of(&other),
    );
    let src = source(&b, None);
    let staged = stage(&b, "NES/quest.nes", &other);
    let id = hand_off(&b, wanted, src, 0, &staged);
    settled(&b, id, DownloadState::Bad).await;
    assert!(row_error(&b, id).contains("Other Quest (USA)"));
    let report = staging(&b)
        .join("quarantine")
        .join(infohash())
        .join("quest.nes.report.txt");
    let text = std::fs::read_to_string(report).expect("report");
    assert!(
        text.contains("Matches instead: Other Quest (USA) (Other Quest (USA).nes)"),
        "{text}"
    );
    assert!(!games(&b).join("NES").exists());
    assert_eq!(log(&b)[0].detail["other"]["title"], "Other Quest (USA)");
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn identical_tracks_of_one_disc_are_each_placed() {
    let b = boot().await;
    let same = payload(23, 600);
    let (_, tracks) = disc_with(&b, vec![payload(24, 90), same.clone(), same]);
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
        settled(&b, *id, DownloadState::Done).await;
    }
    let dir = games(&b).join("PSX/Example Disc (USA)");
    for name in DISC_NAMES {
        assert!(dir.join(name).is_file(), "{name}");
    }
    b.running.shutdown().await.expect("shutdown");
}

/// Seeds a two-rom Neo Geo entry and stages a zip of `members`.
fn romset(b: &Booted, members: &[(&str, &[u8])]) -> (Vec<i64>, PathBuf) {
    let pid = PlatformId("neogeo".into());
    let (ha, hb) = (hash_of(&payload(30, 256)), hash_of(&payload(31, 128)));
    let roms = b
        .running
        .app
        .db
        .write_blocking(move |c| {
            let title = files::seed_title_fixture(c, &pid, "Example Set")?;
            Ok(vec![
                files::seed_rom_for_title_fixture(c, title, "a.rom", &ha, "good")?,
                files::seed_rom_for_title_fixture(c, title, "b.rom", &hb, "good")?,
            ])
        })
        .expect("seed");
    let mut buf = Vec::new();
    let mut z = zip::ZipWriter::new(Cursor::new(&mut buf));
    for (name, data) in members {
        z.start_file(*name, zip::write::SimpleFileOptions::default())
            .expect("start");
        z.write_all(data).expect("write");
    }
    z.finish().expect("finish");
    (roms, stage(b, "NeoGeo/Example Set.zip", &buf))
}

fn member_rows(b: &Booted) -> Vec<files::FileRow> {
    b.running
        .app
        .db
        .read_blocking(|c| {
            files::zip_member_rows(c, &PlatformId("neogeo".into()), "NeoGeo/Example Set.zip")
        })
        .expect("rows")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_romset_is_verified_member_by_member_and_agrees_with_the_scan() {
    let b = boot().await;
    let (a, bb) = (payload(30, 256), payload(31, 128));
    let (roms, staged) = romset(&b, &[("a.rom", &a), ("b.rom", &bb)]);
    let src = source(&b, None);
    let ids: Vec<DownloadId> = roms
        .iter()
        .map(|r| insert(&b, *r, src, 0, &staged))
        .collect();
    announce(&b, ids[0]);
    for id in &ids {
        settled(&b, *id, DownloadState::Done).await;
    }
    assert!(games(&b).join("NeoGeo/Example Set.zip").is_file());
    let before = member_rows(&b);
    assert_eq!(before.len(), 2);
    assert!(before.iter().all(|r| r.state == FileState::Verified));
    assert!(file_at(&b, "neogeo", "NeoGeo/Example Set.zip").is_none());
    let scan = std::sync::Arc::new(mistarr_server::jobs::scan::ScanJob {
        platform_id: Some(PlatformId("neogeo".into())),
    });
    mistarr_server::jobs::Scheduler::run_inline(&b.running.app, scan)
        .await
        .expect("scan");
    let key = |rows: Vec<files::FileRow>| {
        rows.into_iter()
            .map(|r| (r.id, r.state, r.scanned_at))
            .collect::<Vec<_>>()
    };
    assert_eq!(key(member_rows(&b)), key(before));
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_romset_with_one_bad_member_is_quarantined() {
    let b = boot().await;
    let a = payload(30, 256);
    let (roms, staged) = romset(&b, &[("a.rom", &a), ("b.rom", b"not the rom")]);
    let src = source(&b, None);
    let ids: Vec<DownloadId> = roms
        .iter()
        .map(|r| insert(&b, *r, src, 0, &staged))
        .collect();
    announce(&b, ids[0]);
    settled(&b, ids[0], DownloadState::Bad).await;
    settled(&b, ids[1], DownloadState::Failed).await;
    assert!(!games(&b).join("NeoGeo").exists());
    let quarantined = staging(&b)
        .join("quarantine")
        .join(infohash())
        .join("Example Set.zip");
    assert!(quarantined.is_file());
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failure_before_the_renames_changes_nothing_and_can_be_retried() {
    let b = boot().await;
    let body = payload(40, 256);
    let (_, rom) = entry(
        &b,
        "nes",
        "Example Quest (USA)",
        "Example Quest (USA).nes",
        &hash_of(&body),
    );
    set_header(&b, rom, INES);
    let src = source(&b, None);
    let blocker = staging(&b).join(".import");
    write(&blocker, b"in the way");
    let staged = stage(&b, "NES/quest.nes", &body);
    let id = hand_off(&b, rom, src, 0, &staged);
    settled(&b, id, DownloadState::Failed).await;
    assert!(
        row_error(&b, id).contains("nothing was changed"),
        "{}",
        row_error(&b, id)
    );
    assert_eq!(std::fs::read(&staged).expect("untouched"), body);
    assert!(!games(&b).join(NES_TARGET).exists());
    std::fs::remove_file(&blocker).expect("rm");
    set_importing(&b, &[id]);
    announce(&b, id);
    settled(&b, id, DownloadState::Done).await;
    assert!(games(&b).join(NES_TARGET).is_file());
    assert!(!staging(&b).join(".import").join(id.0.to_string()).exists());
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn renames_that_landed_are_kept_and_a_retry_finishes() {
    let b = boot().await;
    let (_, tracks) = disc(&b);
    let src = source(&b, None);
    let dir = games(&b).join("PSX/Example Disc (USA)");
    let blocker = dir.join(DISC_NAMES[2]);
    write(&blocker.join("inside"), b"x");
    let ids: Vec<DownloadId> = tracks
        .iter()
        .zip(0u32..)
        .map(|((rom, path), i)| insert(&b, *rom, src, i, path))
        .collect();
    for id in &ids {
        announce(&b, *id);
    }
    for id in &ids {
        settled(&b, *id, DownloadState::Failed).await;
    }
    assert!(
        row_error(&b, ids[0]).contains("after 2 of 3"),
        "{}",
        row_error(&b, ids[0])
    );
    for name in &DISC_NAMES[..2] {
        let rel = format!("PSX/Example Disc (USA)/{name}");
        assert_eq!(
            file_at(&b, "psx", &rel).expect("landed").state,
            FileState::Verified
        );
    }
    assert!(tracks[2].1.exists());
    std::fs::remove_dir_all(&blocker).expect("rm");
    set_importing(&b, &ids);
    announce(&b, ids[0]);
    for id in &ids {
        settled(&b, *id, DownloadState::Done).await;
    }
    for name in DISC_NAMES {
        assert!(dir.join(name).is_file(), "{name}");
    }
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rename_clears_a_stale_row_at_the_destination() {
    let b = boot().await;
    let body = payload(41, 256);
    let (title, rom) = entry(
        &b,
        "nes",
        "Example Quest (USA)",
        "Example Quest (USA).nes",
        &hash_of(&body),
    );
    let stale = existing_file(&b, NES_TARGET, b"gone", None, FileState::Unverified);
    std::fs::remove_file(games(&b).join(NES_TARGET)).expect("rm");
    let mut data = ines_bytes();
    data.extend_from_slice(&body);
    let file = existing_file(&b, "NES/quest.nes", &data, Some(rom), FileState::Misnamed);
    let path = format!("/api/v1/titles/{title}/rename");
    let body_json = json!({ "file_id": file.0 }).to_string();
    let r = request(b.addr(), "POST", &path, &[], Some(&body_json)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let row = file_at(&b, "nes", NES_TARGET).expect("row");
    assert_eq!(row.id, file);
    assert_ne!(row.id, stale);
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rename_answers_500_when_the_file_cannot_be_read() {
    let b = boot().await;
    let body = payload(42, 256);
    let (title, rom) = entry(
        &b,
        "nes",
        "Example Quest (USA)",
        "Example Quest (USA).nes",
        &hash_of(&body),
    );
    let file = existing_file(&b, "NES/quest.nes", &body, Some(rom), FileState::Misnamed);
    std::fs::remove_file(games(&b).join("NES/quest.nes")).expect("rm");
    let path = format!("/api/v1/titles/{title}/rename");
    let body_json = json!({ "file_id": file.0 }).to_string();
    let r = request(b.addr(), "POST", &path, &[], Some(&body_json)).await;
    assert_eq!(r.status, 500, "{}", r.body);
    assert_eq!(r.json()["error"]["code"], "internal");
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_entering_importing_enqueues_and_once_per_download() {
    let b = boot().await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let r = request(b.addr(), "POST", "/api/v1/system/pause", &[], Some("{}")).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let body = payload(43, 128);
    let (_, rom) = entry(
        &b,
        "gba",
        "Example Quest (USA)",
        "Example Quest (USA).gba",
        &hash_of(&body),
    );
    let src = source(&b, None);
    let staged = stage(&b, "GBA/quest.gba", &body);
    let id = insert(&b, rom, src, 0, &staged);
    let publish = |state: &str| {
        b.running.app.events.publish(
            EventKind::DownloadChanged,
            &json!({ "download_id": id.0, "state": state, "progress": 0.5 }),
        );
    };
    let jobs = |b: &Booted| {
        b.running
            .app
            .db
            .read_blocking(|c| mistarr_server::db::jobs::count_kind(c, "import"))
            .expect("count")
    };
    publish("transferring");
    publish("checking");
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(jobs(&b), 0);
    for _ in 0..3 {
        publish("importing");
    }
    eventually("an import job", || async { jobs(&b) == 1 }).await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(jobs(&b), 1);
    let r = request(b.addr(), "POST", "/api/v1/system/resume", &[], Some("{}")).await;
    assert_eq!(r.status, 200, "{}", r.body);
    settled(&b, id, DownloadState::Done).await;
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
    settled(&b, id, DownloadState::Done).await;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_quarantine_that_settles_the_source_releases_the_torrent() {
    let fake = FakeServer::start().await.expect("fake");
    fake.push(FakeResponse::success(json!({ "version": "4.0.5" })));
    fake.push(FakeResponse::success(json!({ "torrents": [{ "id": 1 }] })));
    fake.push(FakeResponse::success(json!({})));
    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = config_in(dir.path());
    config.client.url = fake.url();
    let b = boot_with(dir, config).await;
    let (_, placed) = entry(
        &b,
        "gba",
        "Example Quest (USA)",
        "Example Quest (USA).gba",
        &hash_of(&payload(13, 512)),
    );
    let (_, wrong) = entry(
        &b,
        "gba",
        "Sample Saga (USA)",
        "Sample Saga (USA).gba",
        &hash_of(&payload(14, 512)),
    );
    let hash = infohash();
    let src = source(&b, Some(&hash));
    b.running
        .app
        .db
        .write_blocking(move |c| {
            downloads_import::insert_fixture(c, placed, src, 0, "done", None)?;
            sources::set_seed_policy(c, src, &SeedPolicy::None)
        })
        .expect("done download");
    let staged = stage(&b, "GBA/Sample Saga (USA).gba", &payload(15, 512));
    let id = hand_off(&b, wrong, src, 1, &staged);
    settled(&b, id, DownloadState::Bad).await;
    eventually("the torrent removed", || async {
        fake.bodies()
            .iter()
            .any(|v| v["method"] == "torrent-remove")
    })
    .await;
    b.running.shutdown().await.expect("shutdown");
}
