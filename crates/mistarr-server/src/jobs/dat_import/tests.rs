use std::fmt::Write as _;
use std::io::{Cursor, Write as _};

use super::*;
use crate::app::testutil::state;
use crate::db::files::FileState;
use crate::db::jobs::{self as rows, JobState};

/// A database in its own temporary directory, dropped with it.
struct TestDb {
    dir: tempfile::TempDir,
    db: Db,
}

impl TestDb {
    fn with<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        self.db.write_blocking(f)
    }
}

fn conn() -> TestDb {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Db::open(&dir.path().join("t.db")).expect("open");
    db.write_blocking(|c| crate::db::platforms::seed(c, &mistarr_mister::platforms::PLATFORMS))
        .expect("seed");
    TestDb { dir, db }
}

/// A Logiqx DAT with games `(name, cloneof)`, one rom each.
fn dat(name: &str, version: &str, games: &[(&str, Option<&str>)]) -> String {
    let mut xml = format!(
        "<?xml version=\"1.0\"?>\n<datafile><header><name>{name}</name><version>{version}</version></header>\n"
    );
    for (i, (game, clone)) in games.iter().enumerate() {
        let clone = clone
            .map(|c| format!(" cloneof=\"{c}\""))
            .unwrap_or_default();
        let _ = writeln!(
            xml,
            "<game name=\"{game}\"{clone}><rom name=\"{i}.bin\" size=\"4\" crc=\"{i:08x}\"/></game>"
        );
    }
    xml.push_str("</datafile>\n");
    xml
}

fn request(stop: bool, bind: Option<Bind>) -> Request {
    Request {
        source_file: "t.dat".into(),
        file_stem: "t".into(),
        bind,
        prefs: Prefs::default(),
        now: 1,
        stop: watch::channel(stop).1,
        gate: watch::channel(GateState::default()).1,
        meter: None,
        abort_on_hold: false,
        floor: None,
    }
}

fn import(c: &TestDb, xml: &str, req: &Request) -> Outcome {
    import_member(&c.db, Cursor::new(xml.as_bytes()), req, "").expect("import")
}

fn loaded(o: Outcome) -> Loaded {
    match o {
        Outcome::Loaded(l) => l,
        other => panic!("expected a load, got {other:?}"),
    }
}

fn count(c: &TestDb, sql: &str) -> i64 {
    c.with(|x| Ok(x.query_row(sql, [], |r| r.get(0))?))
        .expect("count")
}

#[test]
fn rom_header_attributes_are_stored() {
    let c = conn();
    let xml = "<datafile><header><name>Maker - Nintendo Entertainment System</name></header>\
        <game name=\"Example Quest (USA)\"><rom name=\"a.nes\" size=\"4\" crc=\"0a0b0c0d\" \
        header=\"4E 45 53 1A\"/></game></datafile>";
    loaded(import(&c, xml, &request(false, None)));
    let header: Option<String> = c
        .with(|x| Ok(x.query_row("SELECT header FROM roms", [], |r| r.get(0))?))
        .expect("header");
    assert_eq!(header.as_deref(), Some("4E 45 53 1A"));
}

#[test]
fn prefs_map_known_hide_flags() {
    let cfg = PrefsConfig {
        hide: vec!["bios".into(), "unl".into()],
        regions: vec!["Japan".into()],
        ..PrefsConfig::default()
    };
    let p = prefs(&cfg);
    assert_eq!(p.hide, [HiddenFlag::Bios]);
    assert_eq!(p.regions, ["Japan"]);
}

#[test]
fn bound_dats_load_titles_and_pick() {
    let c = conn();
    let xml = dat(
        "Maker - Game Boy",
        "1",
        &[
            ("Example Quest (Japan)", None),
            ("Example Quest (USA)", None),
        ],
    );
    let l = loaded(import(&c, &xml, &request(false, None)));
    assert_eq!(l.platform, Some(PlatformId("gb".into())));
    assert_eq!((l.games, l.has_titles, l.retired), (2, true, 0));
    let picks = count(
        &c,
        "SELECT COUNT(*) FROM titles WHERE is_1g1r_pick = 1 AND name LIKE '%(USA)'",
    );
    assert_eq!(picks, 1);
    assert_eq!(count(&c, "SELECT COUNT(*) FROM roms"), 2);
    assert_eq!(count(&c, "SELECT game_count FROM dat_versions"), 2);
}

#[test]
fn unbound_dats_store_only_the_version() {
    let c = conn();
    let xml = dat("Test Console", "1", &[("Example Quest (USA)", None)]);
    let l = loaded(import(&c, &xml, &request(false, None)));
    assert_eq!((l.platform, l.has_titles, l.games), (None, false, 1));
    assert_eq!(count(&c, "SELECT COUNT(*) FROM titles"), 0);

    let bind = Bind {
        version: l.version,
        platform: PlatformId("nes".into()),
        dat_name: "Test Console".into(),
        dat_version: "1".into(),
    };
    let other = dat("Other Console", "1", &[("Example Quest (USA)", None)]);
    assert_eq!(
        import(&c, &other, &request(false, Some(bind.clone()))),
        Outcome::Skipped
    );
    let bound = loaded(import(&c, &xml, &request(false, Some(bind))));
    assert_eq!(bound.version, l.version);
    assert_eq!(bound.platform, Some(PlatformId("nes".into())));
    assert_eq!(
        count(&c, "SELECT COUNT(*) FROM titles WHERE platform_id = 'nes'"),
        1
    );
}

#[test]
fn clone_of_groups_and_nameless_headers_fall_back_to_the_file() {
    let c = conn();
    let xml = dat(
        "Maker - Game Boy",
        "1",
        &[
            ("Example Quest (Japan)", Some("Example Quest (USA)")),
            ("Example Quest (USA)", None),
        ],
    );
    loaded(import(&c, &xml, &request(false, None)));
    assert_eq!(count(&c, "SELECT COUNT(DISTINCT parent_id) FROM titles"), 1);
    assert_eq!(count(&c, "SELECT SUM(inferred) FROM titles"), 0);

    let nameless = "<datafile><game name=\"A\"><rom name=\"a\" size=\"1\"/></game></datafile>";
    let l = loaded(import(&c, nameless, &request(false, None)));
    let row = c
        .with(|x| dats::get(x, l.version))
        .expect("get")
        .expect("row");
    assert_eq!(row.dat_name, "t");
}

#[test]
fn malformed_dats_are_rejected_and_roll_back() {
    let c = conn();
    let good = dat("Maker - Game Boy", "1", &[("Example Quest (USA)", None)]);
    let broken = good.replace(
        "</datafile>",
        "<game name=\"Cut\"><rom name=\"x\" size=\"z\"/>",
    );
    let o = import(&c, &broken, &request(false, None));
    assert!(
        matches!(&o, Outcome::Rejected(r) if r.contains("size")),
        "{o:?}"
    );
    assert_eq!(count(&c, "SELECT COUNT(*) FROM dat_versions"), 0);
    let o = import_member(
        &c.db,
        Cursor::new(b"<html/>".as_slice()),
        &request(false, None),
        "m.dat",
    )
    .expect("import");
    assert!(
        matches!(&o, Outcome::Rejected(r) if r.starts_with("m.dat: ")),
        "{o:?}"
    );
}

#[test]
fn a_dat_over_several_stage_chunks_applies_at_once() {
    let c = conn();
    let names: Vec<String> = (0..=STAGE_CHUNK * 2)
        .map(|i| format!("Game {i} (USA)"))
        .collect();
    let games: Vec<(&str, Option<&str>)> = names.iter().map(|n| (n.as_str(), None)).collect();
    let xml = dat("Maker - Game Boy", "1", &games);
    let broken = xml.replace(
        "</datafile>",
        "<game name=\"Cut\"><rom name=\"x\" size=\"z\"/>",
    );
    let o = import(&c, &broken, &request(false, None));
    assert!(matches!(o, Outcome::Rejected(_)), "{o:?}");
    assert_eq!(
        count(&c, "SELECT COUNT(*) FROM titles"),
        0,
        "nothing half-loaded"
    );
    assert_eq!(count(&c, "SELECT COUNT(*) FROM dat_stage"), 0);
    let l = loaded(import(&c, &xml, &request(false, None)));
    let all = i64::try_from(names.len()).expect("fits");
    assert_eq!(l.games, u64::try_from(all).expect("fits"));
    assert_eq!(
        count(&c, "SELECT COUNT(*) FROM titles WHERE retired = 0"),
        all
    );
    assert_eq!(count(&c, "SELECT COUNT(*) FROM dat_stage"), 0);
}

#[test]
fn a_long_import_stops_on_shutdown() {
    let c = conn();
    let names: Vec<String> = (0..=CANCEL_EVERY)
        .map(|i| format!("Game {i} (USA)"))
        .collect();
    let games: Vec<(&str, Option<&str>)> = names.iter().map(|n| (n.as_str(), None)).collect();
    let xml = dat("Maker - Game Boy", "1", &games);
    let r = import_member(&c.db, Cursor::new(xml.as_bytes()), &request(true, None), "");
    assert!(matches!(r, Err(Error::Cancelled)));
    assert_eq!(count(&c, "SELECT COUNT(*) FROM titles"), 0);
}

fn zip_of(members: &[(&str, &str)]) -> Vec<u8> {
    let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, body) in members {
        z.start_file(*name, zip::write::SimpleFileOptions::default())
            .expect("start");
        z.write_all(body.as_bytes()).expect("write");
    }
    z.finish().expect("finish").into_inner()
}

#[test]
fn members_are_listed_by_extension() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pack = dir.path().join("p.zip");
    std::fs::write(
        &pack,
        zip_of(&[("a.dat", ""), ("b.XML", ""), ("c.txt", "")]),
    )
    .expect("write");
    assert_eq!(list_members(&pack).expect("members").len(), 2);
    std::fs::write(&pack, zip_of(&[("c.txt", "")])).expect("write");
    assert!(list_members(&pack).is_err());
    std::fs::write(&pack, b"not a zip").expect("write");
    assert!(list_members(&pack).expect_err("bad").contains("zip"));
    assert!(list_members(&dir.path().join("x.txt")).is_err());
    assert!(matches!(
        list_members(&dir.path().join("x.Dat")).as_deref(),
        Ok([Member::Plain])
    ));
}

#[test]
fn the_watcher_waits_for_age_and_a_steady_size() {
    let dir = tempfile::tempdir().expect("tempdir");
    let a = dir.path().join("a.dat");
    std::fs::write(&a, b"one").expect("write");
    std::fs::write(dir.path().join(".upload.part"), b"x").expect("write");
    std::fs::create_dir(dir.path().join("loaded")).expect("mkdir");
    let mut aged = DatWatcher::new(Duration::from_secs(3600));
    assert!(aged.poll(dir.path()).is_empty());
    assert!(aged.poll(dir.path()).is_empty(), "too young");

    let mut w = DatWatcher::new(Duration::ZERO);
    assert!(w.poll(dir.path()).is_empty());
    std::fs::write(&a, b"grown").expect("write");
    assert!(w.poll(dir.path()).is_empty(), "size changed");
    assert_eq!(w.poll(dir.path()), std::slice::from_ref(&a));
    assert!(w.poll(dir.path()).is_empty(), "reported once");
    std::fs::write(&a, b"replaced").expect("write");
    w.poll(dir.path());
    assert_eq!(
        w.poll(dir.path()),
        std::slice::from_ref(&a),
        "a new file under the same name"
    );
    std::fs::remove_file(&a).expect("remove");
    assert!(w.poll(dir.path()).is_empty());
    std::fs::write(&a, b"back").expect("write");
    w.poll(dir.path());
    assert_eq!(w.poll(dir.path()), [a], "a returning file is new again");
    assert!(DatWatcher::new(Duration::ZERO)
        .poll(&dir.path().join("none"))
        .is_empty());
}

#[tokio::test]
async fn the_job_moves_files_and_publishes_events() {
    let (_dir, app) = state();
    let dats_dir = app.config().paths.dats();
    std::fs::create_dir_all(&dats_dir).expect("mkdir");
    let mut events = app.events.subscribe(None).live;
    let pack = dats_dir.join("pack.zip");
    let gb = dat("Maker - Game Boy", "1", &[("Example Quest (USA)", None)]);
    std::fs::write(&pack, zip_of(&[("gb.dat", &gb), ("bad.dat", "<x/>")])).expect("write");
    let id = Scheduler::run_inline(&app, Arc::new(DatImport::new(&pack)))
        .await
        .expect("run");
    let row = app
        .db
        .read(move |c| rows::get(c, id))
        .await
        .expect("get")
        .expect("row");
    assert_eq!(row.state, JobState::Done, "{:?}", row.progress);
    assert!(dats_dir.join("loaded/pack.zip").is_file());
    let mut kinds = Vec::new();
    while let Ok(e) = events.try_recv() {
        kinds.push((e.kind, e.data.clone()));
    }
    assert!(kinds
        .iter()
        .any(|(k, d)| *k == EventKind::DatRejected && d.contains("bad.dat")));
    assert!(kinds
        .iter()
        .any(|(k, d)| *k == EventKind::DatLoaded && d.contains("\"platform_id\":\"gb\"")));

    let junk = dats_dir.join("notes.txt");
    std::fs::write(&junk, b"hello").expect("write");
    Scheduler::run_inline(&app, Arc::new(DatImport::new(&junk)))
        .await
        .expect("run");
    let reason =
        std::fs::read_to_string(dats_dir.join("rejected/notes.txt.reason.txt")).expect("reason");
    assert!(reason.contains("not a DAT"));
    let gone = Scheduler::run_inline(&app, Arc::new(DatImport::new(&junk)))
        .await
        .expect("run");
    let row = app
        .db
        .read(move |c| rows::get(c, gone))
        .await
        .expect("get")
        .expect("row");
    assert_eq!(row.state, JobState::Done, "a vanished file is not an error");

    let id = Scheduler::run_inline(&app, Arc::new(Recompute::new("gb")))
        .await
        .expect("run");
    let row = app
        .db
        .read(move |c| rows::get(c, id))
        .await
        .expect("get")
        .expect("row");
    assert_eq!(
        row.progress,
        Some(json!({ "groups": 1, "picks": 1, "matched": 0 }))
    );
    let remaps = app
        .db
        .read(|c| crate::db::jobs::count_kind(c, crate::jobs::remap::KIND))
        .await
        .expect("count");
    assert_eq!(remaps, 1, "a recompute queues a re-map of its platform");
}

/// A DB export of two NES games, the clone listed first, each with a headered and a headerless file.
fn db_export(version: &str) -> String {
    let file = |id: u8, sha1: char| {
        format!(
            "<source><details section=\"Trusted Dump\"/>\
             <file extension=\"nes\" size=\"32784\" crc32=\"0000000{id}\" sha1=\"{}\" header=\"4E 45 53 1A 02 01\" format=\"Headered\"/>\
             <file extension=\"unh\" size=\"32768\" crc32=\"1000000{id}\" sha1=\"{}\" format=\"Headerless\"/></source>",
            "e".repeat(40),
            sha1.to_string().repeat(40),
        )
    };
    format!(
        "<?xml version=\"1.0\"?><header><version>{version}</version></header><datafile>\
         <game name=\"Example Quest (USA)\"><archive number=\"0002\" clone=\"0001\" region=\"USA\" languages=\"En\"/>{}{}</game>\
         <game name=\"Example Quest (Japan)\"><archive number=\"0001\" clone=\"P\" region=\"Japan\" languages=\"Ja\"/>{}</game>\
         </datafile>",
        file(1, 'a'),
        file(1, 'a'),
        file(2, 'b'),
    )
}

#[tokio::test]
async fn a_zipped_db_export_loads_headerless_roms_with_clones() {
    let (_dir, app) = state();
    let dats_dir = app.config().paths.dats();
    std::fs::create_dir_all(&dats_dir).expect("mkdir");
    let stem = "Example Vendor - Nintendo Entertainment System (DB Export) (20260101-000000)";
    let pack = dats_dir.join(format!("{stem}.zip"));
    let member = format!("{stem}.xml");
    std::fs::write(&pack, zip_of(&[(&member, &db_export("20260101-000000"))])).expect("write");
    let id = Scheduler::run_inline(&app, Arc::new(DatImport::new(&pack)))
        .await
        .expect("run");
    let row = app
        .db
        .read(move |c| rows::get(c, id))
        .await
        .expect("get")
        .expect("row");
    assert_eq!(row.state, JobState::Done, "{:?}", row.progress);
    let (name, version, platform): (String, String, Option<String>) = app
        .db
        .read(|c| {
            Ok(c.query_row(
                "SELECT dat_name, version, platform_id FROM dat_versions",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?)
        })
        .await
        .expect("version");
    assert_eq!(
        name,
        "Example Vendor - Nintendo Entertainment System (DB Export)"
    );
    assert_eq!(version, "20260101-000000");
    assert_eq!(platform.as_deref(), Some("nes"));
    let roms: Vec<(String, String, i64, String, String, bool)> = app
        .db
        .read(|c| {
            let mut stmt = c.prepare(
                "SELECT t.name, r.name, r.size, r.sha1,
                        (SELECT json_group_array(language) FROM
                          (SELECT language FROM title_languages WHERE title_id = t.id ORDER BY pos)),
                        t.parent_id = p.id
                 FROM roms r JOIN titles t ON t.id = r.title_id
                 JOIN titles p ON p.name = 'Example Quest (Japan)' ORDER BY t.name",
            )?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                })?
                .collect::<rusqlite::Result<_>>()?;
            Ok(rows)
        })
        .await
        .expect("roms");
    assert_eq!(roms.len(), 2, "{roms:?}");
    assert_eq!(roms[1].0, "Example Quest (USA)");
    assert_eq!(roms[1].1, "Example Quest (USA).nes");
    assert_eq!(roms[1].2, 32_768);
    assert_eq!(roms[1].3, "a".repeat(40));
    assert_eq!(roms[1].4, "[\"En\"]");
    assert!(roms[1].5, "the clone is grouped under its parent");
    assert!(dats_dir.join(format!("loaded/{stem}.zip")).is_file());
}

#[test]
fn a_plain_db_export_takes_its_name_from_the_file() {
    let c = conn();
    let mut req = request(false, None);
    req.file_stem = "Example Vendor - Game Boy (DB Export) (7)".into();
    let xml = db_export("").replace("<version></version>", "");
    let o = import_stream(
        &c.db,
        Cursor::new(xml.as_bytes()),
        &req,
        "",
        export_parents(xml.as_bytes()).expect("index"),
    )
    .expect("import");
    let l = loaded(o);
    assert_eq!(l.platform.map(|p| p.0).as_deref(), Some("gb"));
    let version: String = c
        .with(|x| Ok(x.query_row("SELECT version FROM dat_versions", [], |r| r.get(0))?))
        .expect("version");
    assert_eq!(version, "7");
    assert_eq!(
        count(
            &c,
            "SELECT COUNT(*) FROM roms WHERE name LIKE '%.gb' AND size = 32768"
        ),
        2,
        "only the unh file matches the platform's image, named with its extension"
    );
}

/// One export game per file set: `(extension, format, size, header bytes)`, plus a save extra.
fn export_game(files: &[Sample]) -> String {
    let mut x = String::from(
        "<header/><datafile><game name=\"Example Quest (World)\"><archive number=\"1\" clone=\"P\"/><source>",
    );
    for (i, (ext, format, size, header)) in files.iter().enumerate() {
        let header = if *header > 0 {
            format!(" header=\"{}\"", "4e".repeat(*header))
        } else {
            String::new()
        };
        write!(
            x,
            "<file extension=\"{ext}\" format=\"{format}\" size=\"{size}\" crc32=\"{:08x}\"{header}/>",
            i + 1
        )
        .expect("write");
    }
    x.push_str("<file extension=\"sav\" size=\"8\" crc32=\"0000ffff\" item=\"Save\"/>");
    x.push_str("</source></game></datafile>");
    x
}

/// A file of a representative export: extension, format, size and header length.
type Sample = (String, &'static str, u64, usize);

/// The file sets a representative export lists for `id`, and the rom name and size wanted.
fn representative(id: &str, loads: &[&str]) -> Vec<(Vec<Sample>, String, u64)> {
    let named = |ext: &str| format!("Example Quest (World).{ext}");
    let f = |ext: &str, format, size, header| (ext.to_owned(), format, size, header);
    match id {
        "nes" => vec![(
            vec![
                f("nes", "Headered", 1040, 16),
                f("unh", "Headerless", 1024, 0),
            ],
            named("nes"),
            1024,
        )],
        "atari7800" => vec![(
            vec![
                f("a78", "Headered", 1152, 128),
                f("bin", "Headerless", 1024, 0),
            ],
            named("a78"),
            1024,
        )],
        "lynx" => vec![(
            vec![
                f("lnx", "Headered", 1088, 64),
                f("lyx", "Headerless", 1024, 0),
            ],
            named("lnx"),
            1024,
        )],
        "n64" => vec![(
            vec![
                f("z64", "BigEndian", 64, 0),
                f("v64", "ByteSwapped", 64, 0),
                f("n64", "LittleEndian", 64, 0),
            ],
            named("z64"),
            64,
        )],
        "sgx" => vec![(vec![f("pce", "", 64, 0)], named("sgx"), 64)],
        _ if loads.is_empty() => vec![(vec![f("zip", "", 64, 0)], named("zip"), 64)],
        _ => loads
            .iter()
            .map(|e| (vec![f(e, "", 64, 0)], named(e), 64))
            .collect(),
    }
}

#[test]
fn every_platform_takes_one_image_from_a_representative_export() {
    for p in &mistarr_mister::platforms::PLATFORMS {
        for (files, name, size) in representative(p.id, p.load_extensions) {
            let xml = export_game(&files);
            let dat = mistarr_core::dat::parse_dat_with(
                xml.as_bytes(),
                export_options(Some(p.id), HashMap::new()),
            )
            .expect("parse");
            let roms = &dat.games[0].roms;
            assert_eq!(roms.len(), 1, "{} from {files:?}: {roms:?}", p.id);
            assert_eq!(roms[0].name, name, "{}", p.id);
            assert_eq!(roms[0].size, size, "{}", p.id);
        }
    }
}

#[test]
fn archive_status_adds_the_stage_flags_a_name_lacks() {
    let c = conn();
    let req = request(false, None);
    let xml = "<header><version>1</version></header><datafile>\
        <game name=\"Example Quest (USA)\"><archive number=\"0001\" clone=\"P\" status=\"Proto 2\"/></game>\
        <game name=\"Example Quest (USA) (Beta)\"><archive number=\"0002\" clone=\"0001\" status=\"Beta\"/></game>\
        </datafile>";
    let mut req = req;
    req.file_stem = "Example Vendor - Nintendo Entertainment System (DB Export) (1)".into();
    loaded(
        import_stream(
            &c.db,
            Cursor::new(xml.as_bytes()),
            &req,
            "",
            export_parents(xml.as_bytes()).expect("index"),
        )
        .expect("import"),
    );
    let flags = |name: &str| -> String {
        let name = name.to_owned();
        c.with(move |x| {
            Ok(x.query_row(
                "SELECT (SELECT json_group_array(flag) FROM
                       (SELECT flag FROM title_flags WHERE title_id = t.id ORDER BY pos))
                     FROM titles t WHERE t.name = ?1",
                [name],
                |r| r.get(0),
            )?)
        })
        .expect("flags")
    };
    assert_eq!(flags("Example Quest (USA)"), "[\"proto\"]");
    assert_eq!(flags("Example Quest (USA) (Beta)"), "[\"beta\"]");
}

#[test]
fn unknown_xml_is_rejected_naming_the_formats() {
    let c = conn();
    let o = import(&c, "<softwarelist/>", &request(false, None));
    match o {
        Outcome::Rejected(r) => assert!(r.contains("DB export"), "{r}"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn recompute_is_enqueued_for_every_platform() {
    let (_dir, app) = state();
    Recompute::enqueue_all(&app).await.expect("enqueue");
    let n = app
        .db
        .read(|c| rows::count_kind(c, RECOMPUTE_KIND))
        .await
        .expect("count");
    assert_eq!(n, mistarr_mister::platforms::PLATFORMS.len() as u64);
}

#[test]
fn nameless_members_take_their_own_names() {
    let c = conn();
    let nameless = "<datafile><game name=\"A\"><rom name=\"a\" size=\"1\"/></game></datafile>";
    let req = request(false, None);
    let a = import_member(
        &c.db,
        Cursor::new(nameless.as_bytes()),
        &req,
        "sub/Alpha.dat",
    )
    .expect("import");
    let b =
        import_member(&c.db, Cursor::new(nameless.as_bytes()), &req, "Beta.xml").expect("import");
    let (a, b) = (loaded(a), loaded(b));
    assert_ne!(a.version, b.version, "members of one pack do not collide");
    let name = |id| {
        c.with(|x| dats::get(x, id))
            .expect("get")
            .expect("row")
            .dat_name
    };
    assert_eq!(
        (name(a.version), name(b.version)),
        ("Alpha".to_owned(), "Beta".to_owned())
    );
}

#[test]
fn reloading_a_version_with_fewer_games_retires_the_rest() {
    let c = conn();
    let both = dat(
        "Maker - Game Boy",
        "1",
        &[("Example Quest (USA)", None), ("Example Tale (USA)", None)],
    );
    loaded(import(&c, &both, &request(false, None)));
    let one = dat("Maker - Game Boy", "1", &[("Example Quest (USA)", None)]);
    let l = loaded(import(&c, &one, &request(false, None)));
    assert_eq!(l.retired, 1);
    assert_eq!(
        count(
            &c,
            "SELECT retired FROM titles WHERE name = 'Example Tale (USA)'"
        ),
        1
    );
    assert_eq!(
        count(
            &c,
            "SELECT retired FROM titles WHERE name = 'Example Quest (USA)'"
        ),
        0
    );
}

#[test]
fn binding_an_older_version_is_rejected_and_rolled_back() {
    let c = conn();
    let v1 = dat("Test Console", "1", &[("Example Quest (USA)", None)]);
    let old = loaded(import(&c, &v1, &request(false, None)));
    let v2 = dat("Test Console", "2", &[("Example Quest (USA)", None)]);
    let newer = loaded(import(&c, &v2, &request(false, None)));
    let bind_newer = Bind {
        version: newer.version,
        platform: PlatformId("nes".into()),
        dat_name: "Test Console".into(),
        dat_version: "2".into(),
    };
    assert!(loaded(import(&c, &v2, &request(false, Some(bind_newer)))).has_titles);
    let bind = Bind {
        version: old.version,
        platform: PlatformId("nes".into()),
        dat_name: "Test Console".into(),
        dat_version: "1".into(),
    };
    let o = import(&c, &v1, &request(false, Some(bind)));
    assert!(
        matches!(&o, Outcome::Rejected(r) if r.contains("newer version")),
        "{o:?}"
    );
    let row = c
        .with(|x| dats::get(x, old.version))
        .expect("get")
        .expect("row");
    assert_eq!(row.platform_id, None);
    assert_eq!(
        count(&c, "SELECT COUNT(*) FROM titles"),
        1,
        "only the bound newer version"
    );
}

#[test]
fn an_unbound_version_never_blocks_binding_one_of_its_family() {
    let c = conn();
    let v1 = dat("Test Console", "1", &[("Example Quest (USA)", None)]);
    let old = loaded(import(&c, &v1, &request(false, None)));
    loaded(import(
        &c,
        &dat("Test Console", "2", &[("Example Quest (USA)", None)]),
        &request(false, None),
    ));
    let bind = Bind {
        version: old.version,
        platform: PlatformId("nes".into()),
        dat_name: "Test Console".into(),
        dat_version: "1".into(),
    };
    assert!(loaded(import(&c, &v1, &request(false, Some(bind)))).has_titles);
}

async fn job_state(app: &AppState, id: crate::db::jobs::JobId) -> rows::JobRow {
    app.db
        .read(move |c| rows::get(c, id))
        .await
        .expect("get")
        .expect("row")
}

#[tokio::test]
async fn binding_fails_loudly_and_finds_renamed_nameless_files() {
    let (_dir, app) = state();
    let dats_dir = app.config().paths.dats();
    let loaded_dir = dats_dir.join(LOADED_DIR);
    std::fs::create_dir_all(&loaded_dir).expect("mkdir");
    let mut events = app.events.subscribe(None).live;
    let nameless = |v: &str| {
        format!("<datafile><header><version>{v}</version></header><game name=\"A\"><rom name=\"a\" size=\"1\"/></game></datafile>")
    };
    for v in ["1", "2"] {
        let path = dats_dir.join("odd.dat");
        std::fs::write(&path, nameless(v)).expect("write");
        Scheduler::run_inline(&app, Arc::new(DatImport::new(&path)))
            .await
            .expect("run");
    }
    let (rows, _) = app.db.read(|c| dats::list(c, 10, 0)).await.expect("list");
    let newest = rows.iter().find(|r| r.version == "2").expect("v2").clone();
    assert_eq!(
        (newest.dat_name.as_str(), newest.source_file.as_str()),
        ("odd", "odd (1).dat")
    );

    let id = Scheduler::run_inline(&app, Arc::new(DatImport::bind(&newest, "nes", &loaded_dir)))
        .await
        .expect("run");
    assert_eq!(job_state(&app, id).await.state, JobState::Done);
    let v = newest.id;
    let bound = app
        .db
        .read(move |c| dats::get(c, v))
        .await
        .expect("get")
        .expect("row");
    assert_eq!(bound.platform_id, Some(PlatformId("nes".into())));

    let mut missing = rows.iter().find(|r| r.version == "1").expect("v1").clone();
    missing.source_file = "gone.dat".into();
    let id = Scheduler::run_inline(
        &app,
        Arc::new(DatImport::bind(&missing, "nes", &loaded_dir)),
    )
    .await
    .expect("run");
    assert_eq!(job_state(&app, id).await.state, JobState::Failed);
    let mut rejected = None;
    while let Ok(e) = events.try_recv() {
        if e.kind == EventKind::DatRejected {
            rejected = Some(e.data.clone());
        }
    }
    let rejected = rejected.expect("dat.rejected");
    assert!(
        rejected.contains("gone.dat") && rejected.contains("no longer"),
        "{rejected}"
    );
    let v = missing.id;
    let row = app
        .db
        .read(move |c| dats::get(c, v))
        .await
        .expect("get")
        .expect("row");
    assert_eq!(row.platform_id, None, "still unbound");
}

#[test]
fn a_forgotten_file_is_reported_again() {
    let dir = tempfile::tempdir().expect("tempdir");
    let a = dir.path().join("a.dat");
    std::fs::write(&a, b"x").expect("write");
    let mut w = DatWatcher::new(Duration::ZERO);
    w.poll(dir.path());
    assert_eq!(w.poll(dir.path()), std::slice::from_ref(&a));
    assert!(w.poll(dir.path()).is_empty());
    w.forget(&a);
    assert_eq!(w.poll(dir.path()), std::slice::from_ref(&a));
}

const NES_LOGIQX: &str = "Example Vendor - Nintendo Entertainment System (Headered)";
const NES_SAMPLES: &str = "Example Samples - Nintendo Entertainment System (Headered)";

/// Loads a DB export under the file name `stem` at time `now`.
fn import_export(c: &TestDb, xml: &str, stem: &str, now: i64) -> Loaded {
    let mut req = request(false, None);
    req.file_stem = stem.into();
    req.now = now;
    let parents = export_parents(xml.as_bytes()).expect("index");
    loaded(import_stream(&c.db, Cursor::new(xml.as_bytes()), &req, "", parents).expect("import"))
}

fn import_at(c: &TestDb, xml: &str, now: i64) -> Loaded {
    let mut req = request(false, None);
    req.now = now;
    loaded(import(c, xml, &req))
}

#[test]
fn an_export_after_a_logiqx_dat_of_the_system_leaves_one_live_set() {
    let c = conn();
    let games = [
        ("Example Quest (Japan)", None),
        ("Example Quest (USA)", Some("Example Quest (Japan)")),
        ("Mock Manor (World)", None),
    ];
    let logiqx = import_at(&c, &dat(NES_LOGIQX, "20260101-000000", &games), 1);
    assert_eq!(logiqx.platform.as_ref().map(|p| p.0.as_str()), Some("nes"));
    let stem = "Example Vendor - Nintendo Entertainment System (DB Export) (20260102-000000)";
    let export = import_export(&c, &db_export(""), stem, 2);
    assert_eq!(export.platform, logiqx.platform);
    assert_eq!(
        count(
            &c,
            "SELECT COUNT(*) FROM dat_versions WHERE superseded_by IS NULL"
        ),
        1
    );
    assert_eq!(
        count(&c, "SELECT COUNT(*) FROM titles WHERE retired = 0"),
        2,
        "the titles the export lists, once each"
    );
    assert_eq!(
        count(&c, "SELECT COUNT(*) FROM titles"),
        3,
        "titles of the same name are reused across forms"
    );
    let older = import_at(&c, &dat(NES_LOGIQX, "20260101-120000", &games), 3);
    assert!(!older.has_titles, "an older version loaded later stays out");
    assert_eq!(
        count(&c, "SELECT COUNT(*) FROM titles WHERE retired = 0"),
        2
    );
}

/// Stores fully hashed files `(path, crc32, rom id)` of 4 bytes on NES, removes `version`
/// as `DELETE /dats/{id}` does and recomputes; returns how many files were matched again.
fn remove_with_files(c: &TestDb, version: DatVersionId, files: &[(&str, &str, i64)]) -> usize {
    c.with(|x| {
        let nes = PlatformId("nes".into());
        let (md5, sha1) = ("d".repeat(32), "d".repeat(40));
        for (path, crc, id) in files {
            let hashed = crate::db::files::Hashed {
                crc32: Some(crc),
                md5: Some(&md5),
                sha1: Some(&sha1),
                header_rule: Some("none"),
            };
            let state = crate::db::files::FileState::Misnamed;
            crate::db::files::upsert(x, &nes, path, 4, 1, &hashed, Some(*id), state, 1)?;
        }
        dats::retire(x, version, 1)?;
        let matched = rematch_chunk(x, &nes)?;
        titles::recompute_platform(x, "nes", &Prefs::default())?;
        Ok(matched)
    })
    .expect("remove")
}

#[test]
fn an_add_on_dat_coexists_and_shares_groups_by_rom() {
    let c = conn();
    let official = [("Example Quest (USA)", None), ("Mock Manor (World)", None)];
    import_at(&c, &dat(NES_LOGIQX, "1", &official), 1);
    let samples = [
        ("Example Quest (USA) (Sample Copy)", None),
        ("Mock Manor (World) (Sample Copy)", None),
        ("Sample Only (World)", None),
    ];
    let added = import_at(&c, &dat(NES_SAMPLES, "1", &samples), 2);
    assert_eq!(
        count(
            &c,
            "SELECT COUNT(*) FROM dat_versions WHERE superseded_by IS NULL"
        ),
        2
    );
    assert_eq!(
        count(&c, "SELECT COUNT(*) FROM titles WHERE retired = 0"),
        5
    );
    assert_eq!(
        count(
            &c,
            "SELECT COUNT(DISTINCT group_root) FROM titles WHERE retired = 0"
        ),
        3,
        "a game both DATs list with the same roms is one group"
    );
    assert_eq!(
        count(
            &c,
            "SELECT COUNT(*) FROM titles WHERE retired = 0 AND is_1g1r_pick = 1"
        ),
        3
    );
    let rom = |name: &str| {
        count(
            &c,
            &format!("SELECT r.id FROM roms r JOIN titles t ON t.id = r.title_id WHERE t.name = '{name}'"),
        )
    };
    let (shared, own) = (
        rom("Example Quest (USA) (Sample Copy)"),
        rom("Sample Only (World)"),
    );
    let files = [("a.bin", "00000000", shared), ("b.bin", "00000002", own)];
    assert_eq!(remove_with_files(&c, added.version, &files), 2);
    assert_eq!(
        count(
            &c,
            "SELECT COUNT(*) FROM files WHERE rom_id IS NULL AND state = 'unverified'"
        ),
        1,
        "a file only the removed DAT listed becomes unmatched"
    );
    assert_eq!(
        count(
            &c,
            "SELECT r.id FROM files f JOIN roms r ON r.id = f.rom_id WHERE f.rel_path = 'a.bin'"
        ),
        rom("Example Quest (USA)"),
        "a file another live DAT lists matches it again"
    );
    assert_eq!(
        count(
            &c,
            "SELECT COUNT(DISTINCT group_root) FROM titles WHERE retired = 0"
        ),
        2
    );
    assert_eq!(
        count(
            &c,
            "SELECT COUNT(*) FROM dat_versions WHERE superseded_by IS NULL AND retired = 0"
        ),
        1
    );
}

#[test]
fn an_export_header_lets_placement_add_it_back() {
    let c = conn();
    let header = "4E 45 53 1A 02 01 00 00 00 00 00 00 00 00 00 00";
    let xml = format!(
        "<header/><datafile><game name=\"Example Quest (World)\"><archive number=\"1\" clone=\"P\"/><source>\
         <file extension=\"nes\" size=\"32784\" crc32=\"00000001\" header=\"{header}\" format=\"Headered\"/>\
         <file extension=\"unh\" size=\"32768\" crc32=\"00000002\" format=\"Headerless\"/>\
         </source></game></datafile>"
    );
    let stem = "Example Vendor - Nintendo Entertainment System (DB Export) (1)";
    import_export(&c, &xml, stem, 1);
    let (name, size, stored): (String, i64, String) = c
        .with(|x| {
            Ok(x.query_row("SELECT name, size, header FROM roms", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?)
        })
        .expect("rom");
    let bytes = crate::jobs::import::parse_header(&stored).expect("hex");
    assert_eq!(bytes.len(), 16);
    let entry = mistarr_mister::DatEntry {
        name: "Example Quest (World)".into(),
        roms: vec![mistarr_mister::DatRom {
            name,
            size: u64::try_from(size).expect("size"),
            header: Some(bytes.clone()),
        }],
    };
    let staged = mistarr_mister::StagedFile {
        path: "x.nes".into(),
        size: u64::try_from(size).expect("size"),
        kind: mistarr_mister::StagedKind::File,
        head: vec![0xA9, 0x00],
        members: Vec::new(),
    };
    let nes = mistarr_mister::adapter_for(&PlatformId("nes".into())).expect("nes");
    let plan = nes.plan_placement(&entry, &staged).expect("plan");
    assert!(
        plan.steps.contains(&mistarr_mister::Step::AddHeader {
            file: "x.nes".into(),
            bytes
        }),
        "{plan:?}"
    );
}

#[test]
fn an_add_on_never_merges_two_groups_of_one_dat() {
    let c = conn();
    let official = [("Alpha Game (World)", None), ("Beta Game (World)", None)];
    import_at(&c, &dat(NES_LOGIQX, "1", &official), 1);
    let groups = |c: &TestDb| {
        (
            count(
                c,
                "SELECT COUNT(DISTINCT group_root) FROM titles WHERE retired = 0",
            ),
            count(
                c,
                "SELECT COUNT(*) FROM titles WHERE retired = 0 AND is_1g1r_pick = 1",
            ),
        )
    };
    assert_eq!(groups(&c), (2, 2));
    let add_on = [
        ("Gamma Pack (World)", None),
        ("Delta Pack (World)", Some("Gamma Pack (World)")),
    ];
    let added = import_at(&c, &dat(NES_SAMPLES, "1", &add_on), 2);
    assert_eq!(
        groups(&c),
        (2, 2),
        "each add-on title joins the group of its match, and the two groups stay apart"
    );
    assert_eq!(
        count(
            &c,
            "SELECT COUNT(DISTINCT parent_id) FROM titles WHERE retired = 0"
        ),
        3,
        "parent_id keeps each DAT's own clone groups"
    );
    remove_with_files(&c, added.version, &[]);
    assert_eq!(groups(&c), (2, 2));
}

#[test]
fn a_clone_left_by_its_linked_parent_keeps_a_group_of_its_own() {
    let c = conn();
    let official = [
        ("Alpha Game (World)", None),
        ("Beta Game (World)", None),
        ("Zeta Game (World)", None),
    ];
    import_at(&c, &dat(NES_LOGIQX, "1", &official), 1);
    let add_on = format!(
        "<datafile><header><name>{NES_SAMPLES}</name><version>1</version></header>\
         <game name=\"Gamma Pack (World)\"><rom name=\"0.bin\" size=\"4\" crc=\"00000000\"/></game>\
         <game name=\"Delta Pack (World)\" cloneof=\"Gamma Pack (World)\">\
         <rom name=\"3.bin\" size=\"4\" crc=\"00000003\"/></game></datafile>"
    );
    let added = import_at(&c, &add_on, 2);
    let id = |name: &str| {
        count(
            &c,
            &format!("SELECT id FROM titles WHERE name = '{name}' AND retired = 0"),
        )
    };
    let (alpha, gamma, delta) = (
        id("Alpha Game (World)"),
        id("Gamma Pack (World)"),
        id("Delta Pack (World)"),
    );
    let root_of = |t: i64| count(&c, &format!("SELECT group_root FROM titles WHERE id = {t}"));
    assert_eq!(root_of(gamma), alpha, "the parent links to its match");
    assert_eq!(root_of(delta), delta, "the clone roots its own group");
    assert_eq!(
        count(
            &c,
            "SELECT COUNT(*) FROM titles t JOIN titles r ON r.id = t.group_root
             WHERE t.retired = 0 AND r.group_root IS NOT r.id"
        ),
        0,
        "no title keeps a root that left its group"
    );
    assert_eq!(count(&c, "SELECT COUNT(*) FROM title_groups"), 4);
    let members = format!("SELECT COUNT(*) FROM titles WHERE group_root = {delta}");
    assert_eq!(count(&c, &members), 1);
    remove_with_files(&c, added.version, &[]);
    let live = "SELECT COUNT(DISTINCT group_root) FROM titles WHERE retired = 0";
    assert_eq!(count(&c, live), 3);
}

#[test]
fn one_family_on_two_platforms_keeps_separate_titles() {
    let c = conn();
    let t = titles::TitleInput {
        name: "Example Quest (World)",
        base_name: "Example Quest",
        group_key: "example quest",
        clone_of: None,
        regions: &[],
        languages: &[],
        revision: None,
        flags: &[],
    };
    let ids = c
        .with(|x| {
            let mut ids = Vec::new();
            for (version, platform) in [("1", "gb"), ("2", "gbc")] {
                let v = dats::upsert_version(
                    x,
                    &NewVersion {
                        dat_name: "Example Vendor - Example Handheld",
                        version,
                        source_file: "h.dat",
                        platform: Some(platform),
                        now: 1,
                    },
                )?;
                assert!(v.current, "{platform}");
                ids.push(titles::upsert_title(x, platform, v.id, &t, &[])?);
            }
            Ok(ids)
        })
        .expect("titles");
    assert_ne!(ids[0], ids[1]);
    assert_eq!(
        count(&c, "SELECT COUNT(*) FROM titles WHERE platform_id = 'gb'"),
        1
    );
}

#[test]
fn a_disc_track_is_matched_again_under_the_all_or_nothing_rule() {
    let c = conn();
    let psx = PlatformId("psx".into());
    let h = |n: u8| mistarr_core::HashSet {
        size: 4,
        crc32: format!("{n:08x}"),
        md5: format!("{n:032x}"),
        sha1: format!("{n:040x}"),
    };
    let track = |n: u8| format!("Example Disc (USA) (Track {n}).bin");
    let states = c
        .with(|x| {
            let old = files::seed_title_fixture(x, &psx, "Example Disc (USA)")?;
            let new = files::seed_title_fixture(x, &psx, "Example Disc (USA) (Rev 1)")?;
            for n in 1..=3 {
                files::seed_rom_for_title_fixture(x, new, &track(n), &h(n), "good")?;
            }
            for n in 1..=2 {
                let rom = files::seed_rom_for_title_fixture(x, old, &track(n), &h(n), "good")?;
                let sums = h(n);
                let hashed = files::Hashed {
                    crc32: Some(&sums.crc32),
                    md5: Some(&sums.md5),
                    sha1: Some(&sums.sha1),
                    header_rule: Some("none"),
                };
                let path = format!("PSX/Example Disc (USA)/{}", track(n));
                files::upsert(
                    x,
                    &psx,
                    &path,
                    4,
                    1,
                    &hashed,
                    Some(rom),
                    FileState::Verified,
                    1,
                )?;
            }
            x.execute("UPDATE titles SET retired = 1 WHERE id = ?1", [old])?;
            assert_eq!(rematch_chunk(x, &psx)?, 2);
            let rows: Vec<(String, bool)> = x
                .prepare(
                    "SELECT f.state, t.name = 'Example Disc (USA) (Rev 1)' FROM files f
                     JOIN roms r ON r.id = f.rom_id JOIN titles t ON t.id = r.title_id",
                )?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            Ok(rows)
        })
        .expect("rematch");
    assert_eq!(
        states,
        [
            ("unverified".to_owned(), true),
            ("unverified".to_owned(), true)
        ],
        "two of the live title's three tracks are not a complete game"
    );
}

/// Synthetic hashes numbered `n`, of a 4-byte payload.
fn sums(n: u32) -> mistarr_core::HashSet {
    mistarr_core::HashSet {
        size: 4,
        crc32: format!("{n:08x}"),
        md5: format!("{n:032x}"),
        sha1: format!("{n:040x}"),
    }
}

/// Stores an unmatched 4-byte file row with `sums`, as a scan with no DAT leaves it.
fn unmatched_file(
    c: &Connection,
    platform: &PlatformId,
    path: &str,
    sums: &mistarr_core::HashSet,
) -> Result<files::FileId> {
    let hashed = files::Hashed {
        crc32: Some(&sums.crc32),
        md5: Some(&sums.md5),
        sha1: Some(&sums.sha1),
        header_rule: Some("none"),
    };
    files::upsert(
        c,
        platform,
        path,
        4,
        1,
        &hashed,
        None,
        FileState::Unverified,
        1,
    )
}

#[tokio::test]
async fn recompute_matches_unmatched_files_and_updates_have() {
    let (_dir, app) = state();
    let gb = PlatformId("gb".into());
    app.db
        .write(move |c| {
            let tx = c.transaction()?;
            let quest = ("Example Quest (USA)", "Example Quest (USA).gb");
            files::seed_rom_fixture(&tx, &gb, quest.0, quest.1, &sums(1), "good")?;
            let manor = ("Mock Manor (USA)", "Mock Manor (USA).gb");
            files::seed_rom_fixture(&tx, &gb, manor.0, manor.1, &sums(2), "good")?;
            titles::recompute_platform(&tx, "gb", &Prefs::default())?;
            // More strays than one chunk, so the cursor pages past files that never match.
            for n in 0..300 {
                unmatched_file(&tx, &gb, &format!("GAMEBOY/stray {n}.gb"), &sums(1000 + n))?;
            }
            unmatched_file(&tx, &gb, "GAMEBOY/Example Quest (USA).gb", &sums(1))?;
            unmatched_file(&tx, &gb, "GAMEBOY/Other Name.gb", &sums(2))?;
            crate::db::commit(tx)
        })
        .await
        .expect("seed");
    let have = |app: Arc<AppState>| async move {
        let sql = "SELECT SUM(have_verified) FROM title_groups WHERE platform_id = 'gb'";
        app.db
            .read(move |c| Ok(c.query_row(sql, [], |r| r.get::<_, i64>(0))?))
            .await
            .expect("have")
    };
    assert_eq!(have(app.clone()).await, 0, "nothing matched yet");

    let run = Scheduler::run_inline(&app, Arc::new(Recompute::new("gb")));
    let id = tokio::time::timeout(Duration::from_secs(30), run)
        .await
        .expect("a file that stays unmatched is read once, so the recompute ends")
        .expect("run");
    let row = app
        .db
        .read(move |c| rows::get(c, id))
        .await
        .expect("get")
        .expect("row");
    let matched = row.progress.as_ref().map(|p| p["matched"].clone());
    assert_eq!(matched, Some(json!(2)));
    let states: Vec<(String, String)> = app
        .db
        .read(|c| {
            let sql =
                "SELECT rel_path, state FROM files WHERE rom_id IS NOT NULL ORDER BY rel_path";
            Ok(c.prepare(sql)?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?)
        })
        .await
        .expect("states");
    let pair = |p: &str, s: &str| (p.to_owned(), s.to_owned());
    assert_eq!(
        states,
        [
            pair("GAMEBOY/Example Quest (USA).gb", "verified"),
            pair("GAMEBOY/Other Name.gb", "misnamed"),
        ]
    );
    assert_eq!(
        have(app.clone()).await,
        1,
        "the verified file's group is have"
    );
}

#[test]
fn unmatched_pages_end_on_a_file_that_never_matches() {
    let c = conn();
    let nes = PlatformId("nes".into());
    let (first, pages) = c
        .with(|x| {
            let first = unmatched_file(x, &nes, "NES/stray.nes", &sums(7))?;
            let mut after = files::FileId(0);
            let mut pages = 0;
            loop {
                let chunk = match_unmatched_chunk(x, &nes, after)?;
                pages += 1;
                after = chunk.last;
                if chunk.read < REMATCH_CHUNK as usize {
                    break;
                }
            }
            let again = match_unmatched_chunk(x, &nes, after)?;
            assert_eq!(
                (again.read, again.last),
                (0, after),
                "nothing past the cursor"
            );
            Ok((first, pages))
        })
        .expect("page");
    assert_eq!(pages, 1);
    let state = c
        .with(|x| Ok(files::get(x, first)?.map(|f| (f.rom_id, f.state))))
        .expect("get");
    assert_eq!(state, Some((None, FileState::Unverified)));
}

#[test]
fn a_stored_crc_matches_a_headered_file_by_its_size_less_the_header() {
    let c = conn();
    let nes = PlatformId("nes".into());
    let state = c
        .with(|x| {
            let rom = sums(9);
            let title = files::seed_title_fixture(x, &nes, "Crc Quest (USA)")?;
            x.execute(
                "INSERT INTO roms (title_id, name, size, crc32)
                 VALUES (?1, 'Crc Quest (USA).nes', 4, ?2)",
                rusqlite::params![title, rom.crc32],
            )?;
            let hashed = files::Hashed {
                crc32: Some(&rom.crc32),
                md5: Some(&rom.md5),
                sha1: Some(&rom.sha1),
                header_rule: Some("ines"),
            };
            let path = "NES/Crc Quest (USA).nes";
            let unverified = FileState::Unverified;
            let id = files::upsert(x, &nes, path, 20, 1, &hashed, None, unverified, 1)?;
            match_unmatched_chunk(x, &nes, files::FileId(0))?;
            Ok(files::get(x, id)?.map(|f| f.state))
        })
        .expect("match");
    assert_eq!(
        state,
        Some(FileState::Verified),
        "20 bytes on disk hash as the 4 after the iNES header"
    );
}

#[test]
fn a_stored_crc_allows_for_a_copier_header_only_at_its_size() {
    let c = conn();
    let snes = PlatformId("snes".into());
    let states = c
        .with(|x| {
            let rom = sums(11);
            let title = files::seed_title_fixture(x, &snes, "Copier Quest (USA)")?;
            x.execute(
                "INSERT INTO roms (title_id, name, size, crc32)
                 VALUES (?1, 'Copier Quest (USA).sfc', 1024, ?2)",
                rusqlite::params![title, rom.crc32],
            )?;
            let hashed = files::Hashed {
                crc32: Some(&rom.crc32),
                md5: Some(&rom.md5),
                sha1: Some(&rom.sha1),
                header_rule: Some("smc"),
            };
            let unverified = FileState::Unverified;
            let mut ids = Vec::new();
            // 1536 is 1024 plus a 512-byte copier header; 1040 is no copier size.
            for (path, size) in [("SNES/Copier Quest (USA).sfc", 1536), ("SNES/b.sfc", 1040)] {
                ids.push(files::upsert(
                    x, &snes, path, size, 1, &hashed, None, unverified, 1,
                )?);
            }
            match_unmatched_chunk(x, &snes, files::FileId(0))?;
            let mut states = Vec::new();
            for id in ids {
                states.push(files::get(x, id)?.map(|f| f.state));
            }
            Ok(states)
        })
        .expect("match");
    assert_eq!(
        states,
        [Some(FileState::Verified), Some(FileState::Unverified)]
    );
}

#[test]
fn a_recompute_that_changes_nothing_writes_nothing() {
    let c = conn();
    let nes = PlatformId("nes".into());
    let changes = c
        .with(|x| {
            unmatched_file(x, &nes, "NES/stray.nes", &sums(12))?;
            let before = x.total_changes();
            match_unmatched_chunk(x, &nes, files::FileId(0))?;
            Ok(x.total_changes() - before)
        })
        .expect("page");
    assert_eq!(changes, 0);
}

/// Every row of every table, the search index's included, as sorted text; the times
/// migrations ran are left out.
fn dump(db: &Db) -> Vec<String> {
    db.read_blocking(|c| {
        let tables: Vec<String> = c
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'table' \
                 AND name NOT LIKE 'sqlite_%' AND name != 'schema_version' ORDER BY name",
            )?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let mut out = Vec::new();
        for t in tables {
            let mut stmt = c.prepare(&format!("SELECT * FROM \"{t}\""))?;
            let n = stmt.column_count();
            let mut rows: Vec<String> = stmt
                .query_map([], |r| {
                    let mut line = t.clone();
                    for i in 0..n {
                        let v: rusqlite::types::Value = r.get(i)?;
                        let _ = write!(line, "|{v:?}");
                    }
                    Ok(line)
                })?
                .collect::<rusqlite::Result<_>>()?;
            rows.sort();
            out.extend(rows);
        }
        Ok(out)
    })
    .expect("dump")
}

/// A seeded database with unmatched gb files a DAT of [`dat`] matches by CRC32 and size.
fn with_files() -> TestDb {
    let c = conn();
    let gb = PlatformId("gb".into());
    c.with(|x| {
        for n in [0, 1, 2, 9] {
            unmatched_file(x, &gb, &format!("GB/file {n}.gb"), &sums(n))?;
        }
        Ok(())
    })
    .expect("files");
    c
}

/// Loads each DAT in turn through [`import_all`], on `c` itself or on a copy in RAM that
/// is then swapped in, and returns every outcome.
fn load_all(c: &TestDb, dats: &[String], via_ram: bool) -> Vec<Outcome> {
    let dir = c.dir.path();
    let mut outcomes = Vec::new();
    for (i, xml) in dats.iter().enumerate() {
        let path = dir.join(format!("{i}.dat"));
        std::fs::write(&path, xml).expect("write");
        let mut req = request(false, None);
        req.now = i64::try_from(i).expect("small") + 1;
        let mut progress = |_: &Db, _, _| Ok(());
        let (out, _) = if via_ram {
            let ram_dir = crate::db::testutil::ram_dir();
            let plan = ram::Plan {
                dir: ram_dir.path().to_path_buf(),
                floor: 0,
                job: 1,
                input: 0,
            };
            let ran =
                c.db.hold_writer_blocking(|h| {
                    ram::run(h, &plan, &mut (), |db| {
                        import_all(db, &path, &[Member::Plain], &req, &mut progress)
                    })
                })
                .expect("run");
            match ran {
                Ram::Done(out, _) => (out, true),
                Ram::Fallback(reason) => panic!("fell back: {reason:?}"),
            }
        } else {
            import_all(&c.db, &path, &[Member::Plain], &req, &mut progress).expect("import")
        };
        outcomes.extend(out);
    }
    outcomes
}

#[test]
fn an_import_in_ram_stores_the_same_rows_as_one_in_place() {
    let gb = "Maker - Game Boy";
    let cases: Vec<(&str, Vec<String>)> = vec![
        (
            "clones and picks",
            vec![dat(
                gb,
                "1",
                &[
                    ("Example Quest (USA)", None),
                    ("Example Quest (Japan)", Some("Example Quest (USA)")),
                    ("Mock Manor (Europe)", None),
                ],
            )],
        ),
        (
            "inferred groups",
            vec![dat(
                gb,
                "1",
                &[
                    ("Example Quest (Europe) (Rev 1)", None),
                    ("Example Quest (USA)", None),
                    ("Sample Tale (World) (Beta)", None),
                ],
            )],
        ),
        (
            "a newer version retires what it lacks",
            vec![
                dat(
                    gb,
                    "1",
                    &[
                        ("Example Quest (USA)", None),
                        ("Mock Manor (USA)", None),
                        ("Sample Tale (USA)", None),
                    ],
                ),
                dat(
                    gb,
                    "2",
                    &[("Sample Tale (USA)", None), ("Example Quest (USA)", None)],
                ),
            ],
        ),
        (
            "an unbound DAT",
            vec![dat("Test Console", "1", &[("Example Quest (USA)", None)])],
        ),
        ("a rejected DAT", vec!["<datafile><game".to_owned()]),
    ];
    for (name, dats) in cases {
        let (in_place, in_ram) = (with_files(), with_files());
        let a = load_all(&in_place, &dats, false);
        let b = load_all(&in_ram, &dats, true);
        assert_eq!(a, b, "{name}: outcomes");
        let (rows_a, rows_b) = (dump(&in_place.db), dump(&in_ram.db));
        let stored = rows_a.iter().any(|r| r.starts_with("dat_versions|"));
        assert_eq!(stored, name != "a rejected DAT", "{name}");
        assert_eq!(rows_a, rows_b, "{name}: rows");
    }
}

fn job_row(app: &AppState, id: crate::db::jobs::JobId) -> rows::JobRow {
    app.db
        .read_blocking(|c| rows::get(c, id))
        .expect("get")
        .expect("row")
}

fn count_kind(app: &AppState, kind: &str) -> u64 {
    app.db
        .read_blocking(|c| rows::count_kind(c, kind))
        .expect("count")
}

#[tokio::test]
async fn a_dat_job_imports_in_ram_and_queues_the_remap_its_recompute_ends_with() {
    let (dir, app) = state();
    let dats_dir = app.config().paths.dats();
    std::fs::create_dir_all(&dats_dir).expect("mkdir");
    let path = dats_dir.join("gb.dat");
    let xml = dat(
        "Maker - Game Boy",
        "1",
        &[("Example Quest (USA)", None), ("Mock Manor (USA)", None)],
    );
    std::fs::write(&path, xml).expect("write");
    let mut events = app.events.subscribe(None).live;
    let id = Scheduler::run_inline(&app, Arc::new(DatImport::new(&path)))
        .await
        .expect("run");
    let row = job_row(&app, id);
    assert_eq!(row.state, JobState::Done, "{:?}", row.progress);
    assert_eq!(
        row.progress.as_ref().and_then(|p| p["phase"].as_str()),
        Some("importing"),
        "{:?}",
        row.progress
    );
    let titles: i64 = app
        .db
        .read_blocking(|c| Ok(c.query_row("SELECT COUNT(*) FROM titles", [], |r| r.get(0))?))
        .expect("titles");
    assert_eq!(titles, 2);
    assert_eq!(
        count_kind(&app, RECOMPUTE_KIND),
        0,
        "the recompute ran in RAM"
    );
    assert_eq!(count_kind(&app, crate::jobs::remap::KIND), 1);
    let mut phases = Vec::new();
    while let Ok(e) = events.try_recv() {
        if e.kind == EventKind::JobProgress {
            let body: Value = serde_json::from_str(&e.data).expect("json");
            let running = body["state"] == "running";
            if let Some(p) = body["progress"]["phase"].as_str().filter(|_| running) {
                phases.push(p.to_owned());
            }
        }
    }
    phases.dedup();
    assert_eq!(
        phases.first().map(String::as_str),
        Some("copying the database to memory"),
        "{phases:?}"
    );
    assert_eq!(
        phases.last().map(String::as_str),
        Some("writing the database to the card"),
        "{phases:?}"
    );
    assert!(phases.iter().any(|p| p == "reading"), "{phases:?}");
    assert!(!dir.path().join("data/mistarr.db.new").exists());
    let left = std::fs::read_dir(dir.ram()).map_or(0, Iterator::count);
    assert_eq!(left, 0, "the copy in RAM is removed");
}

#[tokio::test]
async fn short_memory_imports_in_place_and_says_why() {
    let (_dir, app) = state();
    app.update_config(|c| c.memory.import_floor_mib = 1 << 40);
    let dats_dir = app.config().paths.dats();
    std::fs::create_dir_all(&dats_dir).expect("mkdir");
    let path = dats_dir.join("gb.dat");
    let xml = dat("Maker - Game Boy", "1", &[("Example Quest (USA)", None)]);
    std::fs::write(&path, xml).expect("write");
    let id = Scheduler::run_inline(&app, Arc::new(DatImport::new(&path)))
        .await
        .expect("run");
    let row = job_row(&app, id);
    assert_eq!(row.state, JobState::Done, "{:?}", row.progress);
    let progress = row.progress.expect("progress");
    assert_eq!(progress["phase"], IN_PLACE);
    assert!(
        progress["reason"]
            .as_str()
            .is_some_and(|r| r == ram::why::SHORT),
        "{progress}"
    );
    assert_eq!(progress["games"], 1);
    assert_eq!(
        count_kind(&app, RECOMPUTE_KIND),
        1,
        "the recompute is queued"
    );
}

#[test]
fn a_copy_that_fills_partway_through_the_load_falls_back_with_the_card_untouched() {
    let c = conn();
    let names: Vec<String> = (0..STAGE_CHUNK * 3)
        .map(|i| format!("Game {i} (USA)"))
        .collect();
    let games: Vec<(&str, Option<&str>)> = names.iter().map(|n| (n.as_str(), None)).collect();
    let path = c.dir.path().join("big.dat");
    std::fs::write(&path, dat("Maker - Game Boy", "1", &games)).expect("write");
    let before = dump(&c.db);
    let ram_dir = crate::db::testutil::ram_dir();
    let plan = ram::Plan {
        dir: ram_dir.path().to_path_buf(),
        floor: 0,
        job: 1,
        input: 0,
    };
    let req = request(false, None);
    let out =
        c.db.hold_writer_blocking(|h| {
            ram::run(h, &plan, &mut (), |db| {
                // The copy's file system fills a few pages into the load.
                db.write_blocking(|w| {
                    let pages: i64 = w.pragma_query_value(None, "page_count", |r| r.get(0))?;
                    w.pragma_update(None, "max_page_count", pages + 16)?;
                    Ok(())
                })?;
                import_all(db, &path, &[Member::Plain], &req, &mut |_, _, _| Ok(()))
            })
        })
        .expect("a fallback, not a failure");
    assert!(
        matches!(&out, Ram::Fallback(r) if r.detail.contains("is full")),
        "{out:?}"
    );
    assert_eq!(dump(&c.db), before, "the card is as it was");
    assert_eq!(
        std::fs::read_dir(ram_dir.path()).map_or(0, Iterator::count),
        0
    );
}

#[test]
fn memory_falling_short_during_a_load_in_ram_falls_back_with_the_card_untouched() {
    let c = conn();
    let names: Vec<String> = (0..CANCEL_EVERY * 2)
        .map(|i| format!("Game {i} (USA)"))
        .collect();
    let games: Vec<(&str, Option<&str>)> = names.iter().map(|n| (n.as_str(), None)).collect();
    let path = c.dir.path().join("big.dat");
    std::fs::write(&path, dat("Maker - Game Boy", "1", &games)).expect("write");
    let before = dump(&c.db);
    let ram_dir = crate::db::testutil::ram_dir();
    let plan = ram::Plan {
        dir: ram_dir.path().to_path_buf(),
        floor: 0,
        job: 1,
        input: 0,
    };
    // The copy was allowed; from the first member on, no memory is ever enough.
    let req = Request {
        floor: Some(u64::MAX),
        ..request(false, None)
    };
    let out =
        c.db.hold_writer_blocking(|h| {
            ram::run(h, &plan, &mut (), |db| {
                import_all(db, &path, &[Member::Plain], &req, &mut |_, _, _| Ok(()))
            })
        })
        .expect("a fallback, not a failure");
    assert!(
        matches!(&out, Ram::Fallback(r) if r.summary == ram::why::RAN_SHORT && r.detail.contains("fell to")),
        "{out:?}"
    );
    assert_eq!(dump(&c.db), before, "the card is as it was");
}

#[test]
fn the_ram_budget_counts_every_member_uncompressed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let plain = dir.path().join("a.dat");
    std::fs::write(&plain, "x".repeat(1000)).expect("write");
    assert_eq!(members_size(&plain, &[Member::Plain]), 1000);
    let zipped = dir.path().join("b.zip");
    let (one, two) = ("y".repeat(3000), "z".repeat(500));
    std::fs::write(
        &zipped,
        zip_of(&[("1.dat", one.as_str()), ("2.xml", two.as_str())]),
    )
    .expect("write");
    let members = list_members(&zipped).expect("members");
    assert_eq!(members_size(&zipped, &members), 3500);
    assert_eq!(members_size(&dir.path().join("gone.zip"), &members), 0);
}

#[tokio::test]
async fn a_running_core_halves_the_pace_of_the_copy() {
    let (_dir, app) = state();
    let (tx, gate) = watch::channel(GateState::default());
    let mut watch = RamWatch {
        reporter: Reporter::new(Arc::clone(&app), JobId(1), KIND, None),
        id: JobId(1),
        file: "a.dat".into(),
        members: 1,
        stop: watch::channel(false).1,
        gate,
        chunk_started: Instant::now(),
    };
    let chunk = Duration::from_millis(60);
    watch.chunk_started = Instant::now().checked_sub(chunk).expect("past");
    let started = Instant::now();
    ram::Watch::between(&mut watch, ram::Phase::Writing).expect("idle");
    assert!(started.elapsed() < chunk, "no rest without a core");
    tx.send(GateState {
        corename: Some("NES".into()),
        manual: None,
    })
    .expect("core");
    watch.chunk_started = Instant::now().checked_sub(chunk).expect("past");
    let started = Instant::now();
    ram::Watch::between(&mut watch, ram::Phase::Copying).expect("core");
    assert!(
        started.elapsed() >= chunk,
        "rests as long as the chunk took"
    );
}

#[test]
fn memory_under_the_floor_stops_a_load_in_ram() {
    let req = Request {
        floor: Some(u64::MAX),
        ..request(false, None)
    };
    assert!(check(&req).is_ok(), "a pause or a stop only");
    assert!(matches!(room(&req), Err(Error::NoRoom(_))));
    assert!(matches!(pace(&req, CANCEL_EVERY), Err(Error::NoRoom(_))));
    assert!(
        pace(&req, CANCEL_EVERY + 1).is_ok(),
        "checked every few games"
    );
}

#[test]
fn a_pause_stops_an_import_that_holds_the_writer() {
    let (tx, gate) = watch::channel(GateState::default());
    let req = Request {
        gate,
        ..request(false, None)
    };
    assert!(check(&req).is_ok());
    let held = GateState {
        corename: None,
        manual: Some(crate::jobs::gate::Override::Paused),
    };
    tx.send(held).expect("send");
    assert!(matches!(check(&req), Err(Error::Paused)));
    let stopped = request(true, None);
    assert!(matches!(check(&stopped), Err(Error::Cancelled)));
}
