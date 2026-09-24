use super::*;
use crate::db::dats::{self, NewVersion};
use crate::db::titles::{self, Browse, RomInput, TitleInput};
use mistarr_core::PlatformId;

fn conn() -> Connection {
    let mut c = Connection::open_in_memory().expect("open");
    crate::db::migrate::apply(&mut c).expect("migrate");
    crate::db::platforms::seed(&mut c, &mistarr_mister::platforms::PLATFORMS).expect("seed");
    c
}

fn mra(c: &Connection, name: &str, zips: &[(&str, bool)]) -> TitleId {
    mra_run(c, name, zips, 1)
}

fn mra_run(c: &Connection, name: &str, zips: &[(&str, bool)], run: i64) -> TitleId {
    let v = mra_version(c, "arcade", 1).expect("version");
    let key = format!("mra:{}", name.to_lowercase());
    let t = MraTitle {
        name,
        base_name: name,
        group_key: &key,
        regions: &[],
        languages: &[],
        revision: None,
        flags: &[],
        setname: Some("exblast"),
        rbf: Some("excore"),
        mra_path: "Example.mra",
        file_stamp: "10:1",
        run,
    };
    let zips: Vec<MraZip<'_>> = zips
        .iter()
        .map(|&(name, present)| MraZip {
            name,
            zip_dir: "mame",
            md5: Some("0123456789abcdef0123456789abcdef"),
            present,
        })
        .collect();
    upsert_title(c, "arcade", v, &t, &zips).expect("upsert")
}

fn recompute(c: &Connection) {
    titles::recompute_platform(c, "arcade", &mistarr_core::select::Prefs::default())
        .expect("recompute");
}

fn browse(c: &Connection) -> Vec<(String, u64)> {
    crate::db::groups::flush(c).expect("flush");
    titles::browse(c, "arcade", &Browse::default(), 100, 0)
        .expect("browse")
        .0
        .into_iter()
        .map(|g| (g.name, g.have_verified))
        .collect()
}

#[test]
fn have_follows_zip_presence_and_the_md5_check() {
    let c = conn();
    let full = mra(
        &c,
        "Example Blaster",
        &[("exblast.zip", true), ("exparent.zip", true)],
    );
    let part = mra(
        &c,
        "Example Quest",
        &[("exquest.zip", true), ("exparent2.zip", false)],
    );
    recompute(&c);
    assert_eq!(
        browse(&c),
        [
            ("Example Blaster".to_owned(), 1),
            ("Example Quest".to_owned(), 0)
        ]
    );
    let info = info(&c, part).expect("info").expect("mra");
    assert_eq!(info.missing_zips, ["mame/exparent2.zip"]);
    assert_eq!(info.rbf.as_deref(), Some("excore"));

    set_check(&c, full, Some("mismatch"), Some("rom 0"), Some("s")).expect("check");
    assert_eq!(browse(&c)[0].1, 0);
    set_check(&c, full, Some("refused"), None, Some("s")).expect("check");
    assert_eq!(browse(&c)[0].1, 1);
    let stored = stored_mra(&c, "arcade", "Example.mra")
        .expect("stored")
        .expect("title");
    assert_eq!(stored.id, full);
    assert_eq!(stored.check_stamp.as_deref(), Some("s"));
    assert_eq!(stored.file_stamp.as_deref(), Some("10:1"));
    let zips = zip_roms(&c, part).expect("zips");
    let got: Vec<(&str, bool, bool)> = zips
        .iter()
        .map(|z| (z.name.as_str(), z.present, z.has_md5))
        .collect();
    assert_eq!(
        got,
        [("exquest.zip", true, true), ("exparent2.zip", false, true)]
    );
    assert_eq!(zips[0].zip_dir, "mame");
}

#[test]
fn rescans_keep_ids_and_retire_what_is_gone() {
    let c = conn();
    let a = mra(&c, "Example Blaster", &[("exblast.zip", false)]);
    let b = mra(&c, "Example Quest", &[("exquest.zip", false)]);
    let d = mra(&c, "Example Racer", &[("exrace.zip", false)]);
    c.execute("UPDATE titles SET wanted = 1 WHERE id = ?1", [a.0])
        .expect("want");
    let run = next_run(&c, "arcade").expect("run");
    assert_eq!(run, 2);
    assert_eq!(
        mra_run(&c, "Example Blaster", &[("exblast.zip", true)], run),
        a
    );
    touch(&c, d, run).expect("touch");
    assert_eq!(retire_unseen(&c, "arcade", run).expect("retire"), 1);
    let (retired, wanted): (bool, bool) = c
        .query_row(
            "SELECT (SELECT retired FROM titles WHERE id = ?1), (SELECT wanted FROM titles WHERE id = ?2)",
            [b.0, a.0],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("row");
    assert!(retired && wanted);
    assert_eq!(live_count(&c, "arcade").expect("count"), 2);
    assert!(has_titles(&c, "arcade").expect("has"));
    assert_eq!(next_run(&c, "arcade").expect("run"), 3);
}

#[test]
fn dropped_zips_are_retired_and_duplicates_kept_once() {
    let c = conn();
    let a = mra(
        &c,
        "Example Blaster",
        &[("exblast.zip", false), ("exold.zip", false)],
    );
    mra(
        &c,
        "Example Blaster",
        &[("exblast.zip", false), ("EXBLAST.ZIP", true)],
    );
    let info = info(&c, a).expect("info").expect("mra");
    assert_eq!(info.missing_zips, ["mame/exblast.zip"]);
}

/// Stores a DAT game on the arcade platform the way the DAT import does.
fn dat_game(c: &Connection, version: &str, name: &str) -> TitleId {
    let v = dats::upsert_version(
        c,
        &NewVersion {
            dat_name: "MAME",
            version,
            source_file: "mame.dat",
            platform: Some("arcade"),
            now: version.parse().unwrap_or(0),
        },
    )
    .expect("version")
    .id;
    dats::begin_load(c, v).expect("begin");
    let t = TitleInput {
        name,
        base_name: name,
        group_key: name,
        clone_of: None,
        regions: &[],
        languages: &[],
        revision: None,
        flags: &[],
    };
    let rom = RomInput {
        name: "cpu.bin",
        size: 4,
        crc32: Some("0a0b0c0d"),
        md5: Some("0123456789abcdef0123456789abcdef"),
        sha1: None,
        status: "good",
        header: None,
    };
    let id = titles::upsert_title(c, "arcade", v, "MAME", &t, &[rom]).expect("upsert");
    dats::retire_absent(c, v).expect("retire");
    id
}

#[test]
fn dat_loads_never_touch_mra_titles() {
    let c = conn();
    let m = mra(&c, "Example Blaster", &[("exblast.zip", true)]);
    let d = dat_game(&c, "1", "exblast");
    dat_game(&c, "2", "exquest");
    let retired = |id: TitleId| -> bool {
        c.query_row("SELECT retired FROM titles WHERE id = ?1", [id.0], |r| {
            r.get(0)
        })
        .expect("row")
    };
    assert!(!retired(m));
    assert!(retired(d));
    let mra_version = mra_version(&c, "arcade", 2).expect("version");
    assert!(dats::get(&c, mra_version).expect("get").is_none());
    assert!(dats::retire(&c, mra_version).expect("retire").is_none());
    assert!(!retired(m));
    let (items, total) = dats::list(&c, 10, 0).expect("list");
    assert_eq!((items.len(), total), (2, 2));
    assert_eq!(
        crate::db::system::wizard_counts(&c)
            .expect("counts")
            .dat_versions,
        2
    );
}

#[test]
fn the_browse_shows_mra_titles_alone_once_there_are_any() {
    let c = conn();
    dat_game(&c, "1", "exblast");
    recompute(&c);
    assert_eq!(browse(&c).len(), 1);
    crate::db::groups::flush(&c).expect("flush");
    let counts = titles::counts(&c, &[]).expect("counts");
    assert_eq!(counts["arcade"].titles, 1);
    mra(&c, "Example Blaster", &[("exblast.zip", true)]);
    mra(&c, "Example Quest", &[("exquest.zip", false)]);
    recompute(&c);
    let names: Vec<String> = browse(&c).into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["Example Blaster", "Example Quest"]);
    crate::db::groups::flush(&c).expect("flush");
    let counts = titles::counts(&c, &[]).expect("counts");
    assert_eq!((counts["arcade"].titles, counts["arcade"].have), (2, 1));
}

#[test]
fn scans_never_match_members_to_mra_roms() {
    let c = conn();
    mra(&c, "Example Blaster", &[("exblast.zip", true)]);
    let pid = PlatformId("arcade".into());
    let m = crate::db::files::match_rom(
        &c,
        &pid,
        "0000000000000000000000000000000000000000",
        "0123456789abcdef0123456789abcdef",
        "00000000",
        0,
    )
    .expect("match");
    assert!(m.is_none());
}

#[test]
fn wanting_an_mra_title_skips_zips_on_disk() {
    let c = conn();
    let t = mra(
        &c,
        "Example Blaster",
        &[("exblast.zip", true), ("exparent.zip", false)],
    );
    let created = crate::db::downloads::want_title(&c, t, 1).expect("want");
    assert_eq!(created.len(), 1);
    let rom: String = c
        .query_row(
            "SELECT r.name FROM downloads d JOIN roms r ON r.id = d.rom_id",
            [],
            |r| r.get(0),
        )
        .expect("row");
    assert_eq!(rom, "exparent.zip");
}

#[test]
fn group_detail_carries_the_mra_block() {
    let c = conn();
    let t = mra(&c, "Example Blaster", &[("exblast.zip", false)]);
    recompute(&c);
    let d = titles::group_detail(&c, t).expect("detail").expect("group");
    let v = &d.variants[0];
    assert_eq!(v.source, "mra");
    let mra = v.mra.as_ref().expect("mra");
    assert_eq!(mra.missing_zips, ["mame/exblast.zip"]);
    let json = serde_json::to_value(v).expect("json");
    assert!(json.get("romset").is_none());
    assert_eq!(json["mra"]["setname"], "exblast");
}

#[test]
fn import_reads_find_zip_roms_their_titles_and_dat_entries() {
    let c = conn();
    let main = mra(
        &c,
        "Example Blaster",
        &[("exblast.zip", false), ("exparent.zip", true)],
    );
    let alt = mra(&c, "Example Blaster (set 2)", &[("ExBlast.zip", false)]);
    let rom: i64 = c
        .query_row(
            "SELECT id FROM roms WHERE title_id = ?1 AND name = 'exblast.zip'",
            [main.0],
            |r| r.get(0),
        )
        .expect("rom");
    let found = zip_rom(&c, rom).expect("read").expect("zip");
    assert_eq!(
        (
            found.title_id,
            found.name.as_str(),
            found.zip_dir.as_str(),
            found.mra_path.as_str()
        ),
        (main, "exblast.zip", "mame", "Example.mra")
    );
    let naming = titles_naming(&c, "arcade", "MAME", "EXBLAST.zip").expect("naming");
    assert_eq!(
        naming.iter().map(|(t, _)| *t).collect::<Vec<_>>(),
        [main, alt]
    );
    assert_eq!(
        set_zip_present(&c, alt, "exblast.zip", "mame", true).expect("set"),
        1
    );
    assert_eq!(
        info(&c, alt).expect("info").expect("mra").missing_zips,
        Vec::<String>::new()
    );
    assert_eq!(
        info(&c, main).expect("info").expect("mra").missing_zips,
        ["mame/exblast.zip"]
    );

    let dat = dat_game(&c, "1", "exblast");
    assert!(zip_rom(&c, 0).expect("read").is_none());
    assert_eq!(
        dat_entry_named(&c, "arcade", "ExBlast", false).expect("dat"),
        Some(dat)
    );
    assert_eq!(
        dat_entry_named(&c, "arcade", "exblast", true).expect("dat"),
        None
    );
    assert_eq!(
        dat_entry_named(&c, "arcade", "exquest", false).expect("dat"),
        None
    );
    assert_eq!(
        dat_entry_named(&c, "arcade", "Example Blaster", false).expect("dat"),
        None
    );
}
