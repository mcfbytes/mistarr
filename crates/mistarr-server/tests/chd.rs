//! CHD identification through the booted server: the header-only scan, the `chd_tracks`
//! job, the setting, and rematching after DAT changes. See `docs/VERIFICATION.md` "CHD images".

mod common;

use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{boot_with, config_in, request, Booted};
use mistarr_core::hash::{hash_reader, HeaderRule};
use mistarr_core::{HashSet, PlatformId};
use mistarr_fixture::chd::{to_vec, write_redump_set, Codec, Kind, Spec, TrackSpec, Written};
use mistarr_server::app::AppState;
use mistarr_server::db::files::{self, FileRow, FileState};
use mistarr_server::db::jobs::{self as job_rows, JobId, JobState};
use mistarr_server::jobs::gate::Override;

/// Longest a scan or decode may take in a debug build on a shared runner.
const WAIT: Duration = Duration::from_secs(120);

fn track(kind: Kind, frames: u32, pregap: u32) -> TrackSpec {
    TrackSpec {
        kind,
        frames,
        pregap,
        pregap_stored: true,
    }
}

/// A small three-track disc: data, then two audio tracks, the first with a stored pregap.
fn disc(label: &str) -> Spec {
    Spec::new(
        label,
        vec![
            track(Kind::Mode2Raw, 60, 0),
            track(Kind::Audio, 40, 10),
            track(Kind::Audio, 30, 0),
        ],
    )
}

fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, bytes).expect("write");
}

/// Writes `spec`'s image at `path` and returns what was written.
fn image(path: &Path, spec: &Spec) -> Written {
    let (bytes, written) = to_vec(spec).expect("image");
    write(path, &bytes);
    written
}

fn cue_hash(label: &str) -> HashSet {
    hash_reader(
        Cursor::new(format!("cue sheet of {label}")),
        HeaderRule::None,
        None,
    )
    .expect("hash")
}

/// Boots with the games tree under the test's directory and `[scan] chd_tracks = on`.
async fn boot(dir: tempfile::TempDir, on: bool) -> Booted {
    let mut config = config_in(dir.path());
    config.scan.chd_tracks = on;
    boot_with(dir, config).await
}

fn games(b: &Booted) -> std::path::PathBuf {
    b.running.app.config().paths.games
}

/// Seeds a DAT title `game` on `platform` with a cue rom and one rom per track, named as
/// `write_redump_set` names them; returns the title id.
async fn seed_title(app: &AppState, platform: &str, game: &str, tracks: &[HashSet]) -> i64 {
    seed_title_with(app, platform, game, &cue_hash(game), tracks).await
}

async fn seed_title_with(
    app: &AppState,
    platform: &str,
    game: &str,
    cue: &HashSet,
    tracks: &[HashSet],
) -> i64 {
    let (pid, game, cue, tracks) = (
        PlatformId(platform.into()),
        game.to_owned(),
        cue.clone(),
        tracks.to_vec(),
    );
    app.db
        .write(move |c| {
            let t = files::seed_title_fixture(c, &pid, &game)?;
            files::seed_rom_for_title_fixture(c, t, &format!("{game}.cue"), &cue, "good")?;
            for (i, h) in tracks.iter().enumerate() {
                let name = format!("{game} (Track {:02}).bin", i + 1);
                files::seed_rom_for_title_fixture(c, t, &name, h, "good")?;
            }
            Ok(t)
        })
        .await
        .expect("seed")
}

async fn put_setting(b: &Booted, on: bool) {
    let body = format!(r#"{{"scan":{{"chd_tracks":{on}}}}}"#);
    let r = request(b.addr(), "PUT", "/api/v1/system/settings", &[], Some(&body)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["scan"]["chd_tracks"], on);
}

async fn wait_for<F, Fut>(what: &str, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let start = Instant::now();
    while !f().await {
        assert!(start.elapsed() < WAIT, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn count(app: &AppState, sql: &'static str) -> i64 {
    app.db
        .read(move |c| Ok(c.query_row(sql, [], |r| r.get(0))?))
        .await
        .expect("count")
}

/// Waits until no job is queued, running or paused.
async fn idle(app: &AppState) {
    wait_for("the queues to drain", || async {
        count(
            app,
            "SELECT COUNT(*) FROM jobs WHERE state IN ('queued', 'running', 'paused')",
        )
        .await
            == 0
    })
    .await;
}

/// Posts a scan of `platform` and waits until every job it leads to finished.
async fn scan(b: &Booted, platform: &str) {
    let body = format!(r#"{{"platform_id":"{platform}"}}"#);
    let r = request(b.addr(), "POST", "/api/v1/system/scan", &[], Some(&body)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let id = JobId(r.json()["job_id"].as_i64().expect("job_id"));
    let app = &b.running.app;
    wait_for("the scan", || async move {
        app.db
            .read(move |c| job_rows::get(c, id))
            .await
            .ok()
            .flatten()
            .is_some_and(|r| r.state.is_finished())
    })
    .await;
    idle(app).await;
}

async fn row(app: &AppState, platform: &str, rel: &str) -> Option<FileRow> {
    let (pid, rel) = (PlatformId(platform.into()), rel.to_owned());
    app.db
        .read(move |c| files::find_by_path(c, &pid, &rel))
        .await
        .expect("find")
}

/// `(rel_path, state, reason)` of every row of `platform`, by path.
async fn rows(app: &AppState, platform: &str) -> Vec<(String, FileState, Option<String>)> {
    let pid = PlatformId(platform.into());
    let paths = app
        .db
        .read(move |c| files::existing_paths(c, &pid))
        .await
        .expect("paths");
    let mut out = Vec::new();
    for p in paths {
        let r = row(app, platform, &p).await.expect("row");
        out.push((r.rel_path, r.state, r.reason));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn verified_members(container: &str, tracks: usize) -> Vec<(String, FileState, Option<String>)> {
    let mut want: Vec<_> = (1..=tracks)
        .map(|n| (format!("{container}#{n:02}"), FileState::Verified, None))
        .collect();
    want.push((format!("{container}#cue"), FileState::Verified, None));
    want
}

async fn chd_jobs(app: &AppState) -> i64 {
    count(app, "SELECT COUNT(*) FROM jobs WHERE kind = 'chd_tracks'").await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_setting_off_reads_only_the_header_and_turning_it_on_verifies_the_tracks() {
    let dir = tempfile::tempdir().expect("tempdir");
    let b = boot(dir, false).await;
    let app = &b.running.app;
    let spec = disc("g");
    let (bytes, written) = to_vec(&spec).expect("image");
    write(&games(&b).join("PSX/G/g.chd"), &bytes);
    write(&games(&b).join("PSX/H/h.chd"), &bytes[..124]);
    let title = seed_title(app, "psx", "g", &written.tracks).await;

    scan(&b, "psx").await;
    let off = |rel: &str| {
        (
            rel.to_owned(),
            FileState::Unidentified,
            Some("off".to_owned()),
        )
    };
    assert_eq!(
        rows(app, "psx").await,
        [off("PSX/G/g.chd"), off("PSX/H/h.chd")]
    );
    assert_eq!(
        chd_jobs(app).await,
        0,
        "nothing decodes while the setting is off"
    );
    assert_eq!(count(app, "SELECT COUNT(*) FROM chd_failures").await, 0);
    let listed = request(
        b.addr(),
        "GET",
        "/api/v1/platforms/psx/unidentified?limit=1",
        &[],
        None,
    )
    .await;
    assert_eq!(listed.status, 200, "{}", listed.body);
    assert_eq!(listed.json()["total"], 2);
    assert_eq!(listed.json()["items"][0]["reason"], "off");
    let missing = request(
        b.addr(),
        "GET",
        "/api/v1/platforms/none/unidentified",
        &[],
        None,
    )
    .await;
    assert_eq!(missing.status, 404);

    put_setting(&b, true).await;
    idle(app).await;
    let mut want = verified_members("PSX/G/g.chd", 3);
    want.push((
        "PSX/H/h.chd".to_owned(),
        FileState::Unidentified,
        Some("corrupt".to_owned()),
    ));
    assert_eq!(rows(app, "psx").await, want);
    let t1 = row(app, "psx", "PSX/G/g.chd#01").await.expect("track");
    assert_eq!(t1.sha1.as_deref(), Some(written.tracks[0].sha1.as_str()));
    let counts = request(b.addr(), "GET", "/api/v1/platforms", &[], None)
        .await
        .json();
    let psx = counts["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|p| p["id"] == "psx")
        .expect("psx")["counts"]
        .clone();
    assert_eq!(
        (psx["have"].as_i64(), psx["unidentified_files"].as_i64()),
        (Some(1), Some(1))
    );
    let status = request(b.addr(), "GET", "/api/v1/system/status", &[], None)
        .await
        .json();
    assert!(status["chd_decode_bytes_per_sec"]
        .as_u64()
        .is_some_and(|r| r > 0));

    let sink = Arc::new(mistarr_mister::launch::RecordingSink::new());
    app.set_command_sink(Arc::clone(&sink) as Arc<dyn mistarr_mister::launch::CommandSink>);
    write(&b.dir.path().join("_Console/PSX_20240101.rbf"), b"");
    let launch = request(
        b.addr(),
        "POST",
        &format!("/api/v1/titles/{title}/launch"),
        &[],
        None,
    )
    .await;
    assert_eq!(launch.status, 200, "{}", launch.body);
    assert_eq!(launch.json()["file"], "PSX/G/g.chd");

    // A rescan with the setting off rebuilds the members from the cache and decodes nothing.
    put_setting(&b, false).await;
    let jobs = chd_jobs(app).await;
    scan(&b, "psx").await;
    let mut want = verified_members("PSX/G/g.chd", 3);
    want.push((
        "PSX/H/h.chd".to_owned(),
        FileState::Unidentified,
        Some("corrupt".to_owned()),
    ));
    assert_eq!(rows(app, "psx").await, want);
    assert_eq!(chd_jobs(app).await, jobs);
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_chd_beside_its_bins_and_cue_leaves_both_verified() {
    let dir = tempfile::tempdir().expect("tempdir");
    let b = boot(dir, true).await;
    let app = &b.running.app;
    let spec = disc("g");
    let dir = games(&b).join("PSX/G");
    let written = image(&dir.join("g.chd"), &spec);
    let cue = write_redump_set(&spec, &dir, false).expect("bins");
    let cue_hash = hash_reader(
        std::fs::File::open(&cue).expect("cue"),
        HeaderRule::None,
        None,
    )
    .expect("hash");
    seed_title_with(app, "psx", "g", &cue_hash, &written.tracks).await;

    scan(&b, "psx").await;
    let mut want = vec![
        (
            "PSX/G/g (Track 01).bin".to_owned(),
            FileState::Verified,
            None,
        ),
        (
            "PSX/G/g (Track 02).bin".to_owned(),
            FileState::Verified,
            None,
        ),
        (
            "PSX/G/g (Track 03).bin".to_owned(),
            FileState::Verified,
            None,
        ),
        ("PSX/G/g.chd#01".to_owned(), FileState::Verified, None),
        ("PSX/G/g.chd#02".to_owned(), FileState::Verified, None),
        ("PSX/G/g.chd#03".to_owned(), FileState::Verified, None),
        ("PSX/G/g.chd#cue".to_owned(), FileState::Verified, None),
        ("PSX/G/g.cue".to_owned(), FileState::Verified, None),
    ];
    want.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(rows(app, "psx").await, want);
    b.running.shutdown().await.expect("shutdown");
}

/// A Logiqx DAT for PlayStation with one game `g` of a cue and `tracks`.
fn psx_dat(version: &str, tracks: &[HashSet]) -> String {
    let rom = |name: &str, h: &HashSet| {
        format!(
            "<rom name=\"{name}\" size=\"{}\" crc=\"{}\" md5=\"{}\" sha1=\"{}\"/>",
            h.size, h.crc32, h.md5, h.sha1
        )
    };
    let mut roms = rom("g.cue", &cue_hash("g"));
    for (i, h) in tracks.iter().enumerate() {
        roms.push_str(&rom(&format!("g (Track {:02}).bin", i + 1), h));
    }
    format!(
        "<datafile><header><name>Sony - PlayStation</name><version>{version}</version></header>\
         <game name=\"g\"><description>g</description>{roms}</game></datafile>"
    )
}

async fn load_dat(b: &Booted, file: &str, xml: &str) {
    let app = &b.running.app;
    let path = app.config().paths.data.join("dats").join(file);
    write(&path, xml.as_bytes());
    let job = Arc::new(mistarr_server::jobs::dat_import::DatImport::new(&path));
    let id = mistarr_server::jobs::Scheduler::enqueue(app, job)
        .await
        .expect("enqueue");
    wait_for("the DAT import", || async move {
        app.db
            .read(move |c| job_rows::get(c, id))
            .await
            .ok()
            .flatten()
            .is_some_and(|r| r.state.is_finished())
    })
    .await;
    idle(app).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unknown_layout_waits_for_a_dat_without_decoding() {
    let dir = tempfile::tempdir().expect("tempdir");
    let b = boot(dir, true).await;
    let app = &b.running.app;
    let written = image(&games(&b).join("PSX/G/g.chd"), &disc("g"));

    scan(&b, "psx").await;
    let no_layout = || {
        vec![(
            "PSX/G/g.chd".to_owned(),
            FileState::Unidentified,
            Some("no_layout".to_owned()),
        )]
    };
    assert_eq!(rows(app, "psx").await, no_layout());
    assert_eq!(
        count(app, "SELECT COUNT(*) FROM chd_tracks").await,
        0,
        "no decode"
    );
    let jobs = chd_jobs(app).await;
    scan(&b, "psx").await;
    assert_eq!(
        rows(app, "psx").await,
        no_layout(),
        "a timer rescan keeps it"
    );
    assert_eq!(chd_jobs(app).await, jobs, "and queues nothing");

    load_dat(&b, "psx.dat", &psx_dat("1", &written.tracks)).await;
    assert_eq!(rows(app, "psx").await, verified_members("PSX/G/g.chd", 3));
    assert_eq!(chd_jobs(app).await, jobs + 1);

    // A new DAT version that changes track 2 retires the old roms and rematches the members.
    let mut changed = written.tracks.clone();
    changed[1].sha1 = "0".repeat(40);
    changed[1].md5 = "0".repeat(32);
    load_dat(&b, "psx2.dat", &psx_dat("2", &changed)).await;
    let t1 = row(app, "psx", "PSX/G/g.chd#01").await.expect("track 1");
    let t2 = row(app, "psx", "PSX/G/g.chd#02").await.expect("track 2");
    assert_eq!(t1.state, FileState::Unverified);
    assert!(t1.rom_id.is_some(), "track 1 still matches a live rom");
    assert_eq!((t2.state, t2.rom_id), (FileState::Unverified, None));
    assert!(row(app, "psx", "PSX/G/g.chd#cue").await.is_none());
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cooked_track_is_recorded_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let b = boot(dir, true).await;
    let app = &b.running.app;
    let spec = Spec::new(
        "c",
        vec![track(Kind::Mode1Cooked, 40, 0), track(Kind::Audio, 20, 0)],
    );
    image(&games(&b).join("PSX/C/c.chd"), &spec);

    scan(&b, "psx").await;
    let cooked = vec![(
        "PSX/C/c.chd".to_owned(),
        FileState::Unidentified,
        Some("cooked".to_owned()),
    )];
    assert_eq!(rows(app, "psx").await, cooked);
    let failed_at = || count(app, "SELECT MAX(failed_at) FROM chd_failures");
    let first = failed_at().await;
    assert_eq!(count(app, "SELECT COUNT(*) FROM chd_failures").await, 1);
    let jobs = chd_jobs(app).await;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    scan(&b, "psx").await;
    assert_eq!(rows(app, "psx").await, cooked);
    assert_eq!(failed_at().await, first);
    assert_eq!(
        chd_jobs(app).await,
        jobs,
        "a stored failure is not decoded again"
    );
    b.running.shutdown().await.expect("shutdown");
}

/// A disc of `frames` frames at one frame per hunk, long enough to catch mid-decode.
fn long_disc(label: &str, frames: u32) -> Spec {
    let mut spec = Spec::new(label, vec![track(Kind::Mode1Raw, frames, 0)]);
    spec.frames_per_hunk = 1;
    spec.codecs = vec![Codec::CdZlib];
    spec
}

/// The running `chd_tracks` job once it reported decoded bytes of `file`.
async fn decoding(app: &AppState, file: &str) -> JobId {
    let start = Instant::now();
    loop {
        let open = app
            .db
            .read(|c| job_rows::open_in_lane(c, "heavy"))
            .await
            .expect("open");
        let found = open.into_iter().find(|r| {
            r.kind == "chd_tracks"
                && r.progress
                    .as_ref()
                    .is_some_and(|p| p["file"] == file && p["bytes_done"].as_u64() > Some(0))
        });
        if let Some(r) = found {
            return r.id;
        }
        assert!(start.elapsed() < WAIT, "no decode of {file} in progress");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_state(app: &AppState, id: JobId, want: JobState) {
    wait_for("the job state", || async move {
        app.db
            .read(move |c| job_rows::get(c, id))
            .await
            .ok()
            .flatten()
            .is_some_and(|r| r.state == want)
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_held_gate_pauses_decoding_and_turning_off_stops_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let b = boot(dir, false).await;
    let app = &b.running.app;
    let spec = long_disc("l", 12_000);
    let written = image(&games(&b).join("PSX/L/l.chd"), &spec);
    let cue = cue_hash("l");
    seed_title_with(app, "psx", "l", &cue, &written.tracks).await;
    let other = image(&games(&b).join("PSX/M/m.chd"), &long_disc("m", 12_000));
    seed_title_with(app, "psx", "m", &cue_hash("m"), &other.tracks).await;
    scan(&b, "psx").await;

    // Paused mid-decode, the job resumes and records the same hashes.
    put_setting(&b, true).await;
    let id = decoding(app, "l.chd").await;
    app.gate.set_override(Some(Override::Paused));
    wait_state(app, id, JobState::Paused).await;
    assert!(
        row(app, "psx", "PSX/L/l.chd#01").await.is_none(),
        "paused mid-image"
    );
    app.gate.set_override(None);
    wait_for("the first image", || async {
        row(app, "psx", "PSX/L/l.chd#01").await.is_some()
    })
    .await;
    let t1 = row(app, "psx", "PSX/L/l.chd#01").await.expect("track");
    assert_eq!(
        (t1.state, t1.sha1),
        (FileState::Verified, Some(written.tracks[0].sha1.clone()))
    );

    // Turned off while the second image decodes: the job stops and nothing is left waiting.
    let id = decoding(app, "m.chd").await;
    app.gate.set_override(Some(Override::Paused));
    wait_state(app, id, JobState::Paused).await;
    put_setting(&b, false).await;
    app.gate.set_override(None);
    wait_state(app, id, JobState::Done).await;
    idle(app).await;
    let m = row(app, "psx", "PSX/M/m.chd").await.expect("container");
    assert_eq!(
        (m.state, m.reason.as_deref()),
        (FileState::Unidentified, Some("off"))
    );
    let waiting = count(
        app,
        "SELECT COUNT(*) FROM files WHERE reason IN ('pending', 'no_layout')",
    )
    .await;
    assert_eq!(waiting, 0);
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_job_hands_the_lane_to_a_scan_between_images() {
    let dir = tempfile::tempdir().expect("tempdir");
    let b = boot(dir, false).await;
    let app = &b.running.app;
    for (platform, top, label) in [
        ("psx", "PSX", "a"),
        ("psx", "PSX", "b"),
        ("saturn", "Saturn", "c"),
        ("saturn", "Saturn", "d"),
    ] {
        let w = image(
            &games(&b).join(format!("{top}/{label}/{label}.chd")),
            &disc(label),
        );
        seed_title(app, platform, label, &w.tracks).await;
    }
    scan(&b, "psx").await;
    scan(&b, "saturn").await;

    app.gate.set_override(Some(Override::Paused));
    put_setting(&b, true).await;
    let body = r#"{"platform_id":"psx"}"#;
    let r = request(b.addr(), "POST", "/api/v1/system/scan", &[], Some(body)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let scan_id = JobId(r.json()["job_id"].as_i64().expect("job_id"));
    app.gate.set_override(None);
    idle(app).await;

    for platform in ["psx", "saturn"] {
        let left = rows(app, platform).await;
        assert!(
            left.iter().all(|(_, s, _)| *s == FileState::Verified),
            "{left:?}"
        );
        assert_eq!(left.len(), 8);
    }
    let finished: Vec<(i64, serde_json::Value)> = app
        .db
        .read(|c| {
            let mut stmt = c.prepare(
                "SELECT id, progress FROM jobs WHERE kind = 'chd_tracks' AND state = 'done' ORDER BY id",
            )?;
            let rows = stmt
                .query_map([], |r| {
                    let p: String = r.get(1)?;
                    Ok((r.get(0)?, serde_json::from_str(&p).unwrap_or_default()))
                })?
                .collect::<rusqlite::Result<_>>()?;
            Ok(rows)
        })
        .await
        .expect("jobs");
    assert_eq!(
        finished.len(),
        2,
        "one yield to the scan, no ping-pong: {finished:?}"
    );
    assert_eq!(
        finished[0].1["done"], 1,
        "the lane went to the scan after one image"
    );
    assert!(finished[0].0 < scan_id.0 && scan_id.0 < finished[1].0);
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dat_listing_whole_chd_files_still_hashes_them_whole() {
    let dir = tempfile::tempdir().expect("tempdir");
    let b = boot(dir, true).await;
    let app = &b.running.app;
    let (bytes, _) = to_vec(&disc("w")).expect("image");
    write(&games(&b).join("PSX/W/w.chd"), &bytes);
    let whole = hash_reader(Cursor::new(&bytes), HeaderRule::None, None).expect("hash");
    let pid = PlatformId("psx".into());
    app.db
        .write(move |c| files::seed_rom_fixture(c, &pid, "w", "w.chd", &whole, "good").map(|_| ()))
        .await
        .expect("seed");

    scan(&b, "psx").await;
    let w = row(app, "psx", "PSX/W/w.chd").await.expect("row");
    assert_eq!((w.state, w.reason), (FileState::Verified, None));
    assert_eq!(w.header_rule.as_deref(), Some("none"));
    assert_eq!(chd_jobs(app).await, 0);
    assert_eq!(count(app, "SELECT COUNT(*) FROM chd_failures").await, 0);
    b.running.shutdown().await.expect("shutdown");
}
