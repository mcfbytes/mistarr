use std::io::Write as _;

use mistarr_core::hash::Md5Stream;

use super::*;
use crate::app::testutil::state;
use crate::jobs::Scheduler;

/// Zips as `(file, members)`, each member `(name, bytes)`.
type Zips<'a> = &'a [(&'a str, &'a [(&'a str, &'a [u8])])];

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

fn md5_of(parts: &[&[u8]]) -> String {
    let mut m = Md5Stream::new();
    for p in parts {
        m.update(p);
    }
    m.finish()
}

fn mra_xml(name: &str, rom: &str) -> String {
    format!(
        "<misterromdescription><name>{name}</name><setname>exblast</setname>\
         <rbf>excore</rbf>{rom}</misterromdescription>"
    )
}

#[test]
fn mras_are_listed_shallowest_first_without_following_depth_limits() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    for rel in [
        "Example Blaster.mra",
        "_alternatives/_Example Blaster/Example Blaster (set 2).mra",
        "Abc.MRA",
        "cores/excore_20240101.rbf",
        "a/b/c/d/e/too deep.mra",
        "notes.txt",
    ] {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
        fs::write(p, b"<m/>").expect("write");
    }
    let found: Vec<String> = list_mras(root).into_iter().map(|l| l.rel).collect();
    assert_eq!(
        found,
        [
            "Abc.MRA",
            "Example Blaster.mra",
            "_alternatives/_Example Blaster/Example Blaster (set 2).mra"
        ]
    );
    assert!(list_mras(&root.join("absent")).is_empty());
}

#[test]
fn linked_files_are_listed_once_and_the_organizer_tree_never() {
    let dir = tempfile::tempdir().expect("tempdir");
    let outside = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let main = root.join("Example Blaster.mra");
    fs::write(&main, b"<m/>").expect("write");
    fs::hard_link(&main, root.join("Example Blaster copy.mra")).expect("hard link");
    fs::create_dir_all(root.join("_ORGANIZED/_By Year/_1990s")).expect("mkdir");
    fs::write(root.join("_ORGANIZED/_By Year/real.mra"), b"<m/>").expect("write");
    std::os::unix::fs::symlink(&main, root.join("_ORGANIZED/_By Year/_1990s/eb.mra"))
        .expect("symlink");
    fs::create_dir_all(root.join("_Links")).expect("mkdir");
    std::os::unix::fs::symlink(&main, root.join("_Links/eb.mra")).expect("symlink");
    let quest = outside.path().join("Example Quest.mra");
    fs::write(&quest, b"<m/>").expect("write");
    std::os::unix::fs::symlink(&quest, root.join("_Links/eq.mra")).expect("symlink");
    std::os::unix::fs::symlink(outside.path(), root.join("_Linked dir")).expect("symlink");
    let found = list_mras(root);
    let rels: Vec<&str> = found.iter().map(|l| l.rel.as_str()).collect();
    assert_eq!(rels, ["Example Blaster copy.mra", "_Links/eq.mra"]);
    let meta = fs::metadata(&main).expect("meta");
    assert_eq!(found[0].stamp, mra_stamp(&meta));
    assert!(found[0]
        .stamp
        .starts_with(&format!("p{}:", mra::PARSER_VERSION)));
}

#[test]
fn the_check_stamp_ignores_zip_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (a, b) = (dir.path().join("a.zip"), dir.path().join("b.zip"));
    fs::write(&a, b"A").expect("write");
    fs::write(&b, b"B").expect("write");
    let place = |file: &str| ZipPath {
        dir: "mame".into(),
        file: file.into(),
    };
    let one = [
        (place("a.zip"), Some(a.clone())),
        (place("b.zip"), Some(b.clone())),
    ];
    let other = [(place("b.zip"), Some(b)), (place("a.zip"), Some(a))];
    assert_eq!(joined_stamp("s", &one), joined_stamp("s", &other));
    assert_eq!(joined_stamp("s", &[(place("c.zip"), None)]), None);
    assert_eq!(joined_stamp("s", &[]), None);
}

#[test]
fn entries_take_the_file_stem_when_the_mra_has_no_name() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("Example Quest.mra");
    fs::write(
        &path,
        r#"<misterromdescription><rom zip="exq.zip"/></misterromdescription>"#,
    )
    .expect("write");
    let e = read_entry("Example Quest.mra", &path).expect("entry");
    assert_eq!(e.name, "Example Quest");
    assert!(!e.stamp.is_empty());
    fs::write(&path, "<broken").expect("write");
    assert!(read_entry("Example Quest.mra", &path).is_none());
}

#[test]
fn entry_names_are_trimmed_and_collapsed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("Example Quest.mra");
    let entry = |name: &str| {
        fs::write(
            &path,
            format!(
                "<misterromdescription><name>{name}</name><rom zip=\"exq.zip\"/></misterromdescription>"
            ),
        )
        .expect("write");
        read_entry("Example Quest.mra", &path).expect("entry").name
    };
    assert_eq!(
        entry("\n  Example\t\tQuest  (World)\n"),
        "Example Quest (World)"
    );
    assert_eq!(entry("  \n\t "), "Example Quest");
}

#[test]
fn zips_are_found_case_insensitively_with_their_md5() {
    let dir = tempfile::tempdir().expect("tempdir");
    let games = dir.path();
    write_zip(&games.join("mame/EXBLAST.ZIP"), &[("a.bin", b"A")]);
    let mra = mra::parse(
        mra_xml(
            "Example Blaster",
            r#"<rom index="0" zip="exblast.zip|exparent.zip" md5="0123456789abcdef0123456789abcdef"/>
               <rom index="1" zip="/hbmame/exhb.zip"/>"#,
        )
        .as_bytes(),
    )
    .expect("parse");
    let index = ZipIndex::build(games, vec!["mame".into(), "hbmame".into()]);
    let zips = zips_of(&mra, &index);
    let got: Vec<(String, bool, bool)> = zips
        .iter()
        .map(|z| (z.path.rel_path(), z.on_disk.is_some(), z.md5.is_some()))
        .collect();
    assert_eq!(
        got,
        [
            ("mame/exblast.zip".to_owned(), true, true),
            ("mame/exparent.zip".to_owned(), false, true),
            ("hbmame/exhb.zip".to_owned(), false, false),
        ]
    );
    assert_eq!(check_stamp("1:1", &mra, &zips), None);
    let stamp = check_stamp("1:1", &mra, &zips[..1]).expect("stamp");
    assert!(stamp.starts_with("1:1;mame/exblast.zip:"), "{stamp}");
}

#[test]
fn zip_directories_are_found_case_insensitively() {
    let dir = tempfile::tempdir().expect("tempdir");
    let games = dir.path();
    write_zip(&games.join("hbmame/Sub/exhb.zip"), &[("a.bin", b"A")]);
    write_zip(&games.join("mame/exblast.zip"), &[("a.bin", b"A")]);
    let mra = mra::parse(
        mra_xml(
            "Example Blaster",
            r#"<rom index="0" zip="/HBMAME/sub/EXHB.zip|/HBMame/SUB/exhb.zip"/>
               <rom index="1" zip="../MAME/exblast.zip"/>"#,
        )
        .as_bytes(),
    )
    .expect("parse");
    let dirs = mra.zip_paths().into_iter().map(|z| z.dir).collect();
    let index = ZipIndex::build(games, dirs);
    let zips = zips_of(&mra, &index);
    assert!(!zips.is_empty());
    for z in &zips {
        assert!(z.on_disk.is_some(), "{}", z.path.rel_path());
    }
}

/// Builds `games/mame` with the zips and returns the index and a verify result.
fn verify_with(zips: Zips<'_>, rom: &str) -> Option<(&'static str, Option<String>)> {
    let dir = tempfile::tempdir().expect("tempdir");
    for (zip, members) in zips {
        write_zip(&dir.path().join("mame").join(zip), members);
    }
    let mra = mra::parse(mra_xml("Example Blaster", rom).as_bytes()).expect("parse");
    let index = ZipIndex::build(dir.path(), vec!["mame".into()]);
    verify(&mra, &index)
}

#[test]
fn verify_assembles_parts_and_compares_the_md5() {
    let md5 = md5_of(&[b"CPU0", b"23", b"\xff\xff"]);
    let rom = format!(
        r#"<rom index="0" zip="exblast.zip|exparent.zip" md5="{md5}">
             <part name="cpu.bin" length="4"/>
             <interleave output="16">
               <part name="gfx.bin" crc="deadbeef" map="01"/>
             </interleave>
             <part repeat="2">FF</part>
           </rom>"#
    );
    let zips: Zips<'_> = &[
        ("exblast.zip", &[("CPU.BIN", b"CPU0extra")]),
        ("exparent.zip", &[("gfx.bin", b"23")]),
    ];
    assert_eq!(verify_with(zips, &rom), Some(("match", None)));

    let bad = rom.replace(&md5, &md5_of(&[b"other"]));
    let (state, detail) = verify_with(zips, &bad).expect("checked");
    assert_eq!(state, "mismatch");
    assert!(detail.is_some_and(|d| d.contains("rom 0")));

    let short: Zips<'_> = &[("exblast.zip", &[("cpu.bin", b"CPU0")])];
    let (state, detail) = verify_with(short, &rom).expect("checked");
    assert_eq!(state, "missing_part");
    assert!(detail.is_some_and(|d| d.contains("gfx.bin")));

    let refused = format!(r#"<rom index="0" zip="exblast.zip" md5="{md5}"><group/></rom>"#);
    assert_eq!(verify_with(zips, &refused).map(|v| v.0), Some("refused"));

    let unchecked = r#"<rom index="0" zip="exblast.zip"><part name="cpu.bin"/></rom>"#;
    assert_eq!(verify_with(zips, unchecked), None);
}

#[test]
fn one_matching_alternative_of_an_index_is_enough() {
    let good = md5_of(&[b"CPU0extra"]);
    let rom = format!(
        r#"<rom index="0" zip="exblast.zip" md5="{good}"><part name="cpu.bin"/></rom>
           <rom index="0" zip="exblast.zip" md5="{good}"><part name="gone.bin"/></rom>
           <rom index="1" zip="exblast.zip" md5="{good}"><part name="cpu.bin"/></rom>"#
    );
    let zips: Zips<'_> = &[("exblast.zip", &[("cpu.bin", b"CPU0extra")])];
    assert_eq!(verify_with(zips, &rom), Some(("match", None)));
}

#[tokio::test]
async fn the_job_stores_retires_and_checks() {
    let (dir, app) = state();
    let arcade = dir.path().join(ARCADE_DIR);
    let games = dir.path().join("games");
    fs::create_dir_all(arcade.join("_alternatives/_Example Blaster")).expect("mkdir");
    let md5 = md5_of(&[b"CPU0"]);
    fs::write(
        arcade.join("Example Blaster.mra"),
        mra_xml(
            "Example Blaster",
            &format!(
                r#"<rom index="0" zip="exblast.zip" md5="{md5}"><part name="cpu.bin"/></rom>"#
            ),
        ),
    )
    .expect("write");
    fs::write(
        arcade.join("_alternatives/_Example Blaster/Example Blaster (set 2).mra"),
        mra_xml(
            "Example Blaster (set 2)",
            r#"<rom index="0" zip="exblast2.zip|exblast.zip"><part name="cpu.bin"/></rom>"#,
        ),
    )
    .expect("write");
    write_zip(&games.join("mame/exblast.zip"), &[("cpu.bin", b"CPU0")]);

    let progress = run(&app).await;
    assert_eq!(counts(&progress), (2, 1));
    assert_eq!(progress["mras"], 2);
    let main = stored(&app, "Example Blaster.mra").await.expect("main");
    let id = main.id;
    let info = app
        .db
        .read(move |c| rows::info(c, id))
        .await
        .expect("info")
        .expect("mra");
    assert_eq!(info.md5_check.as_deref(), Some("match"));
    assert!(info.missing_zips.is_empty());
    assert_eq!(info.setname.as_deref(), Some("exblast"));

    let progress = run(&app).await;
    assert_eq!(counts(&progress), (0, 0));
    assert_eq!(
        stored(&app, "Example Blaster.mra").await,
        Some(main.clone())
    );

    fs::remove_file(games.join("mame/exblast.zip")).expect("rm");
    let progress = run(&app).await;
    assert_eq!(counts(&progress), (0, 0));
    let info = app
        .db
        .read(move |c| rows::info(c, id))
        .await
        .expect("info")
        .expect("mra");
    assert_eq!(info.missing_zips, ["mame/exblast.zip"]);
    assert_eq!(info.md5_check, None);

    write_zip(&games.join("mame/exblast.zip"), &[("cpu.bin", b"CPU1")]);
    let progress = run(&app).await;
    assert_eq!(counts(&progress), (1, 1));
    let info = app
        .db
        .read(move |c| rows::info(c, id))
        .await
        .expect("info")
        .expect("mra");
    assert_eq!(info.md5_check.as_deref(), Some("mismatch"));

    fs::remove_file(arcade.join("Example Blaster.mra")).expect("rm");
    assert!(enqueue_if_relevant(&app).await.expect("enqueue").is_some());
    let progress = run(&app).await;
    assert_eq!(
        (&progress["mras"], &progress["retired"]),
        (&1.into(), &1.into())
    );
    assert_eq!(stored(&app, "Example Blaster.mra").await, None);
    let alt = "_alternatives/_Example Blaster/Example Blaster (set 2).mra";
    assert!(stored(&app, alt).await.is_some());
}

/// `(parsed, checked)` of a final progress.
fn counts(progress: &serde_json::Value) -> (u64, u64) {
    let n = |k: &str| progress[k].as_u64().expect("count");
    (n("parsed"), n("checked"))
}

/// Runs the catalogue and returns its final progress.
async fn run(app: &Arc<AppState>) -> serde_json::Value {
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

async fn stored(app: &Arc<AppState>, rel: &str) -> Option<rows::StoredMra> {
    let rel = rel.to_owned();
    app.db
        .read(move |c| rows::stored_mra(c, PLATFORM, &rel))
        .await
        .expect("stored")
}

#[tokio::test]
async fn the_shallowest_mra_keeps_a_shared_name() {
    let (dir, app) = state();
    let arcade = dir.path().join(ARCADE_DIR);
    fs::create_dir_all(arcade.join("_alternatives")).expect("mkdir");
    let body = mra_xml("Example Blaster", r#"<rom index="0" zip="exblast.zip"/>"#);
    fs::write(arcade.join("_alternatives/Example Blaster.mra"), &body).expect("write");
    run(&app).await;
    assert!(stored(&app, "_alternatives/Example Blaster.mra")
        .await
        .is_some());
    fs::write(arcade.join("Example Blaster.mra"), &body).expect("write");
    let progress = run(&app).await;
    assert_eq!(progress["parsed"], 1);
    assert_eq!(progress["mras"], 1);
    assert!(stored(&app, "Example Blaster.mra").await.is_some());
    assert!(stored(&app, "_alternatives/Example Blaster.mra")
        .await
        .is_none());
}

#[tokio::test]
async fn an_unreadable_mra_keeps_its_title_and_check() {
    use std::os::unix::fs::PermissionsExt as _;
    let (dir, app) = state();
    let arcade = dir.path().join(ARCADE_DIR);
    let games = dir.path().join("games");
    fs::create_dir_all(&arcade).expect("mkdir");
    let md5 = md5_of(&[b"CPU0"]);
    let path = arcade.join("Example Blaster.mra");
    let rom =
        format!(r#"<rom index="0" zip="exblast.zip" md5="{md5}"><part name="cpu.bin"/></rom>"#);
    fs::write(&path, mra_xml("Example Blaster", &rom)).expect("write");
    write_zip(&games.join("mame/exblast.zip"), &[("cpu.bin", b"CPU0")]);
    run(&app).await;
    let id = stored(&app, "Example Blaster.mra")
        .await
        .expect("stored")
        .id;
    let check = |app: Arc<AppState>| async move {
        app.db
            .read(move |c| rows::info(c, id))
            .await
            .expect("info")
            .expect("mra")
            .md5_check
    };
    assert_eq!(check(app.clone()).await.as_deref(), Some("match"));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).expect("chmod");
    if File::open(&path).is_ok() {
        return;
    }
    write_zip(
        &games.join("mame/exblast.zip"),
        &[("cpu.bin", b"CPU0"), ("x", b"")],
    );
    run(&app).await;
    assert_eq!(check(app.clone()).await.as_deref(), Some("match"));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("chmod");
    fs::write(&path, b"").expect("truncate");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).expect("chmod");
    run(&app).await;
    assert!(stored(&app, "Example Blaster.mra").await.is_some());
    assert_eq!(check(app.clone()).await.as_deref(), Some("match"));
}

#[tokio::test]
async fn nothing_is_queued_without_mra_files_or_titles() {
    let (_dir, app) = state();
    assert_eq!(enqueue_if_relevant(&app).await.expect("enqueue"), None);
}
