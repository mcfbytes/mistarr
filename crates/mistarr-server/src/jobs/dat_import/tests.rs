use std::fmt::Write as _;
use std::io::Cursor;

use std::collections::HashMap;

use mistarr_core::dat::export_parents;
use mistarr_core::{Crc32, Md5, Sha1};
use rusqlite::Connection;

use super::recompute::*;
use super::stream::*;
use super::*;
use crate::app::testutil::{state, zip_bytes};
use crate::db::dats::NewVersion;
use crate::db::files::{FileState, NewFile};
use crate::db::fixtures::pid;
use crate::db::jobs::{self as rows, JobState};
use crate::db::titles::RomStatus;
use crate::db::{files, titles};
use crate::events::EventKind;
use crate::jobs::watch::gate::GateState;

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
        stop: StopToken::open(stop, Lane::Background),
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
    assert_eq!(l.platform, Some(PlatformId::new("gb")));
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
        platform: PlatformId::new("nes"),
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
    assert_eq!(bound.platform, Some(PlatformId::new("nes")));
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

#[test]
fn members_are_listed_by_extension() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pack = dir.path().join("p.zip");
    std::fs::write(
        &pack,
        zip_bytes(&[("a.dat", b""), ("b.XML", b""), ("c.txt", b"")]),
    )
    .expect("write");
    assert_eq!(list_members(&pack).expect("members").len(), 2);
    std::fs::write(&pack, zip_bytes(&[("c.txt", b"")])).expect("write");
    assert!(list_members(&pack).is_err());
    std::fs::write(&pack, b"not a zip").expect("write");
    assert!(list_members(&pack).expect_err("bad").contains("zip"));
    assert!(list_members(&dir.path().join("x.txt")).is_err());
    assert!(matches!(
        list_members(&dir.path().join("x.Dat")).as_deref(),
        Ok([Member::Plain])
    ));
}

#[tokio::test]
async fn empty_page_and_damaged_files_get_clear_reasons() {
    use crate::incoming::reject::{DAMAGED_ZIP_REASON, EMPTY_REASON, WEB_PAGE_REASON};
    let (_dir, app) = state();
    let dats = app.config().paths.dats();
    std::fs::create_dir_all(&dats).expect("mkdir");
    let good = dat("Maker - Game Boy", "1", &[("Example Quest (USA)", None)]);
    let zip = zip_bytes(&[("gb.dat", good.as_bytes())]);
    let cut = zip.get(..40).expect("zip is longer than 40 bytes").to_vec();
    let cases: [(&str, Vec<u8>, &str); 3] = [
        ("empty.dat", Vec::new(), EMPTY_REASON),
        (
            "page.zip",
            b"<!DOCTYPE html><html>".to_vec(),
            WEB_PAGE_REASON,
        ),
        ("cut.zip", cut, DAMAGED_ZIP_REASON),
    ];
    for (name, body, reason) in cases {
        let path = dats.join(name);
        std::fs::write(&path, &body).expect("write");
        Scheduler::run_inline(&app, Arc::new(DatImport::new(&path)))
            .await
            .expect("run");
        assert!(!path.exists(), "{name}");
        let text =
            std::fs::read_to_string(dats.join("rejected").join(format!("{name}.reason.txt")))
                .expect("reason");
        assert_eq!(text.trim(), reason, "{name}");
    }
}

#[tokio::test]
async fn other_extensions_keep_the_extension_reason() {
    let (_dir, app) = state();
    let dats = app.config().paths.dats();
    std::fs::create_dir_all(&dats).expect("mkdir");
    let cases: [(&str, &[u8]); 2] = [("empty.txt", b""), ("page.html", b"<!DOCTYPE html><html>")];
    for (name, body) in cases {
        let path = dats.join(name);
        std::fs::write(&path, body).expect("write");
        Scheduler::run_inline(&app, Arc::new(DatImport::new(&path)))
            .await
            .expect("run");
        assert!(!path.exists(), "{name}");
        let text =
            std::fs::read_to_string(dats.join("rejected").join(format!("{name}.reason.txt")))
                .expect("reason");
        assert!(text.contains("not a DAT"), "{name}: {text}");
    }
}

#[tokio::test]
async fn the_job_moves_files_and_publishes_events() {
    let (_dir, app) = state();
    let dats_dir = app.config().paths.dats();
    std::fs::create_dir_all(&dats_dir).expect("mkdir");
    let mut events = app.events.subscribe(None).live;
    let pack = dats_dir.join("pack.zip");
    let gb = dat("Maker - Game Boy", "1", &[("Example Quest (USA)", None)]);
    std::fs::write(
        &pack,
        zip_bytes(&[("gb.dat", gb.as_bytes()), ("bad.dat", b"<x/>")]),
    )
    .expect("write");
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

    let id = Scheduler::run_inline(&app, Arc::new(Recompute::new(&pid("gb"))))
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
        .read(|c| crate::db::jobs::count_kind(c, crate::jobs::JobKind::RemapSources))
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
    std::fs::write(
        &pack,
        zip_bytes(&[(&member, db_export("20260101-000000").as_bytes())]),
    )
    .expect("write");
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
                "SELECT t.name, CASE WHEN r.own_name THEN r.name ELSE t.name || r.name END, r.size, lower(hex(r.sha1)),
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
    assert_eq!(l.platform.as_ref().map(PlatformId::as_str), Some("gb"));
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
        .read(|c| rows::count_kind(c, JobKind::Recompute))
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
        platform: PlatformId::new("nes"),
        dat_name: "Test Console".into(),
        dat_version: "2".into(),
    };
    assert!(loaded(import(&c, &v2, &request(false, Some(bind_newer)))).has_titles);
    let bind = Bind {
        version: old.version,
        platform: PlatformId::new("nes"),
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
        platform: PlatformId::new("nes"),
        dat_name: "Test Console".into(),
        dat_version: "1".into(),
    };
    assert!(loaded(import(&c, &v1, &request(false, Some(bind)))).has_titles);
}

async fn job_state(app: &AppState, id: crate::db::ids::JobId) -> rows::JobRow {
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
    let page = crate::db::sql::Page {
        limit: 10,
        offset: 0,
    };
    let rows = app
        .db
        .read(move |c| dats::list(c, page))
        .await
        .expect("list")
        .items;
    let newest = rows.iter().find(|r| r.version == "2").expect("v2").clone();
    assert_eq!(
        (newest.dat_name.as_str(), newest.source_file.as_str()),
        ("odd", "odd (1).dat")
    );

    let id = Scheduler::run_inline(
        &app,
        Arc::new(DatImport::bind(&newest, &pid("nes"), &loaded_dir)),
    )
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
    assert_eq!(bound.platform_id, Some(PlatformId::new("nes")));

    let mut missing = rows.iter().find(|r| r.version == "1").expect("v1").clone();
    missing.source_file = "gone.dat".into();
    let id = Scheduler::run_inline(
        &app,
        Arc::new(DatImport::bind(&missing, &pid("nes"), &loaded_dir)),
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
    assert_eq!(
        logiqx
            .platform
            .as_ref()
            .map(mistarr_core::PlatformId::as_str),
        Some("nes")
    );
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
fn remove_with_files(c: &TestDb, version: DatVersionId, files: &[(&str, u32, i64)]) -> usize {
    c.with(|x| {
        let nes = PlatformId::new("nes");
        let (md5, sha1) = (Md5::from_bytes([0xdd; 16]), Sha1::from_bytes([0xdd; 20]));
        for (path, crc, id) in files {
            let state = crate::db::files::FileState::Misnamed;
            let rom = Some(mistarr_core::RomId::new(*id));
            crate::db::files::upsert(
                x,
                &nes,
                &NewFile {
                    rom_id: rom,
                    crc32: Some(Crc32::from_u32(*crc)),
                    md5: Some(md5),
                    sha1: Some(sha1),
                    header_rule: Some("none".to_string()),
                    ..NewFile::unhashed(path, 4, 1, state)
                },
                1,
            )?;
        }
        dats::retire(x, version, 1)?;
        let matched = rematch_chunk(x, &nes)?;
        titles::recompute::recompute_platform(x, &pid("nes"), &Prefs::default())?;
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
    let files = [("a.bin", 0, shared), ("b.bin", 2, own)];
    assert_eq!(remove_with_files(&c, added.version, &files), 2);
    assert_eq!(
        count(
            &c,
            "SELECT COUNT(*) FROM files WHERE rom_id IS NULL AND state = 3"
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
    let bytes = mistarr_core::hex::decode_spaced(&stored).expect("hex");
    assert_eq!(bytes.len(), 16);
    let entry = mistarr_mister::DatEntry {
        name: "Example Quest (World)".into(),
        roms: vec![mistarr_mister::PlaceRom {
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
    let nes = mistarr_mister::adapter_for(&PlatformId::new("nes")).expect("nes");
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
                ids.push(titles::upsert_title(x, &pid(platform), v.id, &t, &[])?);
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
    let psx = PlatformId::new("psx");
    let h = |n: u8| mistarr_core::Hashes {
        size: 4,
        crc32: format!("{n:08x}").parse().expect("hex"),
        md5: format!("{n:032x}").parse().expect("hex"),
        sha1: format!("{n:040x}").parse().expect("hex"),
    };
    let track = |n: u8| format!("Example Disc (USA) (Track {n}).bin");
    let states = c
        .with(|x| {
            let mut old_disc = crate::db::fixtures::dat(&psx).title("Example Disc (USA)");
            for n in 1..=2 {
                old_disc = old_disc.rom(&track(n), &h(n), crate::db::titles::RomStatus::Good);
            }
            let written = old_disc.write(x)?;
            let (old, old_roms) = (written.titles[0], written.roms);
            let mut new_disc = crate::db::fixtures::dat(&psx).title("Example Disc (USA) (Rev 1)");
            for n in 1..=3 {
                new_disc = new_disc.rom(&track(n), &h(n), crate::db::titles::RomStatus::Good);
            }
            new_disc.write(x)?;
            for n in 1..=2 {
                let rom = old_roms[usize::from(n) - 1];
                let sums = h(n);
                let path = format!("PSX/Example Disc (USA)/{}", track(n));
                files::upsert(
                    x,
                    &psx,
                    &NewFile {
                        rom_id: Some(rom),
                        crc32: Some(sums.crc32),
                        md5: Some(sums.md5),
                        sha1: Some(sums.sha1),
                        header_rule: Some("none".to_string()),
                        ..NewFile::unhashed(&path, 4, 1, FileState::Verified)
                    },
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
                .query_map([], |r| {
                    Ok((r.get::<_, FileState>(0)?.as_str().to_owned(), r.get(1)?))
                })?
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
fn sums(n: u32) -> mistarr_core::Hashes {
    mistarr_core::Hashes {
        size: 4,
        crc32: format!("{n:08x}").parse().expect("hex"),
        md5: format!("{n:032x}").parse().expect("hex"),
        sha1: format!("{n:040x}").parse().expect("hex"),
    }
}

/// Stores an unmatched 4-byte file row with `sums`, as a scan with no DAT leaves it.
fn unmatched_file(
    c: &Connection,
    platform: &PlatformId,
    path: &str,
    sums: &mistarr_core::Hashes,
) -> Result<crate::db::ids::FileId> {
    files::upsert(
        c,
        platform,
        &NewFile {
            crc32: Some(sums.crc32),
            md5: Some(sums.md5),
            sha1: Some(sums.sha1),
            header_rule: Some("none".to_string()),
            ..NewFile::unhashed(path, 4, 1, FileState::Unverified)
        },
        1,
    )
}

#[tokio::test]
async fn recompute_matches_unmatched_files_and_updates_have() {
    let (_dir, app) = state();
    let gb = PlatformId::new("gb");
    app.db
        .write_tx(move |tx| {
            let quest = ("Example Quest (USA)", "Example Quest (USA).gb");
            crate::db::fixtures::dat(&gb)
                .title(quest.0)
                .rom(quest.1, &sums(1), RomStatus::Good)
                .write(tx)?;
            let manor = ("Mock Manor (USA)", "Mock Manor (USA).gb");
            crate::db::fixtures::dat(&gb)
                .title(manor.0)
                .rom(manor.1, &sums(2), RomStatus::Good)
                .write(tx)?;
            titles::recompute::recompute_platform(tx, &pid("gb"), &Prefs::default())?;
            // More strays than one chunk, so the cursor pages past files that never match.
            for n in 0..300 {
                unmatched_file(tx, &gb, &format!("GAMEBOY/stray {n}.gb"), &sums(1000 + n))?;
            }
            unmatched_file(tx, &gb, "GAMEBOY/Example Quest (USA).gb", &sums(1))?;
            unmatched_file(tx, &gb, "GAMEBOY/Other Name.gb", &sums(2))?;
            Ok(())
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

    let run = Scheduler::run_inline(&app, Arc::new(Recompute::new(&pid("gb"))));
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
                .query_map([], |r| {
                    Ok((r.get(0)?, r.get::<_, FileState>(1)?.as_str().to_owned()))
                })?
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
    let nes = PlatformId::new("nes");
    let (first, pages) = c
        .with(|x| {
            let first = unmatched_file(x, &nes, "NES/stray.nes", &sums(7))?;
            let mut after = crate::db::ids::FileId::new(0);
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
    let nes = PlatformId::new("nes");
    let state = c
        .with(|x| {
            let rom = sums(9);
            let title = crate::db::fixtures::dat(&nes)
                .title("Crc Quest (USA)")
                .write(x)?
                .titles[0];
            x.execute(
                "INSERT INTO roms (title_id, name, size, crc32)
                 VALUES (?1, 'Crc Quest (USA).nes', 4, ?2)",
                rusqlite::params![title, rom.crc32],
            )?;
            let path = "NES/Crc Quest (USA).nes";
            let unverified = FileState::Unverified;
            let id = files::upsert(
                x,
                &nes,
                &NewFile {
                    crc32: Some(rom.crc32),
                    md5: Some(rom.md5),
                    sha1: Some(rom.sha1),
                    header_rule: Some("ines".to_string()),
                    ..NewFile::unhashed(path, 20, 1, unverified)
                },
                1,
            )?;
            match_unmatched_chunk(x, &nes, crate::db::ids::FileId::new(0))?;
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
fn stored_whole_and_content_forms_match_headered_and_headerless_roms() {
    let c = conn();
    let lynx = PlatformId::new("lynx");
    let states = c
        .with(|x| {
            let title = crate::db::fixtures::dat(&lynx)
                .title("Form Quest (USA)")
                .write(x)?
                .titles[0];
            // Rom 30 is a headered DAT's whole file, rom 31 a headerless DAT's content.
            let (whole_rom, content_rom, plain) = (sums(30), sums(31), sums(32));
            for (name, h, size) in [("a.lnx", &whole_rom, 68), ("b.lnx", &content_rom, 4)] {
                x.execute(
                    "INSERT INTO roms (title_id, name, size, crc32, md5, sha1)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    rusqlite::params![title, name, size, h.crc32, h.md5, h.sha1],
                )?;
            }
            x.execute(
                "INSERT INTO roms (title_id, name, size, crc32) VALUES (?1, 'c.lnx', 4, ?2)",
                rusqlite::params![title, plain.crc32],
            )?;
            let whole_of = |h: &mistarr_core::Hashes| files::WholeHashes {
                crc32: Some(h.crc32),
                md5: Some(h.md5),
                sha1: Some(h.sha1),
            };
            let unverified = FileState::Unverified;
            let mut ids = Vec::new();
            // a and b hold a 64-byte header; c has none, so its one form is 68 bytes whole.
            let (other, no_header) = (sums(33), whole_of(&plain));
            let rows = [
                ("AtariLynx/a.lnx", &other, whole_of(&whole_rom)),
                ("AtariLynx/b.lnx", &content_rom, whole_of(&other)),
                ("AtariLynx/c.lnx", &plain, no_header),
            ];
            for (path, content, whole) in &rows {
                ids.push(files::upsert(
                    x,
                    &lynx,
                    &NewFile {
                        crc32: Some(content.crc32),
                        md5: Some(content.md5),
                        sha1: Some(content.sha1),
                        header_rule: Some("lnx".to_string()),
                        whole: whole.clone(),
                        ..NewFile::unhashed(path, 68, 1, unverified)
                    },
                    1,
                )?);
            }
            match_unmatched_chunk(x, &lynx, crate::db::ids::FileId::new(0))?;
            let mut states = Vec::new();
            for id in ids {
                states.push(files::get(x, id)?.map(|f| f.state));
            }
            Ok(states)
        })
        .expect("match");
    assert_eq!(
        states,
        [
            Some(FileState::Verified),
            Some(FileState::Verified),
            Some(FileState::Unverified)
        ],
        "the whole form at its size, the content at the size less the header, and no header \
         means no stripped size"
    );
}

#[test]
fn a_stored_crc_allows_for_a_copier_header_only_at_its_size() {
    let c = conn();
    let snes = PlatformId::new("snes");
    let states = c
        .with(|x| {
            let rom = sums(11);
            let title = crate::db::fixtures::dat(&snes)
                .title("Copier Quest (USA)")
                .write(x)?
                .titles[0];
            x.execute(
                "INSERT INTO roms (title_id, name, size, crc32)
                 VALUES (?1, 'Copier Quest (USA).sfc', 1024, ?2)",
                rusqlite::params![title, rom.crc32],
            )?;
            let unverified = FileState::Unverified;
            let mut ids = Vec::new();
            // 1536 is 1024 plus a 512-byte copier header; 1040 is no copier size.
            for (path, size) in [("SNES/Copier Quest (USA).sfc", 1536), ("SNES/b.sfc", 1040)] {
                ids.push(files::upsert(
                    x,
                    &snes,
                    &NewFile {
                        crc32: Some(rom.crc32),
                        md5: Some(rom.md5),
                        sha1: Some(rom.sha1),
                        header_rule: Some("smc".to_string()),
                        ..NewFile::unhashed(path, size, 1, unverified)
                    },
                    1,
                )?);
            }
            match_unmatched_chunk(x, &snes, crate::db::ids::FileId::new(0))?;
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
    let nes = PlatformId::new("nes");
    let changes = c
        .with(|x| {
            unmatched_file(x, &nes, "NES/stray.nes", &sums(12))?;
            let before = x.total_changes();
            match_unmatched_chunk(x, &nes, crate::db::ids::FileId::new(0))?;
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
    let gb = PlatformId::new("gb");
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
                job: Some(crate::db::ids::JobId::new(1)),
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

fn job_row(app: &AppState, id: crate::db::ids::JobId) -> rows::JobRow {
    app.db
        .read_blocking(|c| rows::get(c, id))
        .expect("get")
        .expect("row")
}

fn count_kind(app: &AppState, kind: JobKind) -> u64 {
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
        count_kind(&app, JobKind::Recompute),
        0,
        "the recompute ran in RAM"
    );
    assert_eq!(count_kind(&app, JobKind::RemapSources), 1);
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
        count_kind(&app, JobKind::Recompute),
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
        job: Some(crate::db::ids::JobId::new(1)),
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
        job: Some(crate::db::ids::JobId::new(1)),
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
        zip_bytes(&[("1.dat", one.as_bytes()), ("2.xml", two.as_bytes())]),
    )
    .expect("write");
    let members = list_members(&zipped).expect("members");
    assert_eq!(members_size(&zipped, &members), 3500);
    assert_eq!(members_size(&dir.path().join("gone.zip"), &members), 0);
}

#[tokio::test]
async fn a_running_core_halves_the_pace_of_the_copy() {
    let (_dir, app) = state();
    let (tx, gate) = tokio::sync::watch::channel(GateState::default());
    let mut watch = RamWatch {
        reporter: Reporter::new(Arc::clone(&app), JobId::new(1), JobKind::DatImport, None),
        id: JobId::new(1),
        file: "a.dat".into(),
        members: 1,
        stop: StopToken::fixed(false, gate, Lane::Background),
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
    assert!(req.stop.check().is_ok(), "a pause or a stop only");
    assert!(matches!(room(&req), Err(Error::NoRoom(_))));
    assert!(matches!(pace(&req, CANCEL_EVERY), Err(Error::NoRoom(_))));
    assert!(
        pace(&req, CANCEL_EVERY + 1).is_ok(),
        "checked every few games"
    );
}

#[test]
fn a_pause_stops_an_import_that_holds_the_writer() {
    let (tx, gate) = tokio::sync::watch::channel(GateState::default());
    let req = Request {
        stop: StopToken::fixed(false, gate, Lane::Background),
        ..request(false, None)
    };
    assert!(req.stop.check().is_ok());
    let held = GateState {
        corename: None,
        manual: Some(crate::jobs::watch::gate::Override::Paused),
    };
    tx.send(held).expect("send");
    assert!(matches!(req.stop.check(), Err(Error::Paused)));
    let stopped = request(true, None);
    assert!(matches!(stopped.stop.check(), Err(Error::Cancelled)));
}

/// mistarr's rewrite of `xml`, as a fetch places it.
fn rewritten(xml: &str) -> String {
    let mut out = Vec::new();
    mistarr_core::dat::rewrite(xml.as_bytes(), &mut out).expect("rewrite");
    String::from_utf8(out).expect("utf-8")
}

#[test]
fn a_rewritten_dat_imports_to_the_same_rows() {
    let logiqx = "<?xml version=\"1.0\"?><!-- note --><datafile>\
        <header><name>Maker - Nintendo Entertainment System</name><version>3</version>\
        <clrmamepro header=\"No-Intro_NES.xml\"/></header>\
        <game name=\"Example Quest (USA)\"><description>Example Quest</description>\
        <release name=\"x\" region=\"USA\" language=\"En\"/><extra>ignored</extra>\
        <rom name=\"a.nes\" size=\"4\" crc=\"0A0B0C0D\" header=\"4E 45 53 1A\" status=\"verified\"/></game>\
        <game name=\"Example Quest (Europe) (Proto)\" cloneof=\"Example Quest (USA)\">\
        <rom name=\"b.nes\" size=\"4\" sha1=\"0123456789abcdef0123456789abcdef01234567\"/></game>\
        </datafile>";
    let (original, copy) = (conn(), conn());
    loaded(import(&original, logiqx, &request(false, None)));
    loaded(import(&copy, &rewritten(logiqx), &request(false, None)));
    assert_eq!(dump(&copy.db), dump(&original.db));

    let export = db_export("20260101-000000");
    let stem = "Example Vendor - Nintendo Entertainment System (DB Export) (20260101-000000)";
    let (original, copy) = (conn(), conn());
    import_export(&original, &export, stem, 1);
    import_export(&copy, &rewritten(&export), stem, 1);
    assert_eq!(dump(&copy.db), dump(&original.db));
    assert!(count(&copy, "SELECT COUNT(*) FROM roms") > 0);
}

#[test]
fn a_placed_file_stays_verified_when_its_rom_is_matched_again() {
    let c = conn();
    let snes = PlatformId::new("snes");
    let game = "Mock Manor (Europe) (Rev 1)";
    let row = mistarr_mister::platforms::by_id("snes").expect("snes");
    // The name the cartridge adapter places the file under.
    let placed = format!(
        "{}.{}",
        mistarr_mister::adapter::safe_name(game).expect("name"),
        row.extension_written.expect("ext")
    );
    let sums = mistarr_core::Hashes {
        size: 4,
        crc32: "0a0b0c0d".parse().expect("hex"),
        md5: "e".repeat(32).parse().expect("hex"),
        sha1: "e".repeat(40).parse().expect("hex"),
    };
    let (file, live) = c
        .with(|x| {
            let old = crate::db::fixtures::dat(&snes)
                .title(game)
                .rom("mm.smc", &sums, RomStatus::Good)
                .write(x)?
                .first_rom();
            let rel = format!("SNES/{placed}");
            let file = files::upsert(
                x,
                &snes,
                &NewFile {
                    rom_id: Some(old),
                    crc32: Some(sums.crc32),
                    md5: Some(sums.md5),
                    sha1: Some(sums.sha1),
                    header_rule: Some("smc".to_string()),
                    ..NewFile::unhashed(&rel, 4, 1, FileState::Verified)
                },
                1,
            )?;
            x.execute("UPDATE roms SET retired = 1 WHERE id = ?1", [old])?;
            let live = crate::db::fixtures::dat(&snes)
                .title(game)
                .rom("Mock Manor.smc", &sums, RomStatus::Good)
                .write(x)?
                .first_rom();
            assert_eq!(rematch_chunk(x, &snes)?, 1);
            Ok((file, live))
        })
        .expect("rematch");
    let got = c.with(|x| files::get(x, file)).expect("get").expect("row");
    assert_eq!(
        (got.rom_id, got.state),
        (Some(live), FileState::Verified),
        "the placed name fits, so rename has nothing to do"
    );
}

/// Counts the write-backs of the runs it watches.
#[derive(Default)]
struct WriteBacks(usize);

impl ram::Watch for WriteBacks {
    fn phase(&mut self, phase: ram::Phase) {
        if phase == ram::Phase::Writing {
            self.0 += 1;
        }
    }

    fn between(&mut self, _: ram::Phase) -> Result<()> {
        Ok(())
    }
}

/// File number `i` of a batch holding `xml`, as the work on the copy sees it.
fn entry(dir: &std::path::Path, i: usize, xml: &str) -> batch::Entry {
    let path = dir.join(format!("{i}.dat"));
    std::fs::write(&path, xml).expect("write");
    let mut req = request(false, None);
    req.now = i64::try_from(i).expect("small") + 1;
    batch::Entry {
        id: JobId::new(req.now),
        file: format!("{i}.dat"),
        path,
        members: vec![Member::Plain],
        req,
    }
}

/// Runs `entries` as one batch on a copy in RAM that is swapped in, counting write-backs.
fn run_batch(c: &TestDb, entries: &[batch::Entry]) -> (Vec<batch::End>, usize, ram::Report) {
    let ram_dir = crate::db::testutil::ram_dir();
    let plan = ram::Plan {
        dir: ram_dir.path().to_path_buf(),
        floor: 0,
        job: Some(JobId::new(1)),
        input: 0,
    };
    let mut backs = WriteBacks::default();
    let ran =
        c.db.hold_writer_blocking(|h| {
            ram::run(h, &plan, &mut backs, |db| {
                batch::import_batch(db, entries, &mut |_, _, _, _| Ok(()))
            })
        })
        .expect("run");
    match ran {
        Ram::Done(ends, report) => (ends, backs.0, report),
        Ram::Fallback(reason) => panic!("fell back: {reason:?}"),
    }
}

/// Records every phase of the runs it watches.
#[derive(Default)]
struct Phases(Vec<ram::Phase>);

impl ram::Watch for Phases {
    fn phase(&mut self, phase: ram::Phase) {
        self.0.push(phase);
    }

    fn between(&mut self, _: ram::Phase) -> Result<()> {
        Ok(())
    }
}

#[test]
fn a_batch_with_many_free_pages_is_compacted_before_it_is_written_back() {
    let c = conn();
    c.with(|x| {
        x.execute_batch("CREATE TABLE spare (id INTEGER PRIMARY KEY, v BLOB)")?;
        let mut add = x.prepare("INSERT INTO spare (v) VALUES (zeroblob(65536))")?;
        for _ in 0..96 {
            add.execute([])?;
        }
        drop(add);
        x.execute_batch("DROP TABLE spare")?;
        x.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
        Ok(())
    })
    .expect("free pages");
    let size = std::fs::metadata(c.db.path()).expect("stat").len();
    let entries = [
        entry(
            c.dir.path(),
            0,
            &dat("Maker - Game Boy", "1", &[("Example Quest (USA)", None)]),
        ),
        entry(
            c.dir.path(),
            1,
            &dat("Maker - Game Boy", "2", &[("Mock Manor (USA)", None)]),
        ),
    ];
    let ram_dir = crate::db::testutil::ram_dir();
    let plan = ram::Plan {
        dir: ram_dir.path().to_path_buf(),
        floor: 0,
        job: Some(JobId::new(1)),
        input: 0,
    };
    let mut phases = Phases::default();
    let ran =
        c.db.hold_writer_blocking(|h| {
            ram::run(h, &plan, &mut phases, |db| {
                batch::import_batch(db, &entries, &mut |_, _, _, _| Ok(()))
            })
        })
        .expect("run");
    let Ram::Done(ends, _) = ran else {
        panic!("fell back: {ran:?}");
    };
    assert!(ends.iter().all(loads));
    assert!(phases.0.contains(&ram::Phase::Compacting), "{:?}", phases.0);
    let after = std::fs::metadata(c.db.path()).expect("stat").len();
    assert!(after < size, "{after} bytes after {size}");
}

fn loads(end: &batch::End) -> bool {
    matches!(end, batch::End::Imported(o) if matches!(o[..], [Outcome::Loaded(_)]))
}

#[test]
fn a_batch_stores_the_rows_of_the_same_dats_imported_one_at_a_time() {
    let (gb, nes) = ("Maker - Game Boy", "Maker - Nintendo Entertainment System");
    let quest = [("Example Quest (USA)", None), ("Mock Manor (USA)", None)];
    let cases: Vec<(&str, Vec<String>)> = vec![
        (
            "platforms and an unbound DAT",
            vec![
                dat(gb, "1", &quest),
                dat(nes, "1", &[("Sample Tale (World)", None)]),
                dat("Test Console", "1", &[("Example Quest (USA)", None)]),
            ],
        ),
        (
            "a newer version in the same batch",
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
                dat(nes, "1", &quest),
            ],
        ),
        (
            "clones across a batch",
            vec![
                dat(
                    gb,
                    "1",
                    &[
                        ("Example Quest (USA)", None),
                        ("Example Quest (Japan)", Some("Example Quest (USA)")),
                    ],
                ),
                dat(nes, "1", &[("Mock Manor (Europe)", None)]),
            ],
        ),
        (
            "a rejected DAT between two others",
            vec![
                dat(gb, "1", &quest),
                "<datafile><game".to_owned(),
                dat(nes, "1", &quest),
            ],
        ),
    ];
    for (name, dats) in cases {
        let (one_by_one, batched) = (with_files(), with_files());
        let a = load_all(&one_by_one, &dats, false);
        let entries: Vec<_> = dats
            .iter()
            .enumerate()
            .map(|(i, xml)| entry(batched.dir.path(), i, xml))
            .collect();
        let (ends, backs, _) = run_batch(&batched, &entries);
        let b: Vec<Outcome> = ends
            .into_iter()
            .flat_map(|e| match e {
                batch::End::Imported(o) => o,
                batch::End::Failed(e) | batch::End::Dirty(e) => panic!("{name}: {e}"),
            })
            .collect();
        assert_eq!(a, b, "{name}: outcomes");
        assert_eq!(backs, 1, "{name}: one write-back");
        assert_eq!(dump(&one_by_one.db), dump(&batched.db), "{name}: rows");
    }
}

#[test]
fn ten_small_dats_cost_one_write_back() {
    let names = [
        "Maker - Game Boy",
        "Maker - Nintendo Entertainment System",
        "Test Console",
    ];
    let dats: Vec<String> = (0..10)
        .map(|i| {
            let game = format!("Example Quest {i} (USA)");
            dat(names[i % 3], &i.to_string(), &[(game.as_str(), None)])
        })
        .collect();
    let batched = with_files();
    let entries: Vec<_> = dats
        .iter()
        .enumerate()
        .map(|(i, xml)| entry(batched.dir.path(), i, xml))
        .collect();
    let (ends, backs, report) = run_batch(&batched, &entries);
    assert!(ends.iter().all(loads));
    assert_eq!(backs, 1);
    let alone = with_files();
    let (mut backs_alone, mut writes_alone) = (0, Some(0));
    for (i, xml) in dats.iter().enumerate() {
        let one = [entry(alone.dir.path(), i, xml)];
        let (_, n, r) = run_batch(&alone, &one);
        backs_alone += n;
        writes_alone = writes_alone.zip(r.card_writes).map(|(a, b)| a + b);
    }
    assert_eq!(backs_alone, 10);
    assert_eq!(dump(&batched.db), dump(&alone.db), "the same rows");
    if let (Some(batch), Some(each)) = (report.card_writes, writes_alone) {
        assert!(batch < each, "{batch} card writes against {each}");
    }
}

#[test]
fn one_bad_dat_in_a_batch_is_rejected_alone() {
    let (gb, nes) = ("Maker - Game Boy", "Maker - Nintendo Entertainment System");
    let quest = [("Example Quest (USA)", None)];
    let c = with_files();
    let dir = c.dir.path();
    let mut entries = vec![
        entry(dir, 0, &dat(gb, "1", &quest)),
        entry(dir, 1, "<datafile><game"),
        entry(dir, 2, &dat(nes, "1", &quest)),
        entry(dir, 3, &dat(nes, "2", &quest)),
        entry(dir, 4, &dat(gb, "2", &quest)),
    ];
    // File 2 vanishes before its turn; file 3 is cancelled by the user.
    std::fs::remove_file(&entries[2].path).expect("remove");
    let cancel = Arc::new(crate::jobs::stop::Cancel::default());
    cancel.cancel();
    let (_down, shutdown) = tokio::sync::watch::channel(false);
    let (_gate, gate) = tokio::sync::watch::channel(GateState::default());
    entries[3].req.stop = StopToken::new(shutdown.clone(), gate, Lane::Background, cancel);
    let (ends, backs, _) = run_batch(&c, &entries);
    assert_eq!(backs, 1);
    assert!(loads(&ends[0]) && loads(&ends[4]), "{ends:?}");
    assert!(
        matches!(&ends[1], batch::End::Imported(o) if matches!(o[..], [Outcome::Rejected(_)])),
        "{ends:?}"
    );
    assert!(
        matches!(&ends[2], batch::End::Failed(Error::File { .. })),
        "{ends:?}"
    );
    assert!(
        matches!(&ends[3], batch::End::Failed(Error::CancelledByUser)),
        "{ends:?}"
    );
    let alone = with_files();
    let kept = [
        entry(alone.dir.path(), 0, &dat(gb, "1", &quest)),
        entry(alone.dir.path(), 4, &dat(gb, "2", &quest)),
    ];
    run_batch(&alone, &kept);
    assert_eq!(
        dump(&c.db),
        dump(&alone.db),
        "only the two good DATs are in"
    );
}

#[test]
fn a_shutdown_in_a_batch_ends_it_with_the_card_untouched() {
    let c = with_files();
    let quest = [("Example Quest (USA)", None)];
    let mut entries = vec![
        entry(c.dir.path(), 0, &dat("Maker - Game Boy", "1", &quest)),
        entry(c.dir.path(), 1, &dat("Maker - Game Boy", "2", &quest)),
    ];
    entries[1].req.stop = StopToken::open(true, Lane::Background);
    let before = dump(&c.db);
    let ram_dir = crate::db::testutil::ram_dir();
    let plan = ram::Plan {
        dir: ram_dir.path().to_path_buf(),
        floor: 0,
        job: Some(JobId::new(1)),
        input: 0,
    };
    let ran = c.db.hold_writer_blocking(|h| {
        ram::run(h, &plan, &mut (), |db| {
            batch::import_batch(db, &entries, &mut |_, _, _, _| Ok(()))
        })
    });
    assert!(matches!(ran, Err(Error::Cancelled)), "{ran:?}");
    assert_eq!(dump(&c.db), before, "the first DAT never reached the card");
    assert_eq!(
        std::fs::read_dir(ram_dir.path()).map_or(0, Iterator::count),
        0
    );
}

/// A DAT file at `path` that claims to be `bytes` long, mostly holes.
fn huge_dat(path: &std::path::Path, bytes: u64) {
    let file = File::create(path).expect("create");
    std::io::Write::write_all(
        &mut &file,
        dat("Maker - Game Boy", "9", &[("Example Quest (USA)", None)]).as_bytes(),
    )
    .expect("head");
    file.set_len(bytes).expect("sparse");
}

async fn queue(app: &Arc<AppState>, path: &std::path::Path) -> JobId {
    Scheduler::enqueue(app, Arc::new(DatImport::new(path)))
        .await
        .expect("enqueue")
}

fn drop_dat(app: &AppState, name: &str, xml: &str) -> PathBuf {
    let dir = app.config().paths.dats();
    std::fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join(name);
    std::fs::write(&path, xml).expect("write");
    path
}

#[tokio::test]
async fn queued_dat_jobs_run_as_one_batch_each_with_its_own_job() {
    let (_dir, app) = state();
    let quest = [("Example Quest (USA)", None)];
    let files = [
        drop_dat(&app, "a.dat", &dat("Maker - Game Boy", "1", &quest)),
        drop_dat(
            &app,
            "b.dat",
            &dat("Maker - Nintendo Entertainment System", "1", &quest),
        ),
        drop_dat(&app, "c.dat", "<datafile><game"),
        drop_dat(&app, "d.dat", &dat("Test Console", "1", &quest)),
    ];
    Scheduler::start(&app);
    app.gate
        .set_override(Some(crate::jobs::watch::gate::Override::Paused));
    let mut events = app.events.subscribe(None).live;
    let mut ids = Vec::new();
    for f in &files {
        ids.push(queue(&app, f).await);
    }
    app.gate.set_override(None);
    crate::testing::eventually("every job to end", || async {
        let ids = ids.clone();
        let states: Vec<JobState> = app
            .db
            .read(move |c| {
                ids.iter()
                    .map(|&i| Ok(rows::get(c, i)?.expect("row").state))
                    .collect()
            })
            .await
            .expect("states");
        states.iter().all(|s| *s == JobState::Done)
    })
    .await;
    let mut copying = std::collections::HashSet::new();
    let (mut last_copy, mut first_done) = (None, None);
    let mut loaded = 0;
    let mut n = 0;
    while let Ok(e) = events.try_recv() {
        n += 1;
        let body: Value = serde_json::from_str(&e.data).unwrap_or(Value::Null);
        match e.kind {
            EventKind::DatLoaded => loaded += 1,
            EventKind::JobProgress if body["kind"] == "dat_import" => {
                if body["progress"]["phase"] == "copying the database to memory" {
                    copying.insert(body["id"].as_i64());
                    last_copy = Some(n);
                }
                if body["state"] == "done" && first_done.is_none() {
                    first_done = Some(n);
                }
            }
            _ => {}
        }
    }
    assert_eq!(copying.len(), 4, "every job shows the copy");
    assert!(
        last_copy < first_done,
        "no job ends before the copy is made: one batch"
    );
    assert_eq!(loaded, 3, "the three that loaded each publish");
    let dats = app.config().paths.dats();
    for name in ["a.dat", "b.dat", "d.dat"] {
        assert!(dats.join("loaded").join(name).is_file(), "{name}");
    }
    assert!(dats.join("rejected").join("c.dat").is_file());
    assert!(dats.join("rejected").join("c.dat.reason.txt").is_file());
    let versions = count_rows(&app, "SELECT COUNT(*) FROM dat_versions");
    assert_eq!(versions, 3);
    assert_eq!(count_kind(&app, JobKind::Recompute), 0);
    assert_eq!(
        count_kind(&app, JobKind::RemapSources),
        2,
        "one per platform"
    );
    let row = job_row(&app, ids[1]);
    assert_eq!(
        row.progress.as_ref().map(|p| p["done"].clone()),
        Some(json!(1))
    );
    // A lane skips a taken job when it reaches it, which may be after the job row ends.
    crate::testing::eventually("every lane to skip the job taken from it", || async {
        app.scheduler
            .taken
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
    })
    .await;
}

fn count_rows(app: &AppState, sql: &'static str) -> i64 {
    app.db
        .read_blocking(|c| Ok(c.query_row(sql, [], |r| r.get(0))?))
        .expect("count")
}

#[tokio::test]
async fn a_need_too_large_for_the_batch_leaves_the_rest_queued() {
    let (_dir, app) = state();
    let quest = [("Example Quest (USA)", None)];
    let lead = drop_dat(&app, "a.dat", &dat("Maker - Game Boy", "1", &quest));
    let cancelled = drop_dat(&app, "b.dat", &dat("Maker - Game Boy", "2", &quest));
    let fits = drop_dat(&app, "c.dat", &dat("Maker - Game Boy", "3", &quest));
    let huge = app.config().paths.dats().join("d.dat");
    huge_dat(&huge, 1 << 38);
    let after = drop_dat(&app, "e.dat", &dat("Maker - Game Boy", "4", &quest));
    let (cancelled_id, fits_id, huge_id, after_id) = (
        queue(&app, &cancelled).await,
        queue(&app, &fits).await,
        queue(&app, &huge).await,
        queue(&app, &after).await,
    );
    assert!(app.scheduler.cancel(cancelled_id));
    let id = Scheduler::run_inline(&app, Arc::new(DatImport::new(&lead)))
        .await
        .expect("run");
    assert_eq!(job_row(&app, id).state, JobState::Done);
    assert_eq!(job_row(&app, fits_id).state, JobState::Done, "it fit");
    for left_out in [cancelled_id, huge_id, after_id] {
        assert_eq!(job_row(&app, left_out).state, JobState::Queued);
    }
    let loaded = app.config().paths.dats().join("loaded");
    assert!(loaded.join("a.dat").is_file() && loaded.join("c.dat").is_file());
    assert!(
        !loaded.join("e.dat").exists(),
        "dropped from the end of the batch"
    );
    assert_eq!(count_rows(&app, "SELECT COUNT(*) FROM dat_versions"), 2);
}

#[tokio::test]
async fn a_batch_the_copy_cannot_take_goes_back_to_the_queue() {
    let (dir, app) = state();
    let plain = dir.path().join("plain");
    std::fs::create_dir_all(&plain).expect("mkdir");
    app.update_config(|c| c.memory.import_dir.clone_from(&plain));
    let quest = [("Example Quest (USA)", None)];
    let lead = drop_dat(&app, "a.dat", &dat("Maker - Game Boy", "1", &quest));
    let other = drop_dat(&app, "b.dat", &dat("Maker - Game Boy", "2", &quest));
    let other_id = queue(&app, &other).await;
    let id = Scheduler::run_inline(&app, Arc::new(DatImport::new(&lead)))
        .await
        .expect("run");
    let row = job_row(&app, id);
    assert_eq!(row.state, JobState::Done, "{:?}", row.progress);
    assert_eq!(
        row.progress.as_ref().map(|p| p["phase"].clone()),
        Some(json!(IN_PLACE))
    );
    assert_eq!(job_row(&app, other_id).state, JobState::Queued);
    assert!(other.is_file(), "its own turn is still to come");
    assert_eq!(count_rows(&app, "SELECT COUNT(*) FROM dat_versions"), 1);
    let taken = app
        .scheduler
        .taken
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .len();
    assert_eq!(taken, 0, "its lane will run it");
}

#[test]
fn halt_ignores_the_cancel_of_one_job_but_not_the_lane_or_the_server() {
    let cancelled = Arc::new(crate::jobs::stop::Cancel::default());
    cancelled.cancel();
    let (_down, shutdown) = tokio::sync::watch::channel(false);
    let (gate_tx, gate) = tokio::sync::watch::channel(GateState::default());
    let req = Request {
        stop: StopToken::new(shutdown, gate, Lane::Background, cancelled),
        ..request(false, None)
    };
    assert!(batch::halt(&req).is_ok());
    gate_tx
        .send(GateState {
            corename: None,
            manual: Some(crate::jobs::watch::gate::Override::Paused),
        })
        .expect("gate");
    assert!(matches!(batch::halt(&req), Err(Error::Paused)));
    let down = Request {
        stop: StopToken::open(true, Lane::Background),
        ..request(false, None)
    };
    assert!(matches!(batch::halt(&down), Err(Error::Cancelled)));
}

#[tokio::test]
async fn a_file_failing_after_a_member_loaded_leaves_nothing_of_it_in_the_copy() {
    let (_dir, app) = state();
    let quest = [("Example Quest (USA)", None)];
    let lead = drop_dat(
        &app,
        "a.dat",
        &dat("Maker - Nintendo Entertainment System", "1", &quest),
    );
    let big: Vec<String> = (0..20_000).map(|i| format!("Game {i} (USA)")).collect();
    let big: Vec<(&str, Option<&str>)> = big.iter().map(|n| (n.as_str(), None)).collect();
    let pack = app.config().paths.dats().join("p.zip");
    let (one, two) = (
        dat("Maker - Game Boy", "7", &quest),
        dat("Maker - Game Boy", "8", &big),
    );
    crate::app::testutil::write_zip(
        &pack,
        &[("1.dat", one.as_bytes()), ("2.dat", two.as_bytes())],
    );
    let last = drop_dat(&app, "z.dat", &dat("Test Console", "1", &quest));
    let (pack_id, last_id) = (queue(&app, &pack).await, queue(&app, &last).await);
    let mut events = app.events.subscribe(None).live;
    let watcher = {
        let app = Arc::clone(&app);
        tokio::spawn(async move {
            while let Ok(e) = events.recv().await {
                let body: Value = serde_json::from_str(&e.data).unwrap_or(Value::Null);
                if body["id"] == pack_id.get() && body["progress"]["done"] == 1 {
                    app.scheduler.cancel(pack_id);
                    return;
                }
            }
        })
    };
    let id = Scheduler::run_inline(&app, Arc::new(DatImport::new(&lead)))
        .await
        .expect("run");
    watcher.abort();
    assert_eq!(job_row(&app, id).state, JobState::Done);
    assert_eq!(job_row(&app, last_id).state, JobState::Done);
    let row = job_row(&app, pack_id);
    assert_eq!(row.state, JobState::Failed, "{:?}", row.progress);
    assert!(pack.is_file(), "the file stays in dats/");
    let names: Vec<String> = app
        .db
        .read_blocking(|c| {
            let mut q = c.prepare("SELECT dat_name FROM dat_versions ORDER BY id")?;
            let rows = q.query_map([], |r| r.get(0))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
        .expect("names");
    assert_eq!(names.len(), 2, "{names:?}: only the two good files");
    assert!(!names.iter().any(|n| n == "Maker - Game Boy"));
}

#[test]
fn a_cancelled_lead_ends_the_batch_after_its_file() {
    let c = with_files();
    let quest = [("Example Quest (USA)", None)];
    let mut entries = vec![
        entry(c.dir.path(), 0, &dat("Maker - Game Boy", "1", &quest)),
        entry(c.dir.path(), 1, &dat("Maker - Game Boy", "2", &quest)),
    ];
    let cancel = Arc::new(crate::jobs::stop::Cancel::default());
    cancel.cancel();
    let (_down, shutdown) = tokio::sync::watch::channel(false);
    let (_gate, gate) = tokio::sync::watch::channel(GateState::default());
    entries[0].req.stop = StopToken::new(shutdown, gate, Lane::Background, cancel);
    let imported = std::cell::Cell::new(0);
    let before = dump(&c.db);
    let ram_dir = crate::db::testutil::ram_dir();
    let plan = ram::Plan {
        dir: ram_dir.path().to_path_buf(),
        floor: 0,
        job: Some(JobId::new(1)),
        input: 0,
    };
    let ran = c.db.hold_writer_blocking(|h| {
        ram::run(h, &plan, &mut (), |db| {
            batch::import_batch(db, &entries, &mut |_, e, _, _| {
                imported.set(imported.get() + usize::from(e.id == JobId::new(2)));
                Ok(())
            })
        })
    });
    assert!(matches!(ran, Err(Error::CancelledByUser)), "{ran:?}");
    assert_eq!(imported.get(), 0, "the follower was not imported");
    assert_eq!(dump(&c.db), before);
}

#[tokio::test]
async fn a_cancelled_follower_is_left_to_its_lane_and_fails_there() {
    let (_dir, app) = state();
    let quest = [("Example Quest (USA)", None)];
    let a = drop_dat(&app, "a.dat", &dat("Maker - Game Boy", "1", &quest));
    let b = drop_dat(&app, "b.dat", &dat("Maker - Game Boy", "2", &quest));
    Scheduler::start(&app);
    app.gate
        .set_override(Some(crate::jobs::watch::gate::Override::Paused));
    let (a_id, b_id) = (queue(&app, &a).await, queue(&app, &b).await);
    assert!(app.scheduler.cancel(b_id));
    app.gate.set_override(None);
    crate::testing::eventually("both jobs to end", || async {
        let states = app
            .db
            .read(move |c| Ok([rows::get(c, a_id)?, rows::get(c, b_id)?]))
            .await
            .expect("rows");
        states[0]
            .as_ref()
            .is_some_and(|r| r.state == JobState::Done)
            && states[1]
                .as_ref()
                .is_some_and(|r| r.state == JobState::Failed)
    })
    .await;
    assert!(b.is_file(), "the cancelled file stays in dats/");
    assert_eq!(count_rows(&app, "SELECT COUNT(*) FROM dat_versions"), 1);
}

#[tokio::test]
async fn a_manual_pause_in_a_batch_waits_and_the_batch_starts_again() {
    let (_dir, app) = state();
    let quest = [("Example Quest (USA)", None)];
    let files = [
        drop_dat(&app, "a.dat", &dat("Maker - Game Boy", "1", &quest)),
        drop_dat(&app, "b.dat", &dat("Maker - Game Boy", "2", &quest)),
    ];
    Scheduler::start(&app);
    app.gate
        .set_override(Some(crate::jobs::watch::gate::Override::Paused));
    let mut events = app.events.subscribe(None).live;
    let (a_id, b_id) = (queue(&app, &files[0]).await, queue(&app, &files[1]).await);
    app.gate.set_override(None);
    while let Ok(e) = events.recv().await {
        let body: Value = serde_json::from_str(&e.data).unwrap_or(Value::Null);
        if body["progress"]["phase"] == "copying the database to memory" {
            app.gate
                .set_override(Some(crate::jobs::watch::gate::Override::Paused));
            break;
        }
    }
    crate::testing::eventually("the follower to show the pause", || async {
        app.db
            .read(move |c| Ok(rows::get(c, b_id)?.map(|r| r.state)))
            .await
            .expect("row")
            == Some(JobState::Paused)
    })
    .await;
    app.gate.set_override(None);
    crate::testing::eventually("both jobs to end", || async {
        let ids = [a_id, b_id];
        let states = app
            .db
            .read(move |c| {
                ids.iter()
                    .map(|&i| Ok(rows::get(c, i)?.map(|r| r.state)))
                    .collect::<Result<Vec<_>>>()
            })
            .await
            .expect("rows");
        states.iter().all(|s| *s == Some(JobState::Done))
    })
    .await;
    assert_eq!(count_rows(&app, "SELECT COUNT(*) FROM dat_versions"), 2);
}
