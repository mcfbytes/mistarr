use std::fs;
use std::io::{Cursor, Write as _};
use std::sync::Arc;

use mistarr_core::hash::{hash_reader, HeaderRule};
use rusqlite::params;

use super::*;
use crate::app::testutil::state;
use crate::db::arcade::{self as rows, MraTitle, MraZip};
use crate::jobs::arcade::ArcadeCatalog;
use crate::jobs::Scheduler;

fn conn() -> Connection {
    let mut c = Connection::open_in_memory().expect("open");
    crate::db::migrate::apply(&mut c).expect("migrate");
    crate::db::platforms::seed(&mut c, &mistarr_mister::platforms::PLATFORMS).expect("seed");
    c
}

fn write_zip(path: &Path, members: &[(&str, &[u8])]) {
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    let mut z = zip::ZipWriter::new(File::create(path).expect("create"));
    for (name, body) in members {
        z.start_file(*name, zip::write::SimpleFileOptions::default())
            .expect("start");
        z.write_all(body).expect("write");
    }
    z.finish().expect("finish");
}

fn zip_file(dir: &str, name: &str, path: PathBuf) -> ZipFile {
    ZipFile {
        dir: dir.to_owned(),
        name: name.to_owned(),
        rel: format!("{dir}/{name}"),
        path,
    }
}

#[test]
fn list_zips_finds_mame_and_hbmame_but_only_zips() {
    let dir = tempfile::tempdir().expect("tempdir");
    let games = dir.path().join("games");
    write_zip(&games.join("mame/exampleset.zip"), &[("a.bin", b"A")]);
    write_zip(&games.join("hbmame/exhb.zip"), &[("h.bin", b"H")]);
    fs::write(games.join("mame/notes.txt"), b"n").expect("write");
    let platform = mistarr_mister::platforms::by_id("arcade").expect("arcade");
    let found: Vec<String> = list_zips(&games, platform)
        .into_iter()
        .map(|z| z.rel)
        .collect();
    assert_eq!(found, ["hbmame/exhb.zip", "mame/exampleset.zip"]);
    assert!(list_zips(&dir.path().join("absent"), platform).is_empty());
}

#[test]
fn scan_zip_matches_the_dat_by_crc32_and_size() {
    let c = conn();
    let pid = PlatformId("arcade".into());
    let title = files::seed_title_fixture(&c, &pid, "exampleset").expect("title");
    // A crc-only MAME DAT entry: no md5 or sha1, matched at the presence pass's level.
    let hashed = hash_reader(Cursor::new(b"CPU0"), HeaderRule::None, None).expect("hash");
    c.execute(
        "INSERT INTO roms (title_id, name, size, crc32, md5, sha1, status)
         VALUES (?1, 'cpu.bin', ?2, ?3, NULL, NULL, 'good')",
        params![title, i64::try_from(hashed.size).unwrap_or(0), hashed.crc32],
    )
    .expect("rom");
    let rom_id: i64 = c
        .query_row("SELECT id FROM roms WHERE title_id = ?1", [title], |r| {
            r.get(0)
        })
        .expect("rom id");

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("exampleset.zip");
    write_zip(&path, &[("cpu.bin", b"CPU0")]);
    let zf = zip_file("mame", "exampleset.zip", path);
    let (mut out, mut seen) = (Vec::new(), Vec::new());
    scan_zip(&c, &pid, &zf, &mut out, &mut seen).expect("scan");
    assert_eq!(seen, ["mame/exampleset.zip#cpu.bin"]);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].rom_id, Some(rom_id));
    assert_eq!(out[0].state, FileState::Verified);
}

#[test]
fn scan_zip_records_a_zip_a_live_mra_names_but_no_dat_matches() {
    let c = conn();
    let pid = PlatformId("arcade".into());
    let v = rows::mra_version(&c, "arcade", 1).expect("version");
    let t = MraTitle {
        name: "Example Blaster",
        base_name: "Example Blaster",
        group_key: "mra:example blaster",
        regions: &[],
        languages: &[],
        revision: None,
        flags: &[],
        setname: Some("exblast"),
        rbf: Some("excore"),
        mra_path: "Example Blaster.mra",
        file_stamp: "10:1",
        run: 1,
    };
    let zips = [MraZip {
        name: "exblast.zip",
        zip_dir: "mame",
        md5: None,
        present: false,
    }];
    rows::upsert_title(&c, "arcade", v, &t, &zips).expect("upsert");
    let rom_id: i64 = c
        .query_row("SELECT id FROM roms WHERE name = 'exblast.zip'", [], |r| {
            r.get(0)
        })
        .expect("rom");

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("exblast.zip");
    write_zip(&path, &[("cpu.bin", b"CPU0")]);
    let zf = zip_file("mame", "exblast.zip", path);
    let (mut out, mut seen) = (Vec::new(), Vec::new());
    scan_zip(&c, &pid, &zf, &mut out, &mut seen).expect("scan");
    assert_eq!(seen, ["mame/exblast.zip#cpu.bin"]);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].rom_id, Some(rom_id));
    assert_eq!(out[0].state, FileState::Unverified);
}

#[test]
fn scan_zip_skips_a_member_no_dat_or_mra_covers() {
    let c = conn();
    let pid = PlatformId("arcade".into());
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("exampleset.zip");
    write_zip(&path, &[("stray.bin", b"X")]);
    let zf = zip_file("mame", "exampleset.zip", path);
    let (mut out, mut seen) = (Vec::new(), Vec::new());
    scan_zip(&c, &pid, &zf, &mut out, &mut seen).expect("scan");
    assert!(
        out.is_empty(),
        "nothing knows this zip, so no row is written"
    );
    assert_eq!(
        seen,
        ["mame/exampleset.zip#stray.bin"],
        "still kept from pruning"
    );
}

#[test]
fn scan_zip_marks_an_unreadable_zip_with_a_bare_row() {
    let c = conn();
    let pid = PlatformId("arcade".into());
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("broken.zip");
    fs::write(&path, b"not a zip").expect("write");
    let zf = zip_file("mame", "broken.zip", path);
    let (mut out, mut seen) = (Vec::new(), Vec::new());
    scan_zip(&c, &pid, &zf, &mut out, &mut seen).expect("scan");
    assert_eq!(seen, ["mame/broken.zip"]);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].rel_path, "mame/broken.zip");
    assert_eq!(out[0].state, FileState::Unverified);
    assert!(out[0].rom_id.is_none());
}

/// Seeds a crc-only DAT title with one rom and writes a zip on disk matching it,
/// as if the user placed a set the presence pass alone must verify.
fn seed_and_place(app: &std::sync::Arc<crate::app::AppState>, member: &str, body: &[u8]) -> i64 {
    let pid = PlatformId("arcade".into());
    let hashed = hash_reader(Cursor::new(body), HeaderRule::None, None).expect("hash");
    let member_owned = member.to_owned();
    let title = app
        .db
        .write_blocking(move |c| {
            let t = files::seed_title_fixture(c, &pid, "exampleset")?;
            c.execute(
                "INSERT INTO roms (title_id, name, size, crc32, md5, sha1, status)
                 VALUES (?1, ?2, ?3, ?4, NULL, NULL, 'good')",
                params![
                    t,
                    member_owned,
                    i64::try_from(hashed.size).unwrap_or(0),
                    hashed.crc32
                ],
            )?;
            Ok(t)
        })
        .expect("seed");
    write_zip(
        &app.config().paths.games.join("mame/exampleset.zip"),
        &[(member, body)],
    );
    title
}

fn have_verified(app: &std::sync::Arc<crate::app::AppState>, title: i64) -> i64 {
    app.db
        .read_blocking(move |c| {
            Ok(c.query_row(
                "SELECT have_verified FROM title_groups WHERE platform_id = 'arcade' AND parent_id = ?1",
                [title],
                |r| r.get(0),
            )?)
        })
        .expect("have")
}

/// A crc-only DAT set the user places directly under `games/mame`, with no MRA and no
/// import: the presence pass alone must verify it and drive it to `have`.
#[tokio::test]
async fn a_dat_only_set_the_user_places_becomes_have() {
    let (_dir, app) = state();
    let title = seed_and_place(&app, "cpu.bin", b"CPU0");
    assert_eq!(
        have_verified(&app, title),
        0,
        "not verified before the catalogue runs"
    );

    Scheduler::run_inline(&app, Arc::new(ArcadeCatalog))
        .await
        .expect("run");

    assert_eq!(
        have_verified(&app, title),
        1,
        "the placed zip drives the set to have"
    );
}

/// Once a set the presence pass verified loses its zip, the next run prunes the
/// stale row and the set drops out of `have`.
#[tokio::test]
async fn deleting_the_zip_prunes_its_row_and_have_drops() {
    let (_dir, app) = state();
    let title = seed_and_place(&app, "cpu.bin", b"CPU0");
    Scheduler::run_inline(&app, Arc::new(ArcadeCatalog))
        .await
        .expect("first run");
    assert_eq!(have_verified(&app, title), 1);

    fs::remove_file(app.config().paths.games.join("mame/exampleset.zip")).expect("rm");
    Scheduler::run_inline(&app, Arc::new(ArcadeCatalog))
        .await
        .expect("second run");

    assert_eq!(
        have_verified(&app, title),
        0,
        "the missing zip's row was pruned"
    );
    let rows_left: i64 = app
        .db
        .read_blocking(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM files WHERE platform_id = 'arcade'",
                [],
                |r| r.get(0),
            )?)
        })
        .expect("count");
    assert_eq!(rows_left, 0);
}
