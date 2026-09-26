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

/// A minimal multi-member, stored-method (uncompressed) zip, so this test
/// binary does not need its own copy of the `zip` crate.
fn build_stored_zip_multi(entries: &[(&str, &[u8], &str)]) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut central = Vec::new();
    for (name, data, crc32_hex) in entries {
        let crc32 = u32::from_str_radix(crc32_hex, 16).expect("hex crc32");
        let name = name.as_bytes();
        let local_offset = u32::try_from(buf.len()).expect("offset fits");
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

        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&crc32.to_le_bytes());
        central.extend_from_slice(&(data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        central.extend_from_slice(&[0u8; 8]);
        central.extend_from_slice(&0u32.to_le_bytes());
        central.extend_from_slice(&local_offset.to_le_bytes());
        central.extend_from_slice(name);
    }

    let cd_start = u32::try_from(buf.len()).expect("offset fits");
    let cd_size = u32::try_from(central.len()).expect("size fits");
    buf.extend_from_slice(&central);

    let count = u16::try_from(entries.len()).expect("entry count fits");
    buf.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&count.to_le_bytes());
    buf.extend_from_slice(&count.to_le_bytes());
    buf.extend_from_slice(&cd_size.to_le_bytes());
    buf.extend_from_slice(&cd_start.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf
}

/// A minimal single-member, stored-method (uncompressed) zip.
fn build_stored_zip(name: &str, data: &[u8], crc32_hex: &str) -> Vec<u8> {
    build_stored_zip_multi(&[(name, data, crc32_hex)])
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
) -> job_rows::JobRow {
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
    row
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
                    whole: None,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_header_ruled_member_passes_the_pre_check_by_its_content_crc() {
    let dir = tempfile::tempdir().expect("tempdir");
    let games = dir.path().join("games");

    // The DAT records the header-stripped hash while the central directory holds the
    // whole member's CRC32; the pre-check derives the content's from the header.
    let payload = b"quest payload with a header";
    let headered = ines(payload);
    let raw_hash = hash_of(&headered);
    let stripped_hash = hash_of(payload);
    let zip = build_stored_zip("Ines Quest (USA).nes", &headered, &raw_hash.crc32);
    write(&games.join("NES/Ines Quest (USA).zip"), &zip);

    let mut config = config_in(dir.path());
    config.paths.games = games.clone();
    let booted = boot_with(dir, config).await;
    let app = &booted.running.app;
    let nes = PlatformId("nes".into());

    app.db
        .write({
            let nes = nes.clone();
            let stripped_hash = stripped_hash.clone();
            move |c| {
                files::seed_rom_fixture(
                    c,
                    &nes,
                    "Ines Quest (USA)",
                    "Ines Quest (USA).nes",
                    &stripped_hash,
                    "good",
                )
            }
        })
        .await
        .expect("seed");

    scan_and_wait(app, booted.addr(), "nes").await;

    let row = find(app, &nes, "NES/Ines Quest (USA).zip#Ines Quest (USA).nes")
        .await
        .expect("row");
    assert_eq!(row.state, FileState::Verified);
    assert_eq!(row.sha1.as_deref(), Some(stripped_hash.sha1.as_str()));

    booted.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_loose_disc_title_verifies_independently() {
    let dir = tempfile::tempdir().expect("tempdir");
    let games = dir.path().join("games");

    // Two unrelated single-file games loose directly under the platform's
    // top directory (no per-title subfolder), plus a playlist that is not
    // a track and must be ignored.
    let a = hash_of(b"disc a payload");
    let b = hash_of(b"disc b payload");
    write(&games.join("PSX/Loose A (USA).iso"), b"disc a payload");
    write(&games.join("PSX/Loose B (USA).iso"), b"disc b payload");
    write(&games.join("PSX/Loose A (USA).m3u"), b"Loose A (USA).iso");

    let mut config = config_in(dir.path());
    config.paths.games = games.clone();
    let booted = boot_with(dir, config).await;
    let app = &booted.running.app;
    let pid = PlatformId("psx".into());

    app.db
        .write({
            let pid = pid.clone();
            let (a, b) = (a.clone(), b.clone());
            move |c| {
                files::seed_rom_fixture(c, &pid, "Loose A (USA)", "Loose A (USA).iso", &a, "good")?;
                files::seed_rom_fixture(c, &pid, "Loose B (USA)", "Loose B (USA).iso", &b, "good")
            }
        })
        .await
        .expect("seed");

    scan_and_wait(app, booted.addr(), "psx").await;

    let row_a = find(app, &pid, "PSX/Loose A (USA).iso").await.expect("row");
    assert_eq!(row_a.state, FileState::Verified);
    let row_b = find(app, &pid, "PSX/Loose B (USA).iso").await.expect("row");
    assert_eq!(row_b.state, FileState::Verified);
    assert!(find(app, &pid, "PSX/Loose A (USA).m3u").await.is_none());

    booted.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_corrupt_zip_does_not_abort_the_platform_scan() {
    let dir = tempfile::tempdir().expect("tempdir");
    let games = dir.path().join("games");

    write(&games.join("GBA/Corrupt.zip"), b"not a zip file at all");
    let good = hash_of(b"good payload");
    write(&games.join("GBA/Good.gba"), b"good payload");

    let mut config = config_in(dir.path());
    config.paths.games = games.clone();
    let booted = boot_with(dir, config).await;
    let app = &booted.running.app;
    let gba = PlatformId("gba".into());

    app.db
        .write({
            let gba = gba.clone();
            move |c| files::seed_rom_fixture(c, &gba, "Good (USA)", "Good.gba", &good, "good")
        })
        .await
        .expect("seed");

    scan_and_wait(app, booted.addr(), "gba").await;

    let good_row = find(app, &gba, "GBA/Good.gba").await.expect("row");
    assert_eq!(good_row.state, FileState::Verified);

    let corrupt_row = find(app, &gba, "GBA/Corrupt.zip").await.expect("row");
    assert_eq!(corrupt_row.state, FileState::Unverified);

    booted.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn zip_directory_entries_are_not_recorded_as_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let games = dir.path().join("games");

    let payload = b"zipped payload";
    let hash = hash_of(payload);
    let zip = build_stored_zip_multi(&[
        ("sub/", b"", "00000000"),
        ("sub/Dir Quest (USA).gba", payload, &hash.crc32),
    ]);
    write(&games.join("GBA/Dir Quest (USA).zip"), &zip);

    let mut config = config_in(dir.path());
    config.paths.games = games.clone();
    let booted = boot_with(dir, config).await;
    let app = &booted.running.app;
    let gba = PlatformId("gba".into());

    app.db
        .write({
            let gba = gba.clone();
            move |c| {
                files::seed_rom_fixture(
                    c,
                    &gba,
                    "Dir Quest (USA)",
                    "sub/Dir Quest (USA).gba",
                    &hash,
                    "good",
                )
            }
        })
        .await
        .expect("seed");

    scan_and_wait(app, booted.addr(), "gba").await;

    let member = find(app, &gba, "GBA/Dir Quest (USA).zip#sub/Dir Quest (USA).gba")
        .await
        .expect("row");
    assert_eq!(member.state, FileState::Verified);
    assert!(find(app, &gba, "GBA/Dir Quest (USA).zip#sub/")
        .await
        .is_none());

    booted.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unchanged_disc_track_reuses_its_cached_hash() {
    let dir = tempfile::tempdir().expect("tempdir");
    let games = dir.path().join("games");
    let track_path = games.join("PSX/Cached Quest (USA).iso");
    let original = b"cached track payload";
    write(&track_path, original);

    let mut config = config_in(dir.path());
    config.paths.games = games.clone();
    let booted = boot_with(dir, config).await;
    let app = &booted.running.app;
    let pid = PlatformId("psx".into());
    let hash = hash_of(original);

    app.db
        .write({
            let pid = pid.clone();
            let hash = hash.clone();
            move |c| {
                files::seed_rom_fixture(
                    c,
                    &pid,
                    "Cached Quest (USA)",
                    "Cached Quest (USA).iso",
                    &hash,
                    "good",
                )
            }
        })
        .await
        .expect("seed");

    scan_and_wait(app, booted.addr(), "psx").await;
    let row = find(app, &pid, "PSX/Cached Quest (USA).iso")
        .await
        .expect("row");
    assert_eq!(row.state, FileState::Verified);
    let original_mtime = std::fs::metadata(&track_path)
        .expect("meta")
        .modified()
        .expect("mtime");

    // Same size, different bytes, but the mtime is restored: a correct
    // implementation must trust the cached hash rather than re-read this.
    assert_eq!(original.len(), 20);
    std::fs::write(&track_path, b"XXXXXXXXXXXXXXXXXXXX").expect("corrupt");
    std::fs::File::options()
        .write(true)
        .open(&track_path)
        .expect("open")
        .set_modified(original_mtime)
        .expect("set mtime");

    scan_and_wait(app, booted.addr(), "psx").await;
    let row = find(app, &pid, "PSX/Cached Quest (USA).iso")
        .await
        .expect("row");
    assert_eq!(
        row.state,
        FileState::Verified,
        "cached hash should be reused"
    );
    assert_eq!(row.sha1.as_deref(), Some(hash.sha1.as_str()));

    booted.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scan_of_a_large_file_does_not_block_other_writes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let games = dir.path().join("games");
    write(&games.join("GBA/Big.gba"), &vec![0xABu8; 60 * 1024 * 1024]);

    let mut config = config_in(dir.path());
    config.paths.games = games.clone();
    let booted = boot_with(dir, config).await;
    let addr = booted.addr();

    let body = post_scan(addr, Some("gba")).await;
    let job_id = JobId(body["job_id"].as_i64().expect("job_id"));

    let settings_body =
        r#"{"limits":{"down_kbps_menu":1,"down_kbps_core":1,"up_kbps_menu":1,"up_kbps_core":1}}"#;
    let settings_response = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        request(
            addr,
            "PUT",
            "/api/v1/system/settings",
            &[],
            Some(settings_body),
        ),
    )
    .await
    .expect("a settings write must not wait for the scan's hashing to finish");
    assert_eq!(settings_response.status, 200, "{}", settings_response.body);

    let app = &booted.running.app;
    eventually("scan job to finish", || async move {
        app.db
            .read(move |c| job_rows::get(c, job_id))
            .await
            .ok()
            .flatten()
            .is_some_and(|r| r.state.is_finished())
    })
    .await;

    booted.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scan_rejects_unknown_or_disabled_platforms() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = config_in(dir.path());
    let booted = boot_with(dir, config).await;
    let addr = booted.addr();
    let app = &booted.running.app;

    let r = request(
        addr,
        "POST",
        "/api/v1/system/scan",
        &[],
        Some(r#"{"platform_id":"no-such-platform"}"#),
    )
    .await;
    assert_eq!(r.status, 404, "{}", r.body);
    assert_eq!(r.json()["error"]["code"], "not_found");

    app.db
        .write(|c| {
            c.execute("UPDATE platforms SET enabled = 0 WHERE id = 'nes'", [])?;
            Ok(())
        })
        .await
        .expect("disable");

    let r = request(
        addr,
        "POST",
        "/api/v1/system/scan",
        &[],
        Some(r#"{"platform_id":"nes"}"#),
    )
    .await;
    assert_eq!(r.status, 400, "{}", r.body);
    assert_eq!(r.json()["error"]["code"], "bad_request");

    booted.running.shutdown().await.expect("shutdown");
}

/// A big-endian N64 image: the byte order mark the N64 rule detects, then `payload`.
fn z64(payload: &[u8]) -> Vec<u8> {
    let mut v = vec![0x80, 0x37, 0x12, 0x40];
    v.extend_from_slice(payload);
    v
}

/// `data` with each pair of bytes swapped, the byte-swapped N64 order.
fn swap16(data: &[u8]) -> Vec<u8> {
    data.chunks(2)
        .flat_map(|p| p.iter().rev().copied())
        .collect()
}

/// A Logiqx DAT named `name` with one game per `(game, rom, hashes)`.
fn logiqx(name: &str, games: &[(&str, &str, &HashSet)]) -> String {
    logiqx_version(name, "1", games)
}

/// [`logiqx`] at `version`.
fn logiqx_version(name: &str, version: &str, games: &[(&str, &str, &HashSet)]) -> String {
    let mut xml =
        format!("<datafile><header><name>{name}</name><version>{version}</version></header>");
    for (game, rom, h) in games {
        xml.push_str(&format!(
            "<game name=\"{game}\"><rom name=\"{rom}\" size=\"{}\" crc=\"{}\" md5=\"{}\" sha1=\"{}\"/></game>",
            h.size, h.crc32, h.md5, h.sha1
        ));
    }
    xml.push_str("</datafile>");
    xml
}

/// Loads `xml` as a DAT dropped into `dats/` and waits for its import job.
async fn load_dat(booted: &common::Booted, file: &str, xml: &str) {
    let app = &booted.running.app;
    let path = app.config().paths.data.join("dats").join(file);
    write(&path, xml.as_bytes());
    let job = std::sync::Arc::new(mistarr_server::jobs::dat_import::DatImport::new(&path));
    let id = mistarr_server::jobs::Scheduler::enqueue(app, job)
        .await
        .expect("enqueue");
    eventually("the DAT import", || async move {
        app.db
            .read(move |c| job_rows::get(c, id))
            .await
            .ok()
            .flatten()
            .is_some_and(|r| r.state == JobState::Done)
    })
    .await;
}

async fn platform_counts(addr: std::net::SocketAddr, id: &str) -> serde_json::Value {
    let r = request(addr, "GET", "/api/v1/platforms", &[], None).await;
    assert_eq!(r.status, 200, "{}", r.body);
    r.json()["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|p| p["id"] == id)
        .map(|p| p["counts"].clone())
        .expect("platform")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dat_loaded_after_the_first_scan_matches_the_files_already_there() {
    let dir = tempfile::tempdir().expect("tempdir");
    let games = dir.path().join("games");
    let quest = z64(b"n64 quest payload, big-endian...");
    write(&games.join("N64/Example Quest (USA).z64"), &quest);
    let swapped = z64(b"n64 manor payload, swapped order");
    write(&games.join("N64/Mock Manor (USA).v64"), &swap16(&swapped));
    write(
        &games.join("N64/Stray.z64"),
        &z64(b"listed by no DAT at all........."),
    );
    let nes_payload = b"nes payload behind an ines header";
    write(
        &games.join("NES/Header Quest (USA).nes"),
        &ines(nes_payload),
    );

    let mut config = config_in(dir.path());
    config.paths.games = games.clone();
    let booted = boot_with(dir, config).await;
    let (app, addr) = (&booted.running.app, booted.addr());
    let (n64, nes) = (PlatformId("n64".into()), PlatformId("nes".into()));

    scan_and_wait(app, addr, "n64").await;
    scan_and_wait(app, addr, "nes").await;
    let before = find(app, &n64, "N64/Mock Manor (USA).v64")
        .await
        .expect("row");
    assert_eq!((before.rom_id, before.state), (None, FileState::Unverified));
    assert_eq!(platform_counts(addr, "n64").await["unmatched_files"], 3);

    // A running core holds the heavy lane, so the scans the loads queue wait and only
    // the background recompute can match the files.
    std::fs::write(booted.corename(), "N64\n").expect("corename");
    eventually("the heavy lane to close", || async {
        app.gate.state().core_running()
    })
    .await;

    let (quest_h, manor_h) = (hash_of(&quest), hash_of(&swapped));
    let n64_dat = logiqx(
        "Example Vendor - Nintendo 64",
        &[
            ("Example Quest (USA)", "Example Quest (USA).z64", &quest_h),
            ("Mock Manor (USA)", "Mock Manor (USA).z64", &manor_h),
        ],
    );
    load_dat(&booted, "n64.dat", &n64_dat).await;
    let nes_h = hash_of(nes_payload);
    let nes_dat = logiqx(
        "Example Vendor - Nintendo Entertainment System",
        &[("Header Quest (USA)", "Header Quest (USA).nes", &nes_h)],
    );
    load_dat(&booted, "nes.dat", &nes_dat).await;

    eventually("the N64 files to match", || async {
        let counts = platform_counts(addr, "n64").await;
        counts["have"] == 1 && counts["unmatched_files"] == 1
    })
    .await;
    let state = |rel: &'static str, pid: PlatformId| async move {
        find(app, &pid, rel).await.map(|r| r.state)
    };
    assert_eq!(
        state("N64/Example Quest (USA).z64", n64.clone()).await,
        Some(FileState::Verified)
    );
    assert_eq!(
        state("N64/Mock Manor (USA).v64", n64.clone()).await,
        Some(FileState::Misnamed),
        "stored hashes are of the big-endian form the DAT lists"
    );
    assert_eq!(
        state("N64/Stray.z64", n64).await,
        Some(FileState::Unverified)
    );
    eventually("the headered NES file to match", || async {
        platform_counts(addr, "nes").await["have"] == 1
    })
    .await;
    let open_scans = app
        .db
        .read(|c| {
            let open = job_rows::open_rows(c)?;
            Ok(open.iter().filter(|j| j.kind == "scan").count())
        })
        .await
        .expect("jobs");
    assert_eq!(
        open_scans, 2,
        "both automatic scans still wait for the core"
    );
    assert_eq!(
        state("NES/Header Quest (USA).nes", nes).await,
        Some(FileState::Verified)
    );
    let r = request(
        addr,
        "GET",
        "/api/v1/platforms/n64/titles?have=yes",
        &[],
        None,
    )
    .await;
    assert_eq!(r.json()["total"], 1, "{}", r.body);

    booted.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rescan_matches_unchanged_unmatched_files_without_hashing_them() {
    let dir = tempfile::tempdir().expect("tempdir");
    let games = dir.path().join("games");
    let path = games.join("GAMEBOY/Example Quest (USA).gb");
    let original = b"gb payload scanned before any DAT";
    write(&path, original);

    let mut config = config_in(dir.path());
    config.paths.games = games.clone();
    let booted = boot_with(dir, config).await;
    let (app, addr) = (&booted.running.app, booted.addr());
    let gb = PlatformId("gb".into());

    let first = scan_and_wait(app, addr, "gb").await;
    let progress = first.progress.expect("progress");
    assert_eq!(
        (&progress["matched"], &progress["unmatched"]),
        (&0.into(), &1.into())
    );
    let mtime = std::fs::metadata(&path)
        .expect("meta")
        .modified()
        .expect("mtime");

    let hash = hash_of(original);
    app.db
        .write({
            let (gb, hash) = (gb.clone(), hash.clone());
            move |c| {
                let name = "Example Quest (USA)";
                files::seed_rom_fixture(c, &gb, name, "Example Quest (USA).gb", &hash, "good")
            }
        })
        .await
        .expect("seed");

    // Same size and mtime, other bytes: only the stored hashes can match now.
    std::fs::write(&path, vec![b'X'; original.len()]).expect("overwrite");
    std::fs::File::options()
        .write(true)
        .open(&path)
        .expect("open")
        .set_modified(mtime)
        .expect("set mtime");

    let second = scan_and_wait(app, addr, "gb").await;
    let progress = second.progress.expect("progress");
    assert_eq!(
        (&progress["matched"], &progress["unmatched"]),
        (&1.into(), &0.into())
    );
    let row = find(app, &gb, "GAMEBOY/Example Quest (USA).gb")
        .await
        .expect("row");
    assert_eq!(row.state, FileState::Verified);
    assert!(row.rom_id.is_some());
    assert_eq!(
        row.sha1.as_deref(),
        Some(hash.sha1.as_str()),
        "not hashed again"
    );

    let r = request(addr, "GET", "/api/v1/system/jobs/recent", &[], None).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let recent = r.json();
    assert_eq!(recent["items"][0]["id"], second.id.0, "newest first");
    assert_eq!(recent["items"][0]["progress"]["matched"], 1);
    assert_eq!(recent["items"][1]["id"], first.id.0);

    booted.running.shutdown().await.expect("shutdown");
}

/// Writes `data` over `path` and gives it `mtime` back, so a scan sees it unchanged.
fn rewrite_keeping_mtime(path: &Path, data: &[u8]) {
    let mtime = std::fs::metadata(path)
        .expect("meta")
        .modified()
        .expect("mtime");
    std::fs::write(path, data).expect("rewrite");
    std::fs::File::options()
        .write(true)
        .open(path)
        .expect("open")
        .set_modified(mtime)
        .expect("set mtime");
}

/// `zip` with its first member flagged as encrypted, local and central: it lists but
/// cannot be read without a password.
fn encrypted(zip: &[u8]) -> Vec<u8> {
    let mut out = zip.to_vec();
    out[6] |= 1;
    let central = 0x0201_4b50u32.to_le_bytes();
    let at = out
        .windows(4)
        .position(|w| w == central)
        .expect("central directory");
    out[at + 8] |= 1;
    out
}

/// A rom whose CRC32 and size are `h`'s but whose sha1 and md5 are other, synthetic ones.
fn same_crc_other_sha1(h: &HashSet) -> HashSet {
    HashSet {
        size: h.size,
        crc32: h.crc32.clone(),
        md5: "7".repeat(32),
        sha1: "7".repeat(40),
    }
}

async fn seed_gba_rom(app: &mistarr_server::app::AppState, name: &str, h: &HashSet) {
    let (name, h) = (name.to_owned(), h.clone());
    app.db
        .write(move |c| {
            let gba = PlatformId("gba".into());
            files::seed_rom_fixture(c, &gba, &name, &format!("{name}.gba"), &h, "good")
        })
        .await
        .expect("seed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_crc_only_member_is_hashed_once_a_candidate_appears_and_never_again() {
    let dir = tempfile::tempdir().expect("tempdir");
    let games = dir.path().join("games");
    let zip_path = games.join("GBA/Crc Quest (USA).zip");
    let payload = b"gba member payload known by crc only";
    let h = hash_of(payload);
    let zip = build_stored_zip("Crc Quest (USA).gba", payload, &h.crc32);
    write(&zip_path, &zip);

    let mut config = config_in(dir.path());
    config.paths.games = games.clone();
    let booted = boot_with(dir, config).await;
    let (app, addr) = (&booted.running.app, booted.addr());
    let gba = PlatformId("gba".into());
    let rel = "GBA/Crc Quest (USA).zip#Crc Quest (USA).gba";

    scan_and_wait(app, addr, "gba").await;
    let row = find(app, &gba, rel).await.expect("row");
    assert_eq!(
        (row.sha1.as_deref(), row.header_rule.as_deref()),
        (None, None)
    );

    // The candidate shares the CRC32 and size but not the sha1: the CRC32 decides nothing.
    seed_gba_rom(app, "Crc Quest (USA)", &same_crc_other_sha1(&h)).await;
    let recompute = mistarr_server::jobs::dat_import::Recompute::new("gba");
    mistarr_server::jobs::Scheduler::run_inline(app, std::sync::Arc::new(recompute))
        .await
        .expect("recompute");
    let row = find(app, &gba, rel).await.expect("row");
    assert_eq!((row.rom_id, row.state), (None, FileState::Unverified));
    assert!(
        row.sha1.is_none(),
        "the recompute never matches a CRC32 alone"
    );

    scan_and_wait(app, addr, "gba").await;
    let row = find(app, &gba, rel).await.expect("row");
    assert_eq!(row.sha1.as_deref(), Some(h.sha1.as_str()), "hashed once");
    assert_eq!(row.state, FileState::Unverified);

    // Other bytes of the same length: a second hash would store another sha1.
    let other = vec![b'Z'; payload.len()];
    rewrite_keeping_mtime(
        &zip_path,
        &build_stored_zip("Crc Quest (USA).gba", &other, &h.crc32),
    );
    scan_and_wait(app, addr, "gba").await;
    let row = find(app, &gba, rel).await.expect("row");
    assert_eq!(
        row.sha1.as_deref(),
        Some(h.sha1.as_str()),
        "never hashed again"
    );

    booted.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_member_that_fails_to_hash_is_not_retried_while_unchanged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let games = dir.path().join("games");
    let zip_path = games.join("GBA/Odd Quest (USA).zip");
    let payload = b"gba member behind a password flag";
    let h = hash_of(payload);
    let stored = build_stored_zip("Odd Quest (USA).gba", payload, &h.crc32);
    write(&zip_path, &encrypted(&stored));

    let mut config = config_in(dir.path());
    config.paths.games = games.clone();
    let booted = boot_with(dir, config).await;
    let (app, addr) = (&booted.running.app, booted.addr());
    let gba = PlatformId("gba".into());
    let rel = "GBA/Odd Quest (USA).zip#Odd Quest (USA).gba";

    scan_and_wait(app, addr, "gba").await;
    seed_gba_rom(app, "Odd Quest (USA)", &h).await;
    scan_and_wait(app, addr, "gba").await;
    let row = find(app, &gba, rel).await.expect("row");
    assert_eq!(
        row.header_rule.as_deref(),
        Some("none"),
        "the failed attempt is recorded"
    );
    assert!(row.sha1.is_none());

    // Now readable at the same size and mtime: a retry would hash and verify it.
    rewrite_keeping_mtime(&zip_path, &stored);
    scan_and_wait(app, addr, "gba").await;
    let row = find(app, &gba, rel).await.expect("row");
    assert_eq!((row.sha1, row.state), (None, FileState::Unverified));

    booted.running.shutdown().await.expect("shutdown");
}

/// A platform whose rule strips a header, as `docs/PLATFORMS.md` "Header rules" lists it.
struct HeaderPlatform {
    id: &'static str,
    dir: &'static str,
    ext: &'static str,
    dat: &'static str,
    header: fn() -> Vec<u8>,
}

fn ines_header() -> Vec<u8> {
    ines(&[])
}

fn a78_header() -> Vec<u8> {
    let mut h = vec![1];
    h.extend_from_slice(b"ATARI7800");
    h.resize(128, 0);
    h
}

fn lnx_header() -> Vec<u8> {
    let mut h = b"LYNX".to_vec();
    h.resize(64, 0);
    h
}

const NES: HeaderPlatform = HeaderPlatform {
    id: "nes",
    dir: "NES",
    ext: "nes",
    dat: "Example Vendor - Nintendo Entertainment System",
    header: ines_header,
};

const A7800: HeaderPlatform = HeaderPlatform {
    id: "atari7800",
    dir: "Atari7800",
    ext: "a78",
    dat: "Example Vendor - Atari 7800",
    header: a78_header,
};

const LYNX: HeaderPlatform = HeaderPlatform {
    id: "lynx",
    dir: "AtariLynx",
    ext: "lnx",
    dat: "Example Vendor - Atari Lynx",
    header: lnx_header,
};

impl HeaderPlatform {
    /// A synthetic body, distinct per `seed`, that never starts with a header's magic.
    fn body(&self, seed: &str) -> Vec<u8> {
        format!("{seed} body of a synthetic {} cartridge. ", self.id)
            .into_bytes()
            .repeat(9)
    }

    fn headered(&self, body: &[u8]) -> Vec<u8> {
        let mut v = (self.header)();
        v.extend_from_slice(body);
        v
    }

    fn name(&self, title: &str) -> String {
        format!("{title} Quest (USA).{}", self.ext)
    }

    fn rel(&self, file: &str) -> String {
        format!("{}/{file}", self.dir)
    }
}

/// Scans a headered file, a headered zip member, a headerless file and a file without the
/// header's magic against a headered DAT of `p`, or a headerless one, and checks each state.
async fn both_forms_match(p: &HeaderPlatform, headered_dat: bool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let games = dir.path().join("games");
    let (loose, member, bare, plain) = (
        p.body("loose"),
        p.body("member"),
        p.body("bare"),
        p.body("plain"),
    );
    let loose_file = p.headered(&loose);
    let member_file = p.headered(&member);
    write(&games.join(p.rel(&p.name("Loose"))), &loose_file);
    let zip = build_stored_zip(
        &p.name("Member"),
        &member_file,
        &hash_of(&member_file).crc32,
    );
    write(&games.join(p.rel("Member Quest (USA).zip")), &zip);
    write(&games.join(p.rel(&p.name("Bare"))), &bare);
    write(&games.join(p.rel(&p.name("Plain"))), &plain);

    let mut config = config_in(dir.path());
    config.paths.games = games.clone();
    let booted = boot_with(dir, config).await;
    let (app, addr) = (&booted.running.app, booted.addr());
    let pid = PlatformId(p.id.into());

    let listed = |body: &[u8]| {
        if headered_dat {
            hash_of(&p.headered(body))
        } else {
            hash_of(body)
        }
    };
    let header_len = (p.header)().len();
    let (loose_h, member_h, bare_h) = (listed(&loose), listed(&member), listed(&bare));
    // A file without the magic is only ever its whole self; its tail must match nothing.
    let (plain_h, plain_tail_h) = (hash_of(&plain), hash_of(&plain[header_len..]));
    let names = [
        p.name("Loose"),
        p.name("Member"),
        p.name("Bare"),
        p.name("Plain"),
        p.name("Tail"),
    ];
    let dat = logiqx(
        p.dat,
        &[
            ("Loose Quest (USA)", &names[0], &loose_h),
            ("Member Quest (USA)", &names[1], &member_h),
            ("Bare Quest (USA)", &names[2], &bare_h),
            ("Plain Quest (USA)", &names[3], &plain_h),
            ("Tail Quest (USA)", &names[4], &plain_tail_h),
        ],
    );
    load_dat(&booted, &format!("{}.dat", p.id), &dat).await;
    scan_and_wait(app, addr, p.id).await;

    let state = |rel: String| {
        let pid = pid.clone();
        async move { find(app, &pid, &rel).await.map(|r| r.state) }
    };
    let kind = if headered_dat {
        "headered"
    } else {
        "headerless"
    };
    let loose_row = find(app, &pid, &p.rel(&names[0])).await.expect("row");
    assert_eq!(
        loose_row.state,
        FileState::Verified,
        "{} loose, {kind} DAT",
        p.id
    );
    assert_eq!(
        loose_row.sha1,
        Some(hash_of(&loose).sha1),
        "the content form"
    );
    assert_eq!(
        loose_row.whole.sha1,
        Some(hash_of(&loose_file).sha1),
        "the whole form"
    );
    let member_rel = format!("{}#{}", p.rel("Member Quest (USA).zip"), names[1]);
    assert_eq!(
        state(member_rel).await,
        Some(FileState::Verified),
        "{} member, {kind} DAT",
        p.id
    );
    let bare_state = if headered_dat {
        FileState::Unverified
    } else {
        FileState::Verified
    };
    assert_eq!(
        state(p.rel(&names[2])).await,
        Some(bare_state),
        "{} headerless file, {kind} DAT",
        p.id
    );
    assert_eq!(
        state(p.rel(&names[3])).await,
        Some(FileState::Verified),
        "{} file without magic matches its whole form only, {kind} DAT",
        p.id
    );
    let plain_row = find(app, &pid, &p.rel(&names[3])).await.expect("row");
    assert_eq!(
        plain_row.whole.sha1, plain_row.sha1,
        "one form without a header"
    );

    booted.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nes_files_match_a_headered_dat() {
    both_forms_match(&NES, true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nes_files_match_a_headerless_dat() {
    both_forms_match(&NES, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn atari_7800_files_match_a_headered_dat() {
    both_forms_match(&A7800, true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn atari_7800_files_match_a_headerless_dat() {
    both_forms_match(&A7800, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lynx_files_match_a_headered_dat() {
    both_forms_match(&LYNX, true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lynx_files_match_a_headerless_dat() {
    both_forms_match(&LYNX, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_zip_whose_members_match_neither_form_is_not_decompressed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let games = dir.path().join("games");
    let p = &NES;
    let file = p.headered(&p.body("stray"));
    let zip = build_stored_zip(&p.name("Stray"), &file, &hash_of(&file).crc32);
    write(&games.join(p.rel("Stray Quest (USA).zip")), &zip);

    let mut config = config_in(dir.path());
    config.paths.games = games.clone();
    let booted = boot_with(dir, config).await;
    let (app, addr) = (&booted.running.app, booted.addr());
    let other = hash_of(&p.headered(&p.body("listed")));
    load_dat(
        &booted,
        "nes.dat",
        &logiqx(p.dat, &[("Listed Quest (USA)", &p.name("Listed"), &other)]),
    )
    .await;
    scan_and_wait(app, addr, p.id).await;

    let rel = format!("{}#{}", p.rel("Stray Quest (USA).zip"), p.name("Stray"));
    let row = find(app, &PlatformId(p.id.into()), &rel)
        .await
        .expect("row");
    assert_eq!(
        (row.md5, row.sha1, row.header_rule),
        (None, None, None),
        "not decompressed"
    );
    let content_crc = hash_of(&p.body("stray")).crc32;
    assert_eq!(
        row.crc32,
        Some(content_crc),
        "the content CRC32 from the header alone"
    );
    assert_eq!(row.whole.crc32, Some(hash_of(&file).crc32));

    // A later headerless DAT listing it makes the next scan hash it.
    let listed = hash_of(&p.body("stray"));
    let dat = logiqx_version(
        p.dat,
        "2",
        &[("Stray Quest (USA)", &p.name("Stray"), &listed)],
    );
    load_dat(&booted, "nes.dat", &dat).await;
    scan_and_wait(app, addr, p.id).await;
    let row = find(app, &PlatformId(p.id.into()), &rel)
        .await
        .expect("row");
    assert_eq!(row.state, FileState::Verified);

    booted.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stored_forms_match_a_later_dat_in_both_directions() {
    let dir = tempfile::tempdir().expect("tempdir");
    let games = dir.path().join("games");
    let p = &NES;
    let titles = ["Whole", "Content", "Whole Member", "Content Member"];
    let bodies: Vec<Vec<u8>> = titles.iter().map(|t| p.body(t)).collect();
    for (title, body) in titles.iter().zip(&bodies) {
        let file = p.headered(body);
        if title.ends_with("Member") {
            let zip = build_stored_zip(&p.name(title), &file, &hash_of(&file).crc32);
            write(
                &games.join(p.rel(&format!("{title} Quest (USA).zip"))),
                &zip,
            );
        } else {
            write(&games.join(p.rel(&p.name(title))), &file);
        }
    }

    let mut config = config_in(dir.path());
    config.paths.games = games.clone();
    let booted = boot_with(dir, config).await;
    let (app, addr) = (&booted.running.app, booted.addr());
    scan_and_wait(app, addr, p.id).await;
    assert_eq!(platform_counts(addr, p.id).await["unmatched_files"], 4);

    // A running core holds the heavy lane: only the recompute can match the files.
    std::fs::write(booted.corename(), "NES\n").expect("corename");
    eventually("the heavy lane to close", || async {
        app.gate.state().core_running()
    })
    .await;
    let hashes: Vec<HashSet> = titles
        .iter()
        .zip(&bodies)
        .map(|(t, b)| {
            if t.starts_with("Whole") {
                hash_of(&p.headered(b))
            } else {
                hash_of(b)
            }
        })
        .collect();
    let names: Vec<String> = titles.iter().map(|t| p.name(t)).collect();
    let games_listed: Vec<(String, &str, &HashSet)> = titles
        .iter()
        .zip(&names)
        .zip(&hashes)
        .map(|((t, n), h)| (format!("{t} Quest (USA)"), n.as_str(), h))
        .collect();
    let entries: Vec<(&str, &str, &HashSet)> = games_listed
        .iter()
        .map(|(g, n, h)| (g.as_str(), *n, *h))
        .collect();
    load_dat(&booted, "nes.dat", &logiqx(p.dat, &entries)).await;
    eventually("the stored forms of the loose files to match", || async {
        platform_counts(addr, p.id).await["have"] == 2
    })
    .await;
    let pid = PlatformId(p.id.into());
    for title in ["Whole", "Content"] {
        let row = find(app, &pid, &p.rel(&p.name(title))).await.expect("row");
        assert_eq!(row.state, FileState::Verified, "{title}");
    }
    let open_scans = app
        .db
        .read(|c| {
            Ok(job_rows::open_rows(c)?
                .iter()
                .filter(|j| j.kind == "scan")
                .count())
        })
        .await
        .expect("jobs");
    assert_eq!(open_scans, 1, "the scan the load queued still waits");

    // Members known by their CRC32 alone wait for that scan, which the pre-check lets hash them.
    std::fs::write(booted.corename(), "MENU\n").expect("corename");
    eventually("the members to match", || async {
        platform_counts(addr, p.id).await["have"] == 4
    })
    .await;

    booted.running.shutdown().await.expect("shutdown");
}

/// Size and mtime as a scan stores them.
fn meta(path: &Path) -> (i64, i64) {
    let m = std::fs::metadata(path).expect("meta");
    let mtime = m
        .modified()
        .expect("mtime")
        .duration_since(std::time::UNIX_EPOCH)
        .expect("epoch")
        .as_secs();
    (
        i64::try_from(m.len()).expect("size"),
        i64::try_from(mtime).expect("mtime"),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rows_hashed_before_the_whole_form_are_hashed_again_after_the_upgrade() {
    let dir = tempfile::tempdir().expect("tempdir");
    let games = dir.path().join("games");
    let p = &NES;
    let body = p.body("upgraded");
    let file = p.headered(&body);
    let path = games.join(p.rel(&p.name("Upgraded")));
    write(&path, &file);
    let (size, mtime) = meta(&path);

    let mut config = config_in(dir.path());
    config.paths.games = games.clone();
    let booted = boot_with(dir, config).await;
    let app = &booted.running.app;
    let pid = PlatformId(p.id.into());
    let rel = p.rel(&p.name("Upgraded"));
    let (whole, content) = (hash_of(&file), hash_of(&body));
    app.db
        .write({
            let (pid, rel, whole, content) =
                (pid.clone(), rel.clone(), whole.clone(), content.clone());
            let name = p.name("Upgraded");
            move |c| {
                files::seed_rom_fixture(c, &pid, "Upgraded Quest (USA)", &name, &whole, "good")?;
                // The stripped form alone, as a scan stored it before the whole-file columns.
                let hashed = files::Hashed {
                    crc32: Some(&content.crc32),
                    md5: Some(&content.md5),
                    sha1: Some(&content.sha1),
                    header_rule: Some("ines"),
                    whole: None,
                };
                files::upsert(
                    c,
                    &pid,
                    &rel,
                    size,
                    mtime,
                    &hashed,
                    None,
                    FileState::Unverified,
                    1,
                )
                .map(|_| ())
            }
        })
        .await
        .expect("seed");
    let dir = booted.dir;
    booted.running.shutdown().await.expect("shutdown");

    // Back to the schema before the whole-file columns, so booting runs that migration.
    let db = rusqlite::Connection::open(config_in(dir.path()).paths.db()).expect("open");
    db.execute_batch(
        "ALTER TABLE files DROP COLUMN crc32_whole;
         ALTER TABLE files DROP COLUMN md5_whole;
         ALTER TABLE files DROP COLUMN sha1_whole;
         DELETE FROM schema_version WHERE version = 20;",
    )
    .expect("downgrade");
    drop(db);

    let mut config = config_in(dir.path());
    config.paths.games = games;
    let upgraded = boot_with(dir, config).await;
    let app = &upgraded.running.app;
    eventually("the scan queued by the upgrade to match the file", || {
        let (pid, rel) = (pid.clone(), rel.clone());
        async move {
            find(app, &pid, &rel)
                .await
                .is_some_and(|r| r.state == FileState::Verified)
        }
    })
    .await;
    let row = find(app, &pid, &rel).await.expect("row");
    assert_eq!(row.whole.sha1, Some(whole.sha1));
    assert_eq!(row.sha1, Some(content.sha1));

    upgraded.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_headered_dat_after_a_headerless_one_matches_its_files_again() {
    let dir = tempfile::tempdir().expect("tempdir");
    let games = dir.path().join("games");
    let p = &NES;
    let (loose, member, gone) = (p.body("loose"), p.body("member"), p.body("gone"));
    let member_file = p.headered(&member);
    write(&games.join(p.rel(&p.name("Loose"))), &p.headered(&loose));
    let zip = build_stored_zip(
        &p.name("Member"),
        &member_file,
        &hash_of(&member_file).crc32,
    );
    write(&games.join(p.rel("Member Quest (USA).zip")), &zip);
    write(&games.join(p.rel(&p.name("Gone"))), &p.headered(&gone));

    let mut config = config_in(dir.path());
    config.paths.games = games.clone();
    let booted = boot_with(dir, config).await;
    let (app, addr) = (&booted.running.app, booted.addr());
    let pid = PlatformId(p.id.into());
    let names = [p.name("Loose"), p.name("Member"), p.name("Gone")];
    let dat = |version: &str, hashes: [HashSet; 3]| {
        let games = [
            ("Loose Quest (USA)", names[0].as_str(), &hashes[0]),
            ("Member Quest (USA)", names[1].as_str(), &hashes[1]),
            ("Gone Quest (USA)", names[2].as_str(), &hashes[2]),
        ];
        logiqx_version(p.dat, version, &games)
    };
    let headerless = [hash_of(&loose), hash_of(&member), hash_of(&gone)];
    load_dat(&booted, "nes.dat", &dat("1", headerless)).await;
    scan_and_wait(app, addr, p.id).await;
    let member_rel = format!("{}#{}", p.rel("Member Quest (USA).zip"), names[1]);
    let rels = [p.rel(&names[0]), member_rel, p.rel(&names[2])];
    let mut roms = Vec::new();
    for rel in &rels {
        let row = find(app, &pid, rel).await.expect("row");
        assert_eq!(
            row.state,
            FileState::Verified,
            "{rel} under the headerless DAT"
        );
        roms.push(row.rom_id);
    }

    // The next version lists the same roms by their headered hashes, and one by another dump.
    std::fs::write(booted.corename(), "NES\n").expect("corename");
    eventually("the heavy lane to close", || async {
        app.gate.state().core_running()
    })
    .await;
    let other_dump = hash_of(&p.headered(&p.body("another dump")));
    let headered = [
        hash_of(&p.headered(&loose)),
        hash_of(&member_file),
        other_dump,
    ];
    load_dat(&booted, "nes.dat", &dat("2", headered)).await;
    let want = vec![
        Some((FileState::Verified, roms[0])),
        Some((FileState::Verified, roms[1])),
        Some((FileState::Unverified, None)),
    ];
    eventually("the files to match the rewritten roms again", || async {
        let mut got = Vec::new();
        for rel in &rels {
            got.push(find(app, &pid, rel).await.map(|r| (r.state, r.rom_id)));
        }
        got == want
    })
    .await;

    booted.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn zipped_nes_files_matched_by_a_headerless_dat_count_as_have() {
    let dir = tempfile::tempdir().expect("tempdir");
    let games = dir.path().join("games");
    let names = ["Example Quest (USA)", "Mock Manor (Europe)"];
    let payloads: Vec<Vec<u8>> = names
        .iter()
        .map(|n| format!("synthetic cartridge payload of {n}").into_bytes())
        .collect();
    for (name, payload) in names.iter().zip(&payloads) {
        let image = ines(payload);
        let zip = build_stored_zip(&format!("{name}.nes"), &image, &hash_of(&image).crc32);
        write(&games.join(format!("NES/{name}.zip")), &zip);
    }
    let headered: Vec<HashSet> = payloads.iter().map(|p| hash_of(&ines(p))).collect();
    let headerless: Vec<HashSet> = payloads.iter().map(|p| hash_of(p)).collect();

    let mut config = config_in(dir.path());
    config.paths.games = games;
    let booted = boot_with(dir, config).await;
    let (app, addr) = (&booted.running.app, booted.addr());
    let nes = PlatformId("nes".into());

    let dat = |marker: &str, ext: &str, hashes: &[HashSet]| {
        let roms: Vec<String> = names.iter().map(|n| format!("{n}.{ext}")).collect();
        let games: Vec<(&str, &str, &HashSet)> = names
            .iter()
            .zip(&roms)
            .zip(hashes)
            .map(|((n, r), h)| (*n, r.as_str(), h))
            .collect();
        logiqx(
            &format!("Example Vendor - Nintendo Entertainment System ({marker})"),
            &games,
        )
    };
    load_dat(&booted, "headered.dat", &dat("Headered", "nes", &headered)).await;
    // The headerless form of the same family replaces it and names its roms `.unh`.
    let headerless_dat = dat("Headerless", "unh", &headerless);
    load_dat(&booted, "headerless.dat", &headerless_dat).await;

    let scan = scan_and_wait(app, addr, "nes").await;
    let progress = scan.progress.expect("progress");
    assert_eq!(
        (&progress["matched"], &progress["unmatched"]),
        (&2.into(), &0.into())
    );
    for name in names {
        let row = find(app, &nes, &format!("NES/{name}.zip#{name}.nes"))
            .await
            .expect("row");
        assert_eq!(row.state, FileState::Verified, "{name}");
    }
    let counts = platform_counts(addr, "nes").await;
    assert_eq!((&counts["have"], &counts["titles"]), (&2.into(), &2.into()));
    let r = request(
        addr,
        "GET",
        "/api/v1/platforms/nes/titles?have=yes",
        &[],
        None,
    )
    .await;
    assert_eq!(r.json()["total"], 2, "{}", r.body);

    booted.running.shutdown().await.expect("shutdown");
}
