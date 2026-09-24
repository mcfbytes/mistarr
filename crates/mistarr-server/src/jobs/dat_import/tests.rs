use std::fmt::Write as _;
use std::io::{Cursor, Write as _};

use super::*;
use crate::app::testutil::state;
use crate::db::jobs::{self as rows, JobState};

fn conn() -> Connection {
    let mut c = Connection::open_in_memory().expect("open");
    crate::db::migrate::apply(&mut c).expect("migrate");
    crate::db::platforms::seed(&mut c, &mistarr_mister::platforms::PLATFORMS).expect("seed");
    c
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
        fallback_name: "t".into(),
        bind,
        prefs: Prefs::default(),
        now: 1,
        stop: watch::channel(stop).1,
    }
}

fn import(c: &mut Connection, xml: &str, req: &Request) -> Outcome {
    import_member(c, Cursor::new(xml.as_bytes()), req, "").expect("import")
}

fn loaded(o: Outcome) -> Loaded {
    match o {
        Outcome::Loaded(l) => l,
        other => panic!("expected a load, got {other:?}"),
    }
}

fn count(c: &Connection, sql: &str) -> i64 {
    c.query_row(sql, [], |r| r.get(0)).expect("count")
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
    let mut c = conn();
    let xml = dat(
        "Maker - Game Boy",
        "1",
        &[
            ("Example Quest (Japan)", None),
            ("Example Quest (USA)", None),
        ],
    );
    let l = loaded(import(&mut c, &xml, &request(false, None)));
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
    let mut c = conn();
    let xml = dat("Test Console", "1", &[("Example Quest (USA)", None)]);
    let l = loaded(import(&mut c, &xml, &request(false, None)));
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
        import(&mut c, &other, &request(false, Some(bind.clone()))),
        Outcome::Skipped
    );
    let bound = loaded(import(&mut c, &xml, &request(false, Some(bind))));
    assert_eq!(bound.version, l.version);
    assert_eq!(bound.platform, Some(PlatformId("nes".into())));
    assert_eq!(
        count(&c, "SELECT COUNT(*) FROM titles WHERE platform_id = 'nes'"),
        1
    );
}

#[test]
fn clone_of_groups_and_nameless_headers_fall_back_to_the_file() {
    let mut c = conn();
    let xml = dat(
        "Maker - Game Boy",
        "1",
        &[
            ("Example Quest (Japan)", Some("Example Quest (USA)")),
            ("Example Quest (USA)", None),
        ],
    );
    loaded(import(&mut c, &xml, &request(false, None)));
    assert_eq!(count(&c, "SELECT COUNT(DISTINCT parent_id) FROM titles"), 1);
    assert_eq!(count(&c, "SELECT SUM(inferred) FROM titles"), 0);

    let nameless = "<datafile><game name=\"A\"><rom name=\"a\" size=\"1\"/></game></datafile>";
    let l = loaded(import(&mut c, nameless, &request(false, None)));
    let row = dats::get(&c, l.version).expect("get").expect("row");
    assert_eq!(row.dat_name, "t");
}

#[test]
fn malformed_dats_are_rejected_and_roll_back() {
    let mut c = conn();
    let good = dat("Maker - Game Boy", "1", &[("Example Quest (USA)", None)]);
    let broken = good.replace(
        "</datafile>",
        "<game name=\"Cut\"><rom name=\"x\" size=\"z\"/>",
    );
    let o = import(&mut c, &broken, &request(false, None));
    assert!(
        matches!(&o, Outcome::Rejected(r) if r.contains("size")),
        "{o:?}"
    );
    assert_eq!(count(&c, "SELECT COUNT(*) FROM dat_versions"), 0);
    let o = import_member(
        &mut c,
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
fn a_long_import_stops_on_shutdown() {
    let mut c = conn();
    let names: Vec<String> = (0..=CANCEL_EVERY)
        .map(|i| format!("Game {i} (USA)"))
        .collect();
    let games: Vec<(&str, Option<&str>)> = names.iter().map(|n| (n.as_str(), None)).collect();
    let xml = dat("Maker - Game Boy", "1", &games);
    let r = import_member(
        &mut c,
        Cursor::new(xml.as_bytes()),
        &request(true, None),
        "",
    );
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
    assert_eq!(row.progress, Some(json!({ "groups": 1, "picks": 1 })));
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
