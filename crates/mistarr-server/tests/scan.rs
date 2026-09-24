//! End-to-end library scan tests: real files under a tempdir games tree,
//! scanned through the booted server. See `docs/ARCHITECTURE.md` "Library
//! scan" and `docs/VERIFICATION.md`.

mod common;

use std::io::Cursor;
use std::path::Path;

use common::{boot_with, config_in, eventually, request, Sse};
use mistarr_core::hash::{hash_reader, HeaderRule};
use mistarr_core::{HashSet, PlatformId};
use mistarr_server::db::files::{self, FileState};
use mistarr_server::db::jobs::{self as job_rows, JobId, JobState};

fn write(path: &Path, data: &[u8]) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, data).expect("write");
}

fn hash_of(data: &[u8]) -> HashSet {
    hash_reader(Cursor::new(data), HeaderRule::None, None).expect("hash")
}

fn ines(payload: &[u8]) -> Vec<u8> {
    let mut v = b"NES\x1a".to_vec();
    v.resize(16, 0);
    v.extend_from_slice(payload);
    v
}

/// An SMC file with a 512-byte copier header; `payload.len()` must be a
/// multiple of 1024 for the header to be recognised.
fn smc(payload: &[u8]) -> Vec<u8> {
    let mut v = vec![0xAAu8; 512];
    v.extend_from_slice(payload);
    v
}

/// A minimal single-member, stored-method (uncompressed) zip, so this test
/// binary does not need its own copy of the `zip` crate.
fn build_stored_zip(name: &str, data: &[u8], crc32_hex: &str) -> Vec<u8> {
    let crc32 = u32::from_str_radix(crc32_hex, 16).expect("hex crc32");
    let name = name.as_bytes();
    let mut buf = Vec::new();
    let local_offset = 0u32;
    buf.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
    buf.extend_from_slice(&20u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes()); // method: stored
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&crc32.to_le_bytes());
    buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
    buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
    buf.extend_from_slice(&(name.len() as u16).to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(name);
    buf.extend_from_slice(data);

    let cd_start = buf.len() as u32;
    buf.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
    buf.extend_from_slice(&20u16.to_le_bytes());
    buf.extend_from_slice(&20u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&crc32.to_le_bytes());
    buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
    buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
    buf.extend_from_slice(&(name.len() as u16).to_le_bytes());
    buf.extend_from_slice(&[0u8; 8]);
    buf.extend_from_slice(&0u32.to_le_bytes());
    buf.extend_from_slice(&local_offset.to_le_bytes());
    buf.extend_from_slice(name);
    let cd_size = buf.len() as u32 - cd_start;

    buf.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes());
    buf.extend_from_slice(&cd_size.to_le_bytes());
    buf.extend_from_slice(&cd_start.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf
}

async fn post_scan(addr: std::net::SocketAddr, platform_id: Option<&str>) -> serde_json::Value {
    let body = platform_id.map_or_else(
        || "{}".to_owned(),
        |p| format!(r#"{{"platform_id":"{p}"}}"#),
    );
    let r = request(addr, "POST", "/api/v1/system/scan", &[], Some(&body)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    r.json()
}

/// Posts a scan for one platform and waits until its job row is finished.
async fn scan_and_wait(
    app: &mistarr_server::app::AppState,
    addr: std::net::SocketAddr,
    platform_id: &str,
) {
    let body = post_scan(addr, Some(platform_id)).await;
    let id = JobId(body["job_id"].as_i64().expect("job_id"));
    eventually("scan job to finish", || async move {
        app.db
            .read(move |c| job_rows::get(c, id))
            .await
            .ok()
            .flatten()
            .is_some_and(|r| r.state.is_finished())
    })
    .await;
    let row = app
        .db
        .read(move |c| job_rows::get(c, id))
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.state, JobState::Done, "{:?}", row.progress);
}

async fn find(
    app: &mistarr_server::app::AppState,
    pid: &PlatformId,
    rel: &str,
) -> Option<files::FileRow> {
    let (pid, rel) = (pid.clone(), rel.to_owned());
    app.db
        .read(move |c| files::find_by_path(c, &pid, &rel))
        .await
        .expect("read")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scan_matches_hashes_and_states_over_the_api() {
    let dir = tempfile::tempdir().expect("tempdir");
    let games = dir.path().join("games");

    let headered = hash_of(b"quest payload one");
    write(
        &games.join("NES/Example Quest (USA).nes"),
        &ines(b"quest payload one"),
    );

    let misnamed = hash_of(b"third payload");
    write(&games.join("NES/Weird Name.nes"), &ines(b"third payload"));

    write(&games.join("NES/Unknown.xyz"), b"not a rom");

    // A zipped cartridge, on a platform with no header rule to keep this
    // check independent from the iNES header-stripping one above.
    let zipped_payload = b"zipped payload for gba";
    let zipped_hash = hash_of(zipped_payload);
    let zip = build_stored_zip("Zip Quest (USA).gba", zipped_payload, &zipped_hash.crc32);
    write(&games.join("GBA/Zip Quest (USA).zip"), &zip);

    let snes_payload = vec![7u8; 1024];
    let snes_hash = hash_of(&snes_payload);
    write(
        &games.join("SNES/Example Quest (USA).sfc"),
        &smc(&snes_payload),
    );

    let mut config = config_in(dir.path());
    config.paths.games = games.clone();
    let booted = boot_with(dir, config).await;
    let app = &booted.running.app;
    let nes = PlatformId("nes".into());
    let gba = PlatformId("gba".into());
    let snes = PlatformId("snes".into());

    app.db
        .write({
            let (nes, gba, snes) = (nes.clone(), gba.clone(), snes.clone());
            let headered = headered.clone();
            move |c| {
                files::seed_rom_fixture(
                    c,
                    &nes,
                    "Example Quest (USA)",
                    "Example Quest (USA).nes",
                    &headered,
                    "good",
                )?;
                files::seed_rom_fixture(
                    c,
                    &nes,
                    "Correct Name (USA)",
                    "Correct Name (USA).nes",
                    &misnamed,
                    "good",
                )?;
                files::seed_rom_fixture(
                    c,
                    &gba,
                    "Zip Quest (USA)",
                    "Zip Quest (USA).gba",
                    &zipped_hash,
                    "good",
                )?;
                files::seed_rom_fixture(
                    c,
                    &snes,
                    "Example Quest (USA)",
                    "Example Quest (USA).sfc",
                    &snes_hash,
                    "good",
                )
            }
        })
        .await
        .expect("seed");

    let mut sse = Sse::open(booted.addr(), "/api/v1/events", &[]).await;
    scan_and_wait(app, booted.addr(), "nes").await;
    scan_and_wait(app, booted.addr(), "gba").await;
    scan_and_wait(app, booted.addr(), "snes").await;
    sse.until("file.changed").await;

    let headered_row = find(app, &nes, "NES/Example Quest (USA).nes")
        .await
        .expect("row");
    assert_eq!(headered_row.state, FileState::Verified);
    assert_eq!(headered_row.sha1.as_deref(), Some(headered.sha1.as_str()));

    let zip_row = find(app, &gba, "GBA/Zip Quest (USA).zip#Zip Quest (USA).gba")
        .await
        .expect("row");
    assert_eq!(zip_row.state, FileState::Verified);

    let misnamed_row = find(app, &nes, "NES/Weird Name.nes").await.expect("row");
    assert_eq!(misnamed_row.state, FileState::Misnamed);

    assert!(find(app, &nes, "NES/Unknown.xyz").await.is_none());

    let snes_row = find(app, &snes, "SNES/Example Quest (USA).sfc")
        .await
        .expect("row");
    assert_eq!(snes_row.state, FileState::Verified);
    assert_eq!(
        snes_row.size, 1536,
        "size on disk includes the copier header"
    );

    // Rescanning leaves an unchanged file's scanned_at untouched: it was skipped.
    let before = headered_row.scanned_at;
    scan_and_wait(app, booted.addr(), "nes").await;
    let after = find(app, &nes, "NES/Example Quest (USA).nes")
        .await
        .expect("row");
    assert_eq!(after.scanned_at, before);

    booted.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disc_game_is_verified_only_when_every_track_matches() {
    let dir = tempfile::tempdir().expect("tempdir");
    let games = dir.path().join("games");
    let disc_dir = games.join("PSX/Example Quest (USA)");
    let tracks: [(&str, &[u8]); 4] = [
        ("Example Quest (USA).cue", b"CUE SHEET TEXT"),
        ("Example Quest (USA) (Track 1).bin", b"track one data"),
        ("Example Quest (USA) (Track 2).bin", b"track two data"),
        ("Example Quest (USA) (Track 3).bin", b"track three data"),
    ];
    for (name, data) in tracks {
        write(&disc_dir.join(name), data);
    }

    let mut config = config_in(dir.path());
    config.paths.games = games.clone();
    let booted = boot_with(dir, config).await;
    let app = &booted.running.app;
    let pid = PlatformId("psx".into());

    app.db
        .write({
            let pid = pid.clone();
            move |c| {
                let title = files::seed_title_fixture(c, &pid, "Example Quest (USA)")?;
                for (name, data) in tracks {
                    files::seed_rom_for_title_fixture(c, title, name, &hash_of(data), "good")?;
                }
                Ok(())
            }
        })
        .await
        .expect("seed");

    scan_and_wait(app, booted.addr(), "psx").await;

    for (name, _) in tracks {
        let rel = format!("PSX/Example Quest (USA)/{name}");
        let row = find(app, &pid, &rel).await.expect("row");
        assert_eq!(row.state, FileState::Verified, "{name}");
    }

    // Delete one track: the whole game drops to unverified, and its row is gone.
    std::fs::remove_file(disc_dir.join("Example Quest (USA) (Track 3).bin")).expect("remove");
    scan_and_wait(app, booted.addr(), "psx").await;

    for (name, _) in &tracks[..3] {
        let rel = format!("PSX/Example Quest (USA)/{name}");
        let row = find(app, &pid, &rel).await.expect("row");
        assert_eq!(row.state, FileState::Unverified, "{name}");
    }
    let gone = find(
        app,
        &pid,
        "PSX/Example Quest (USA)/Example Quest (USA) (Track 3).bin",
    )
    .await;
    assert!(gone.is_none());

    booted.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scan_endpoint_enqueues_and_is_no_longer_a_stub() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = config_in(dir.path());
    let booted = boot_with(dir, config).await;
    let body = post_scan(booted.addr(), None).await;
    assert!(body.get("job_id").is_some());
    booted.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_interrupted_scan_resumes_at_startup() {
    let dir = tempfile::tempdir().expect("tempdir");
    let games = dir.path().join("games");
    let a_hash = hash_of(b"already committed");
    let b_hash = hash_of(b"not yet scanned");
    write(&games.join("Genesis/A.md"), b"already committed");
    write(&games.join("MegaDrive/B.md"), b"not yet scanned");

    let mut config = config_in(dir.path());
    config.paths.games = games.clone();
    let booted = boot_with(dir, config).await;
    let app = &booted.running.app;
    let pid = PlatformId("megadrive".into());

    app.db
        .write({
            let pid = pid.clone();
            let (a_hash, b_hash) = (a_hash.clone(), b_hash.clone());
            move |c| {
                files::seed_rom_fixture(c, &pid, "A", "A.md", &a_hash, "good")?;
                files::seed_rom_fixture(c, &pid, "B", "B.md", &b_hash, "good")?;
                // Genesis/A.md was already committed by a scan that never reached MegaDrive.
                let now = mistarr_server::unix_now();
                let hashed = files::Hashed {
                    crc32: Some(&a_hash.crc32),
                    md5: Some(&a_hash.md5),
                    sha1: Some(&a_hash.sha1),
                    header_rule: Some("none"),
                };
                files::upsert(
                    c,
                    &pid,
                    "Genesis/A.md",
                    18,
                    now,
                    &hashed,
                    None,
                    FileState::Pending,
                    now,
                )?;
                files::save_scan_progress(c, &pid, &["Genesis".to_owned()], now)
            }
        })
        .await
        .expect("seed interrupted state");

    let dir = booted.dir;
    booted.running.shutdown().await.expect("shutdown");

    let config = {
        let mut c = config_in(dir.path());
        c.paths.games = games.clone();
        c
    };
    let resumed = boot_with(dir, config).await;
    let app = &resumed.running.app;
    let pid2 = pid.clone();
    eventually("resumed scan to finish", || {
        let pid2 = pid2.clone();
        async move {
            let cleared = app
                .db
                .read({
                    let pid2 = pid2.clone();
                    move |c| files::scan_progress(c, &pid2)
                })
                .await
                .is_ok_and(|d| d.is_empty());
            cleared && find(app, &pid2, "MegaDrive/B.md").await.is_some()
        }
    })
    .await;

    let b_row = find(app, &pid, "MegaDrive/B.md").await.expect("row");
    assert_eq!(b_row.state, FileState::Verified);

    let a_row = find(app, &pid, "Genesis/A.md").await.expect("row");
    assert_eq!(
        a_row.state,
        FileState::Pending,
        "the already-committed row is left as it was"
    );

    resumed.running.shutdown().await.expect("shutdown");
}
