use std::io::Write as _;
use std::time::{Duration, SystemTime};

use rusqlite::params;

use super::*;
use crate::app::testutil::state;
use crate::app::AppState;
use crate::jobs::arcade::{ArcadeCatalog, ARCADE_DIR};
use crate::jobs::Scheduler;

fn hashes() -> mistarr_core::HashSet {
    mistarr_core::HashSet {
        size: 4,
        crc32: "0000abcd".into(),
        md5: "0123456789abcdef0123456789abcdef".into(),
        sha1: "0123456789abcdef0123456789abcdef01234567".into(),
    }
}

fn pid() -> PlatformId {
    PlatformId("arcade".into())
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

/// Moves `path`'s mtime `secs` seconds into the past.
fn age(path: &Path, secs: u64) {
    let when = SystemTime::now() - Duration::from_secs(secs);
    File::options()
        .write(true)
        .open(path)
        .expect("open")
        .set_modified(when)
        .expect("set mtime");
}

fn zip(rel: &str, size: i64, mtime: i64) -> Zip {
    Zip {
        rel: rel.to_owned(),
        size,
        mtime,
    }
}

/// A row as `find_by_path` would return it.
fn row(id: i64, rel: &str, size: i64, mtime: i64, rom: Option<i64>, state: FileState) -> FileRow {
    FileRow {
        id: FileId(id),
        platform_id: pid(),
        rel_path: rel.to_owned(),
        size,
        mtime,
        crc32: Some("0000abcd".into()),
        md5: Some("m".into()),
        sha1: Some("s".into()),
        header_rule: Some("none".into()),
        rom_id: rom,
        state,
        scanned_at: 1,
    }
}

#[test]
fn zip_names_lists_only_zips_sorted_bytewise() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_zip(&dir.path().join("b.zip"), &[("a.bin", b"A")]);
    write_zip(&dir.path().join("B.ZIP"), &[("a.bin", b"A")]);
    fs::write(dir.path().join("notes.txt"), b"n").expect("write");
    assert_eq!(zip_names(dir.path()), ["B.ZIP", "b.zip"]);
    assert!(zip_names(&dir.path().join("absent")).is_empty());
}

#[test]
fn stat_leaves_out_a_zip_it_cannot_stat() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_zip(&dir.path().join("mame/a.zip"), &[("a.bin", b"A")]);
    let found = stat(dir.path(), "mame", "a.zip").expect("stated");
    assert_eq!(found.rel, "mame/a.zip");
    assert!(found.size > 0);
    assert!(stat(dir.path(), "mame", "gone.zip").is_none());
}

#[test]
fn an_mra_named_zip_with_no_rows_gets_one_presence_row() {
    let mut out = Changes::default();
    let z = zip("mame/a.zip", 10, 5);
    assert!(decide(&z, Some(7), None, Vec::new(), &mut out).is_none());
    assert_eq!(out.record, [(z, 7)]);
    assert!(out.drop.is_empty());

    let mut out = Changes::default();
    let z = zip("mame/stray.zip", 10, 5);
    assert!(decide(&z, None, None, Vec::new(), &mut out).is_none());
    assert_eq!(out, Changes::default(), "a zip no MRA names gets nothing");
}

#[test]
fn a_presence_row_follows_its_zip_and_the_live_mra_set() {
    let z = zip("mame/a.zip", 10, 5);
    let promoted = row(1, "mame/a.zip", 10, 5, Some(7), FileState::Verified);

    let mut out = Changes::default();
    decide(&z, Some(7), Some(promoted.clone()), Vec::new(), &mut out);
    assert_eq!(
        out,
        Changes::default(),
        "unchanged zip and rom: kept, verified"
    );

    let mut out = Changes::default();
    let touched = zip("mame/a.zip", 10, 6);
    decide(
        &touched,
        Some(7),
        Some(promoted.clone()),
        Vec::new(),
        &mut out,
    );
    assert_eq!(
        out.record,
        [(touched, 7)],
        "a changed zip is recorded again"
    );

    let mut out = Changes::default();
    decide(&z, Some(8), Some(promoted.clone()), Vec::new(), &mut out);
    assert_eq!(out.record, [(z.clone(), 8)], "another MRA rom names it now");

    let mut out = Changes::default();
    decide(&z, None, Some(promoted), Vec::new(), &mut out);
    assert_eq!(out.drop, ["mame/a.zip"], "no live MRA names it any more");
}

#[test]
fn member_rows_stand_for_the_zip_and_are_left_alone_while_it_is_unchanged() {
    let z = zip("mame/a.zip", 10, 5);
    let bare = row(1, "mame/a.zip", 10, 5, Some(7), FileState::Unverified);
    let member = row(2, "mame/a.zip#a.bin", 1, 5, Some(9), FileState::Verified);
    let mut out = Changes::default();
    let rc = decide(&z, Some(7), Some(bare), vec![member.clone()], &mut out);
    assert!(rc.is_none(), "an unchanged zip is never read");
    assert_eq!(out.drop, ["mame/a.zip"], "the presence row gives way");
    assert!(out.record.is_empty() && out.restamp.is_empty() && out.reverify.is_empty());

    let mut out = Changes::default();
    let moved = zip("mame/a.zip", 10, 6);
    let rc = decide(&moved, Some(7), None, vec![member], &mut out).expect("recheck");
    assert_eq!(rc.members.len(), 1);
    assert_eq!(out, Changes::default());
}

#[test]
fn recheck_keeps_matching_members_and_marks_changed_ones() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_zip(
        &dir.path().join("mame/a.zip"),
        &[("same.bin", b"SAME"), ("changed.bin", b"NEW!")],
    );
    let listed =
        zip_members(File::open(dir.path().join("mame/a.zip")).expect("open")).expect("members");
    let crc_of = |n: &str| {
        listed
            .iter()
            .find(|m| m.name == n)
            .map(|m| m.crc32.clone())
            .expect("member")
    };
    let mut same = row(1, "mame/a.zip#same.bin", 4, 5, Some(9), FileState::Verified);
    same.crc32 = Some(crc_of("same.bin").to_ascii_uppercase());
    let changed = row(
        2,
        "mame/a.zip#changed.bin",
        4,
        5,
        Some(9),
        FileState::Verified,
    );
    let gone = row(3, "mame/a.zip#gone.bin", 4, 5, Some(9), FileState::Verified);
    let rc = Recheck {
        zip: zip("mame/a.zip", 99, 6),
        rom: Some(7),
        members: vec![same, changed, gone],
    };
    let mut out = Changes::default();
    recheck(dir.path(), rc, &mut out);
    assert_eq!(out.restamp, [(FileId(1), 6)]);
    assert_eq!(out.reverify, [(FileId(2), 4, 6, crc_of("changed.bin"))]);
    assert_eq!(out.drop, ["mame/a.zip#gone.bin"]);
    assert!(out.record.is_empty(), "member rows still stand for the zip");
}

#[test]
fn recheck_of_an_unreadable_zip_changes_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::create_dir_all(dir.path().join("mame")).expect("mkdir");
    fs::write(dir.path().join("mame/a.zip"), b"not a zip yet").expect("write");
    let member = row(2, "mame/a.zip#a.bin", 1, 5, Some(9), FileState::Verified);
    let rc = Recheck {
        zip: zip("mame/a.zip", 13, 6),
        rom: Some(7),
        members: vec![member],
    };
    let mut out = Changes::default();
    recheck(dir.path(), rc, &mut out);
    assert_eq!(out, Changes::default());
}

#[test]
fn recheck_records_a_presence_row_once_no_member_is_left() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_zip(&dir.path().join("mame/a.zip"), &[("other.bin", b"O")]);
    let member = row(2, "mame/a.zip#a.bin", 1, 5, Some(9), FileState::Verified);
    let z = zip("mame/a.zip", 50, 6);
    let rc = Recheck {
        zip: z.clone(),
        rom: Some(7),
        members: vec![member],
    };
    let mut out = Changes::default();
    recheck(dir.path(), rc, &mut out);
    assert_eq!(out.drop, ["mame/a.zip#a.bin"]);
    assert_eq!(out.record, [(z, 7)]);
}

#[test]
fn plan_and_write_a_batch_against_the_database() {
    let (_dir, app) = state();
    let games = app.config().paths.games;
    write_zip(&games.join("mame/A.zip"), &[("a.bin", b"A")]);
    write_zip(&games.join("mame/b.zip"), &[("a.bin", b"B")]);
    let p = pid();
    let b_mtime = mtime_of(&games.join("mame/b.zip"));
    let rom = app
        .db
        .write_blocking(|c| {
            let rom = files::seed_rom_fixture(c, &p, "exampleset", "a.bin", &hashes(), "good")?;
            let none = Hashed::default();
            let verified = FileState::Verified;
            files::upsert(
                c,
                &p,
                "mame/b.zip#a.bin",
                1,
                b_mtime,
                &none,
                Some(rom),
                verified,
                1,
            )?;
            Ok(rom)
        })
        .expect("seed");
    let live: HashMap<String, i64> = [("mame/a.zip".to_owned(), rom)].into_iter().collect();
    let names = [
        "A.zip".to_owned(),
        "b.zip".to_owned(),
        "gone.zip".to_owned(),
    ];
    let out = plan_batch(&app.db, &games, &p, &live, "mame", &names).expect("plan");
    assert_eq!(out.record.len(), 1, "names match case-insensitively");
    assert_eq!(out.record[0].0.rel, "mame/A.zip");
    assert!(out.drop.is_empty() && out.restamp.is_empty() && out.reverify.is_empty());

    let (recorded, dropped) = app
        .db
        .write_blocking(|c| write_changes(c, &p, &out, 2))
        .expect("write");
    assert_eq!((recorded, dropped), (1, 0));
    let written = app
        .db
        .read_blocking(|c| files::find_by_path(c, &p, "mame/A.zip"))
        .expect("find")
        .expect("row");
    assert_eq!(written.rom_id, Some(rom));
    assert_eq!(written.state, FileState::Unverified);
    assert!(written.crc32.is_none() && written.md5.is_none());
}

#[test]
fn gone_keeps_listed_and_present_zips() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_zip(&dir.path().join("mame/sub/nested.zip"), &[("a.bin", b"A")]);
    let names = vec!["a.zip".to_owned()];
    let page = vec![
        "mame/a.zip".to_owned(),
        "mame/a.zip#x.bin".to_owned(),
        "mame/sub/nested.zip#a.bin".to_owned(),
        "mame/b.zip#x.bin".to_owned(),
    ];
    assert_eq!(gone(dir.path(), "mame", &names, page), ["mame/b.zip#x.bin"]);
}

/// Writes `_Arcade/<name>.mra` naming zip `zip` in `games/mame`.
fn write_mra(root: &Path, name: &str, zip: &str) {
    let arcade = root.join(ARCADE_DIR);
    fs::create_dir_all(&arcade).expect("mkdir");
    fs::write(
        arcade.join(format!("{name}.mra")),
        format!(
            "<misterromdescription><name>{name}</name><setname>exblast</setname><rbf>excore</rbf>\
             <rom index=\"0\" zip=\"{zip}\"><part name=\"a.bin\"/></rom></misterromdescription>"
        ),
    )
    .expect("write mra");
}

async fn catalogue(app: &Arc<AppState>) -> serde_json::Value {
    let id = Scheduler::run_inline(app, Arc::new(ArcadeCatalog))
        .await
        .expect("run");
    let row = app
        .db
        .read(move |c| crate::db::jobs::get(c, id))
        .await
        .expect("job")
        .expect("row");
    assert_eq!(
        row.state,
        crate::db::jobs::JobState::Done,
        "{:?}",
        row.progress
    );
    row.progress.expect("progress")
}

fn arcade_rows(app: &Arc<AppState>) -> Vec<(String, FileState, Option<i64>, i64)> {
    app.db
        .read_blocking(|c| {
            let mut stmt = c.prepare(
                "SELECT rel_path, state, rom_id, mtime FROM files
                 WHERE platform_id = 'arcade' ORDER BY rel_path",
            )?;
            let rows = stmt.query_map([], |r| {
                let state: String = r.get(1)?;
                Ok((
                    r.get(0)?,
                    FileState::parse(&state).unwrap_or(FileState::Pending),
                    r.get(2)?,
                    r.get(3)?,
                ))
            })?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
        .expect("rows")
}

fn zip_rom(app: &Arc<AppState>, name: &str) -> i64 {
    let name = name.to_owned();
    app.db
        .read_blocking(move |c| {
            Ok(c.query_row(
                "SELECT id FROM roms WHERE name = ?1 AND retired = 0",
                [name],
                |r| r.get(0),
            )?)
        })
        .expect("rom")
}

fn mtime_of(path: &Path) -> i64 {
    file_meta(path).expect("meta").1
}

/// Seeds a row the import path would write for member `a.bin` of `rel`.
fn import_row(app: &Arc<AppState>, rel: &str, mtime: i64, rom: i64) -> i64 {
    let member = format!("{rel}#a.bin");
    app.db
        .write_blocking(move |c| {
            let h = Hashed {
                crc32: Some("0000abcd"),
                md5: Some("0123456789abcdef0123456789abcdef"),
                sha1: None,
                header_rule: Some("none"),
            };
            let id = files::upsert(c, &pid(), &member, 1, mtime, &h, Some(rom), FileState::Verified, 1)?;
            c.execute(
                "INSERT INTO import_log (at, file_id, action, detail) VALUES (1, ?1, 'placed', '{}')",
                [id.0],
            )?;
            Ok(id.0)
        })
        .expect("seed")
}

#[tokio::test]
async fn a_zip_the_user_places_gets_a_row_that_follows_its_mra() {
    let (dir, app) = state();
    let games = app.config().paths.games;
    write_zip(&games.join("mame/exblast.zip"), &[("a.bin", b"AAAA")]);
    write_zip(&games.join("mame/stray.zip"), &[("a.bin", b"S")]);
    write_mra(dir.path(), "Example Blaster", "exblast.zip");

    let progress = catalogue(&app).await;
    assert_eq!(progress["presence_zips"], 2);
    assert_eq!(progress["presence_recorded"], 1);
    let rom = zip_rom(&app, "exblast.zip");
    let mtime = mtime_of(&games.join("mame/exblast.zip"));
    assert_eq!(
        arcade_rows(&app),
        [(
            "mame/exblast.zip".into(),
            FileState::Unverified,
            Some(rom),
            mtime
        )],
        "one row per zip, none for a zip nothing names"
    );

    let progress = catalogue(&app).await;
    assert_eq!(
        progress["presence_recorded"], 0,
        "an unchanged zip is not rewritten"
    );

    fs::remove_file(dir.path().join(ARCADE_DIR).join("Example Blaster.mra")).expect("rm");
    catalogue(&app).await;
    assert!(arcade_rows(&app).is_empty(), "the row follows its MRA away");

    write_mra(dir.path(), "Example Blaster", "exblast.zip");
    catalogue(&app).await;
    assert_eq!(
        arcade_rows(&app).len(),
        1,
        "and back, with the zip untouched"
    );
}

#[tokio::test]
async fn an_unchanged_import_row_is_never_downgraded() {
    let (dir, app) = state();
    let games = app.config().paths.games;
    let path = games.join("mame/exblast.zip");
    write_zip(&path, &[("a.bin", b"AAAA")]);
    write_mra(dir.path(), "Example Blaster", "exblast.zip");
    catalogue(&app).await;
    let rom = zip_rom(&app, "exblast.zip");
    let mtime = mtime_of(&path);
    import_row(&app, "mame/exblast.zip", mtime, rom);

    catalogue(&app).await;
    catalogue(&app).await;
    assert_eq!(
        arcade_rows(&app),
        [(
            "mame/exblast.zip#a.bin".into(),
            FileState::Verified,
            Some(rom),
            mtime
        )],
        "the import row stays verified and the presence row gives way"
    );
    let md5: Option<String> = app
        .db
        .read_blocking(|c| Ok(c.query_row("SELECT md5 FROM files", [], |r| r.get(0))?))
        .expect("md5");
    assert!(md5.is_some(), "hashes are kept");
}

#[tokio::test]
async fn an_unreadable_zip_leaves_its_rows_alone() {
    let (dir, app) = state();
    let games = app.config().paths.games;
    let path = games.join("mame/exblast.zip");
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(&path, b"still being copied").expect("write");
    write_mra(dir.path(), "Example Blaster", "exblast.zip");
    catalogue(&app).await;
    let rom = zip_rom(&app, "exblast.zip");
    let old = mtime_of(&path) - 100;
    import_row(&app, "mame/exblast.zip", old, rom);

    catalogue(&app).await;
    assert_eq!(
        arcade_rows(&app),
        [(
            "mame/exblast.zip#a.bin".into(),
            FileState::Verified,
            Some(rom),
            old
        )],
        "a zip that cannot be read keeps its import rows as they were"
    );
}

#[tokio::test]
async fn a_rewritten_zip_keeps_matching_members_verified() {
    let (dir, app) = state();
    let games = app.config().paths.games;
    let path = games.join("mame/exblast.zip");
    write_zip(&path, &[("a.bin", b"AAAA")]);
    write_mra(dir.path(), "Example Blaster", "exblast.zip");
    catalogue(&app).await;
    let rom = zip_rom(&app, "exblast.zip");
    let crc = zip_members(File::open(&path).expect("open")).expect("members")[0]
        .crc32
        .clone();
    let old = mtime_of(&path);
    let id = import_row(&app, "mame/exblast.zip", old, rom);
    app.db
        .write_blocking(move |c| {
            c.execute(
                "UPDATE files SET size = 4, crc32 = ?2 WHERE id = ?1",
                params![id, crc],
            )?;
            Ok(())
        })
        .expect("crc");
    age(&path, 3600);

    catalogue(&app).await;
    let now = mtime_of(&path);
    assert_ne!(now, old);
    assert_eq!(
        arcade_rows(&app),
        [(
            "mame/exblast.zip#a.bin".into(),
            FileState::Verified,
            Some(rom),
            now
        )]
    );

    write_zip(&path, &[("a.bin", b"BBBB")]);
    age(&path, 60);
    catalogue(&app).await;
    let rows = arcade_rows(&app);
    assert_eq!(
        rows[0].1,
        FileState::Unverified,
        "changed content is checked again"
    );
    assert_eq!(rows[0].2, Some(rom));
}

#[tokio::test]
async fn deleting_a_zip_prunes_its_rows_and_have_drops() {
    let (_dir, app) = state();
    let games = app.config().paths.games;
    let path = games.join("mame/exampleset.zip");
    write_zip(&path, &[("a.bin", b"AAAA")]);
    let (title, rom) = app
        .db
        .write_blocking(|c| {
            let t = files::seed_title_fixture(c, &pid(), "exampleset")?;
            let r = files::seed_rom_for_title_fixture(c, t, "a.bin", &hashes(), "good")?;
            Ok((t, r))
        })
        .expect("seed");
    let log = import_row(&app, "mame/exampleset.zip", mtime_of(&path), rom);
    let have = |app: &Arc<AppState>| -> i64 {
        app.db
            .read_blocking(move |c| {
                Ok(c.query_row(
                    "SELECT have_verified FROM title_groups WHERE parent_id = ?1",
                    [title],
                    |r| r.get(0),
                )?)
            })
            .expect("have")
    };
    catalogue(&app).await;
    assert_eq!(have(&app), 1);

    fs::remove_file(&path).expect("rm");
    let progress = catalogue(&app).await;
    assert_eq!(progress["presence_pruned"], 1);
    assert_eq!(have(&app), 0);
    assert!(arcade_rows(&app).is_empty());
    let logged: Option<i64> = app
        .db
        .read_blocking(move |c| {
            Ok(c.query_row(
                "SELECT file_id FROM import_log WHERE file_id = ?1 OR file_id IS NULL",
                [log],
                |r| r.get(0),
            )?)
        })
        .expect("log");
    assert_eq!(logged, None, "the import_log entry outlives its row");
}
