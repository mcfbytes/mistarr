use std::time::Duration;

use axum::http::StatusCode;
use mistarr_core::{HashSet, PlatformId};
use mistarr_mister::launch::{FakeOutcome, RecordingSink};

use super::*;
use crate::app::testutil::state;
use crate::db::files::{self, FileState, Hashed};

fn hashes() -> HashSet {
    HashSet {
        size: 4,
        crc32: "00000001".into(),
        md5: "0".repeat(32),
        sha1: "1".repeat(40),
    }
}

fn touch(root: &FsPath, rel: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, b"").expect("write");
}

fn recording(app: &AppState) -> Arc<RecordingSink> {
    let sink = Arc::new(RecordingSink::new());
    app.set_command_sink(Arc::clone(&sink) as Arc<dyn CommandSink>);
    sink
}

/// A title on `platform` with one rom per name, each with a file in `state`.
async fn seed(app: &AppState, platform: &str, roms: &[(&str, FileState)]) -> TitleId {
    let pid = PlatformId(platform.to_owned());
    let roms: Vec<(String, FileState)> = roms.iter().map(|(n, s)| ((*n).to_owned(), *s)).collect();
    app.db
        .write(move |c| {
            let t = files::seed_title_fixture(c, &pid, "Example Quest (USA)")?;
            for (rel, st) in &roms {
                let name = rel.rsplit('/').next().unwrap_or(rel);
                let rom = files::seed_rom_for_title_fixture(c, t, name, &hashes(), "good")?;
                files::upsert(c, &pid, rel, 4, 0, &Hashed::default(), Some(rom), *st, 0)?;
            }
            Ok(TitleId(t))
        })
        .await
        .expect("seed")
}

fn status(r: Result<Launched, ApiError>) -> StatusCode {
    r.map_or_else(|e| e.status, |_| StatusCode::OK)
}

#[tokio::test]
async fn a_verified_game_starts_through_an_mgl() {
    let (dir, app) = state();
    let sink = recording(&app);
    touch(dir.path(), "_Console/NES_20230101.rbf");
    touch(dir.path(), "_Console/NES_20240101.rbf");
    let id = seed(
        &app,
        "nes",
        &[("NES/Example Quest (USA).nes", FileState::Verified)],
    )
    .await;
    let launched = launch_title(&app, id).await.expect("launch");
    assert_eq!(launched.core, "_Console/NES_20240101.rbf");
    assert_eq!(
        launched.file.as_deref(),
        Some("NES/Example Quest (USA).nes")
    );
    let mgl = only_mgl(dir.path());
    assert_eq!(sink.lines(), [format!("load_core {}\n", mgl.display())]);
    let doc = std::fs::read_to_string(&mgl).expect("mgl");
    assert!(doc.contains("<rbf>_Console/NES</rbf>"), "{doc}");
    assert!(doc.contains(r#"delay="2" type="f" index="1""#), "{doc}");
    let game = dir.path().join("games/NES/Example Quest (USA).nes");
    assert!(
        doc.contains(&format!("path=\"../../../../..{}\"", game.display())),
        "{doc}"
    );
}

/// The one launch MGL written into `dir`.
fn only_mgl(dir: &FsPath) -> PathBuf {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("list")
        .map(|e| e.expect("entry").path())
        .filter(|p| p.extension().is_some_and(|e| e == "mgl"))
        .collect();
    found.sort();
    found.pop().expect("an MGL was written")
}

#[tokio::test]
async fn a_disc_starts_from_its_cue() {
    let (dir, app) = state();
    recording(&app);
    touch(dir.path(), "_Console/PSX_20240101.rbf");
    let disc = dir.path().join("games/PSX/Example Disc");
    std::fs::create_dir_all(&disc).expect("mkdir");
    std::fs::write(
        disc.join("Example Disc.cue"),
        "FILE \"Example Disc (Track 1).bin\" BINARY\n",
    )
    .expect("cue");
    std::fs::write(disc.join("Example Disc (Track 1).bin"), b"").expect("bin");
    let id = seed(
        &app,
        "psx",
        &[
            (
                "PSX/Example Disc/Example Disc (Track 1).bin",
                FileState::Verified,
            ),
            ("PSX/Example Disc/Example Disc.cue", FileState::Verified),
        ],
    )
    .await;
    let launched = launch_title(&app, id).await.expect("launch");
    assert_eq!(
        launched.file.as_deref(),
        Some("PSX/Example Disc/Example Disc.cue")
    );
    let doc = std::fs::read_to_string(only_mgl(dir.path())).expect("mgl");
    assert!(doc.contains("type=\"s\""), "{doc}");
}

#[tokio::test]
async fn a_disc_with_a_misnamed_track_is_refused() {
    let (dir, app) = state();
    let sink = recording(&app);
    touch(dir.path(), "_Console/PSX_20240101.rbf");
    let id = seed(
        &app,
        "psx",
        &[
            ("PSX/D/d.bin", FileState::Misnamed),
            ("PSX/D/d.cue", FileState::Verified),
        ],
    )
    .await;
    let err = launch_title(&app, id).await.expect_err("misnamed track");
    assert_eq!(err.status, StatusCode::CONFLICT);
    assert!(sink.lines().is_empty());
}

#[tokio::test]
async fn a_second_launch_within_the_gap_is_busy() {
    let (dir, app) = crate::app::testutil::state_with(|o| o.launch_gap = Duration::from_secs(60));
    let sink = recording(&app);
    touch(dir.path(), "_Console/NES_20240101.rbf");
    let id = seed(&app, "nes", &[("NES/a.nes", FileState::Verified)]).await;
    launch_core(&app, "nes").await.expect("first");
    let err = launch_title(&app, id).await.expect_err("busy");
    assert_eq!((err.status, err.code), (StatusCode::CONFLICT, "busy"));
    assert_eq!(sink.lines().len(), 1);
}

#[tokio::test]
async fn failed_launches_do_not_hold_the_gap() {
    let (dir, app) = crate::app::testutil::state_with(|o| o.launch_gap = Duration::from_secs(60));
    recording(&app);
    assert_eq!(status(launch_core(&app, "nes").await), StatusCode::CONFLICT);
    touch(dir.path(), "_Console/NES_20240101.rbf");
    assert_eq!(status(launch_core(&app, "nes").await), StatusCode::OK);
}

#[tokio::test]
async fn an_unwritable_launch_dir_is_an_internal_error() {
    let (dir, app) =
        crate::app::testutil::state_with(|o| o.launch_dir = "/nonexistent/launch".into());
    let sink = recording(&app);
    touch(dir.path(), "_Console/NES_20240101.rbf");
    let id = seed(&app, "nes", &[("NES/a.nes", FileState::Verified)]).await;
    let err = launch_title(&app, id).await.expect_err("no dir");
    assert_eq!(
        (err.status, err.code),
        (StatusCode::INTERNAL_SERVER_ERROR, "internal")
    );
    assert_eq!(err.message, "the launch file could not be written");
    assert!(sink.lines().is_empty());
}

#[tokio::test]
async fn titles_that_cannot_start_are_conflicts() {
    let (dir, app) = state();
    let sink = recording(&app);
    let missing = seed(&app, "nes", &[("NES/a.nes", FileState::Unverified)]).await;
    assert_eq!(
        status(launch_title(&app, missing).await),
        StatusCode::CONFLICT
    );
    let placed = seed(&app, "nes", &[("NES/b.nes", FileState::Misnamed)]).await;
    assert_eq!(
        status(launch_title(&app, placed).await),
        StatusCode::CONFLICT,
        "no core"
    );
    touch(dir.path(), "_Console/NES_20240101.rbf");
    assert_eq!(status(launch_title(&app, placed).await), StatusCode::OK);
    app.db
        .write(move |c| crate::db::titles::set_flags(c, placed, &["bios".to_owned()]))
        .await
        .expect("flag");
    assert_eq!(
        status(launch_title(&app, placed).await),
        StatusCode::CONFLICT
    );
    let dat_arcade = seed(
        &app,
        "arcade",
        &[("mame/exb.zip#a.rom", FileState::Verified)],
    )
    .await;
    assert_eq!(
        status(launch_title(&app, dat_arcade).await),
        StatusCode::CONFLICT
    );
    assert_eq!(
        status(launch_title(&app, TitleId(9999)).await),
        StatusCode::NOT_FOUND
    );
    assert_eq!(sink.lines().len(), 1);
}

#[tokio::test]
async fn an_mra_title_loads_its_mra() {
    let (dir, app) = state();
    let sink = recording(&app);
    let id = app
        .db
        .write(|c| {
            let t = files::seed_title_fixture(c, &PlatformId("arcade".into()), "Example Blaster")?;
            c.execute(
                "UPDATE titles SET source = 'mra', mra_path = 'Example Blaster.mra' WHERE id = ?1",
                [t],
            )?;
            let rom = files::seed_rom_for_title_fixture(c, t, "exb.zip", &hashes(), "good")?;
            c.execute("UPDATE roms SET present = 1 WHERE id = ?1", [rom])?;
            Ok(TitleId(t))
        })
        .await
        .expect("seed");
    assert_eq!(
        status(launch_title(&app, id).await),
        StatusCode::CONFLICT,
        "no MRA on disk"
    );
    touch(dir.path(), "outside.mra");
    app.db
        .write(move |c| {
            c.execute(
                "UPDATE titles SET mra_path = '../outside.mra' WHERE id = ?1",
                [id.0],
            )?;
            Ok(())
        })
        .await
        .expect("path");
    assert_eq!(status(launch_title(&app, id).await), StatusCode::CONFLICT);
    app.db
        .write(move |c| {
            c.execute(
                "UPDATE titles SET mra_path = 'Example Blaster.mra' WHERE id = ?1",
                [id.0],
            )?;
            Ok(())
        })
        .await
        .expect("path");
    touch(dir.path(), "_Arcade/Example Blaster.mra");
    let launched = launch_title(&app, id).await.expect("launch");
    assert_eq!(launched.core, "_Arcade/Example Blaster.mra");
    let mra = dir.path().join("_Arcade/Example Blaster.mra");
    assert_eq!(sink.lines(), [format!("load_core {}\n", mra.display())]);
}

#[tokio::test]
async fn a_bare_core_starts_the_newest_rbf() {
    let (dir, app) = state();
    let sink = recording(&app);
    assert_eq!(
        status(launch_core(&app, "snes").await),
        StatusCode::CONFLICT
    );
    touch(dir.path(), "_Console/SNES_20240101.rbf");
    touch(dir.path(), "_Console/SNES_20250101.rbf");
    let launched = launch_core(&app, "snes").await.expect("launch");
    assert_eq!(launched.core, "_Console/SNES_20250101.rbf");
    assert_eq!(launched.file, None);
    let rbf = dir.path().join("_Console/SNES_20250101.rbf");
    assert_eq!(sink.lines(), [format!("load_core {}\n", rbf.display())]);
    assert_eq!(
        status(launch_core(&app, "arcade").await),
        StatusCode::CONFLICT
    );
    assert_eq!(
        status(launch_core(&app, "nope").await),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn an_absent_interface_is_unavailable() {
    let (dir, app) = state();
    touch(dir.path(), "_Console/SNES_20240101.rbf");
    let err = launch_core(&app, "snes").await.expect_err("no FIFO");
    assert_eq!(
        (err.status, err.code),
        (StatusCode::SERVICE_UNAVAILABLE, "unavailable")
    );
    let sink = recording(&app);
    sink.set_outcome(FakeOutcome::NotListening);
    assert_eq!(
        status(launch_core(&app, "snes").await),
        StatusCode::SERVICE_UNAVAILABLE
    );
    sink.set_outcome(FakeOutcome::Absent);
    let id = seed(&app, "nes", &[("NES/a.nes", FileState::Verified)]).await;
    assert_eq!(
        status(launch_title(&app, id).await),
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn the_setting_turns_launching_off() {
    let (dir, app) = state();
    let sink = recording(&app);
    touch(dir.path(), "_Console/SNES_20240101.rbf");
    app.update_config(|c| c.prefs.launch = false);
    let err = launch_core(&app, "snes").await.expect_err("disabled");
    assert_eq!((err.status, err.code), (StatusCode::CONFLICT, "conflict"));
    assert!(sink.lines().is_empty());
}

#[test]
fn command_errors_map_to_statuses() {
    use mistarr_mister::Error as E;
    assert_eq!(
        command_error(E::NotListening).status,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        command_error(E::CommandBusy).status,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        command_error(E::UnsafePath("x".into())).status,
        StatusCode::CONFLICT
    );
    assert_eq!(
        command_error(E::MissingHeader).status,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(
        relative(FsPath::new("/r/_Console/a.rbf"), FsPath::new("/r")),
        "_Console/a.rbf"
    );
    assert_eq!(
        relative(FsPath::new("/x/a.rbf"), FsPath::new("/r")),
        "/x/a.rbf"
    );
}
