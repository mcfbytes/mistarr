use super::*;
use crate::db::dats::{self, NewVersion};
use mistarr_core::naming::group_key;
use proptest::prelude::*;

const DAT: &str = "Maker - Game Boy";

/// [`super::browse`] after refreshing the groups the test's autocommit writes left dirty.
fn browse(
    c: &Connection,
    platform: &str,
    filter: &Browse,
    limit: u32,
    offset: u32,
) -> Result<(Vec<GroupRow>, u64)> {
    groups::flush(c)?;
    super::browse(c, platform, filter, limit, offset)
}

/// [`super::counts`] after the same refresh.
fn counts(c: &Connection, hidden: &[String]) -> Result<HashMap<String, Counts>> {
    groups::flush(c)?;
    super::counts(c, hidden)
}

fn conn() -> Connection {
    let mut c = Connection::open_in_memory().expect("open");
    crate::db::migrate::apply(&mut c).expect("migrate");
    crate::db::platforms::seed(&mut c, &mistarr_mister::platforms::PLATFORMS).expect("seed");
    c
}

fn version(c: &Connection, v: &str) -> DatVersionId {
    dats::upsert_version(
        c,
        &NewVersion {
            dat_name: DAT,
            version: v,
            source_file: "gb.dat",
            platform: Some("gb"),
            now: v.parse().unwrap_or(0),
        },
    )
    .expect("version")
    .id
}

/// Stores a game the way the import job does, with roms `(name, status)`.
fn add(
    c: &Connection,
    v: DatVersionId,
    name: &str,
    clone_of: Option<&str>,
    roms: &[(&str, &str)],
) -> TitleId {
    let p = parse_name(name);
    let regions: Vec<String> = p.regions.iter().map(|r| r.name().to_owned()).collect();
    let flags = p.flag_labels();
    let key = group_key(&p);
    let t = TitleInput {
        name,
        base_name: &p.base_name,
        group_key: &key,
        clone_of,
        regions: &regions,
        languages: &p.languages,
        revision: p.revision.as_ref().map(|r| r.label.as_str()),
        flags: &flags,
    };
    let roms: Vec<_> = roms
        .iter()
        .map(|&(name, status)| RomInput {
            name,
            size: 4,
            crc32: Some("0a0b0c0d"),
            md5: None,
            sha1: None,
            status,
            header: None,
        })
        .collect();
    upsert_title(c, "gb", v, &t, &roms).expect("upsert")
}

fn rom(c: &Connection, title: TitleId, name: &str) -> i64 {
    c.query_row(
        "SELECT id FROM roms WHERE title_id = ?1 AND name = ?2",
        params![title.0, name],
        |r| r.get(0),
    )
    .expect("rom")
}

fn file(c: &Connection, rom: Option<i64>, path: &str, state: &str) -> i64 {
    c.execute(
        "INSERT INTO files (platform_id, rel_path, size, mtime, rom_id, state, scanned_at)
         VALUES ('gb', ?1, 4, 0, ?2, ?3, 0)",
        params![path, rom, state],
    )
    .expect("file");
    c.last_insert_rowid()
}

fn parent(c: &Connection, id: TitleId) -> TitleId {
    group_of(c, id).expect("group").expect("title")
}

fn pick_names(c: &Connection) -> Vec<String> {
    let mut stmt = c
        .prepare("SELECT name FROM titles WHERE is_1g1r_pick = 1 ORDER BY name")
        .expect("prepare");
    stmt.query_map([], |r| r.get(0))
        .expect("query")
        .collect::<rusqlite::Result<_>>()
        .expect("rows")
}

/// A plain DAT: one inferred group of four, a BIOS entry and a second game.
fn plain(c: &Connection) -> DatVersionId {
    let v = version(c, "1");
    add(c, v, "Example Quest (Japan)", None, &[("q.bin", "good")]);
    add(c, v, "Example Quest (USA)", None, &[("q.bin", "good")]);
    add(
        c,
        v,
        "Example Quest (USA) (Rev 1)",
        None,
        &[("q.bin", "good")],
    );
    add(
        c,
        v,
        "Example Quest (USA) (Beta)",
        None,
        &[("q.bin", "good")],
    );
    add(
        c,
        v,
        "[BIOS] Example System (USA)",
        None,
        &[("bios.bin", "good")],
    );
    add(c, v, "Other Tale (Europe)", None, &[("t.bin", "good")]);
    link_parents(c, v, false).expect("link");
    recompute_platform(c, "gb", &Prefs::default()).expect("recompute");
    v
}

fn id_of(c: &Connection, name: &str) -> TitleId {
    c.query_row("SELECT id FROM titles WHERE name = ?1", [name], |r| {
        r.get(0).map(TitleId)
    })
    .expect("title")
}

#[test]
fn upsert_keeps_ids_across_versions_and_retires_dropped_roms() {
    let c = conn();
    let v1 = version(&c, "1");
    let id = add(
        &c,
        v1,
        "Example Saga (Europe)",
        None,
        &[("a.bin", "good"), ("b.bin", "good")],
    );
    let v2 = version(&c, "2");
    assert_eq!(
        add(
            &c,
            v2,
            "Example Saga (Europe)",
            None,
            &[("a.bin", "baddump")]
        ),
        id
    );
    let dv: i64 = c
        .query_row(
            "SELECT dat_version_id FROM titles WHERE id = ?1",
            [id.0],
            |r| r.get(0),
        )
        .expect("title");
    assert_eq!(dv, v2.0);
    assert_eq!(
        tags_of(&c, id.0, Tag::Regions).expect("regions"),
        ["Europe"]
    );
    let roms: Vec<(String, String, bool)> = c
        .prepare("SELECT name, status, retired FROM roms WHERE title_id = ?1 ORDER BY name")
        .expect("prepare")
        .query_map([id.0], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .expect("query")
        .collect::<rusqlite::Result<_>>()
        .expect("rows");
    assert_eq!(
        roms,
        [
            ("a.bin".to_owned(), "baddump".to_owned(), false),
            ("b.bin".to_owned(), "good".to_owned(), true)
        ]
    );
    assert_eq!(dats::retire_absent(&c, v2).expect("retire"), 0);
}

#[test]
fn clone_of_links_parents_and_falls_back_to_self() {
    let c = conn();
    let v = version(&c, "1");
    let p = add(&c, v, "Example Quest (USA)", None, &[]);
    let j = add(
        &c,
        v,
        "Example Quest (Japan)",
        Some("Example Quest (USA)"),
        &[],
    );
    let lost = add(&c, v, "Example Quest (Europe)", Some("Missing (USA)"), &[]);
    link_parents(&c, v, true).expect("link");
    assert_eq!(parent(&c, j), p);
    assert_eq!(parent(&c, p), p);
    assert_eq!(parent(&c, lost), lost);
    let inferred: i64 = c
        .query_row("SELECT SUM(inferred) FROM titles", [], |r| r.get(0))
        .expect("sum");
    assert_eq!(inferred, 0);
}

#[test]
fn inferred_groups_elect_a_parent_and_picks_follow_prefs() {
    let c = conn();
    plain(&c);
    let rev = id_of(&c, "Example Quest (USA) (Rev 1)");
    for name in ["Example Quest (Japan)", "Example Quest (USA) (Beta)"] {
        assert_eq!(parent(&c, id_of(&c, name)), rev, "{name}");
    }
    let bios = id_of(&c, "[BIOS] Example System (USA)");
    assert_eq!(parent(&c, bios), bios);
    assert_eq!(
        pick_names(&c),
        ["Example Quest (USA) (Rev 1)", "Other Tale (Europe)"]
    );

    let japan = Prefs {
        regions: vec!["Japan".into()],
        ..Prefs::default()
    };
    let r = recompute_platform(&c, "gb", &japan).expect("recompute");
    assert_eq!((r.groups, r.picks), (3, 2));
    assert_eq!(
        pick_names(&c),
        ["Example Quest (Japan)", "Other Tale (Europe)"]
    );
    assert_eq!(parent(&c, id_of(&c, "Example Quest (Japan)")), rev);

    let oldest = Prefs {
        prefer_latest_revision: false,
        ..Prefs::default()
    };
    recompute_platform(&c, "gb", &oldest).expect("recompute");
    assert_eq!(pick_names(&c)[0], "Example Quest (USA)");
}

#[test]
fn bad_dumps_lose_the_pick() {
    let c = conn();
    let v = version(&c, "1");
    add(&c, v, "Example Quest (USA)", None, &[("q.bin", "baddump")]);
    add(&c, v, "Example Quest (USA) [b]", None, &[("q.bin", "good")]);
    add(
        &c,
        v,
        "Example Quest (USA) (Alt)",
        None,
        &[("q.bin", "good")],
    );
    link_parents(&c, v, false).expect("link");
    recompute_platform(&c, "gb", &Prefs::default()).expect("recompute");
    assert_eq!(pick_names(&c), ["Example Quest (USA) (Alt)"]);
}

#[test]
fn view_counts_a_title_once_whatever_its_roms_and_files() {
    let c = conn();
    let v = version(&c, "1");
    let disc = add(
        &c,
        v,
        "Example Saga (USA)",
        None,
        &[
            ("Example Saga (USA) (Track 1).bin", "good"),
            ("Example Saga (USA) (Track 2).bin", "good"),
        ],
    );
    link_parents(&c, v, true).expect("link");
    let t1 = rom(&c, disc, "Example Saga (USA) (Track 1).bin");
    let t2 = rom(&c, disc, "Example Saga (USA) (Track 2).bin");
    file(&c, Some(t1), "saga/t1.bin", "verified");
    file(&c, Some(t1), "copy/t1.bin", "verified");
    let have = |c: &Connection| -> (i64, i64) {
        groups::flush(c).expect("flush");
        c.query_row(
            "SELECT variants, have_verified FROM title_groups WHERE parent_id = ?1",
            [disc.0],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("group")
    };
    assert_eq!(have(&c), (1, 0), "one track of two is not a verified game");
    file(&c, Some(t2), "saga/t2.bin", "verified");
    file(&c, Some(t2), "saga/t2-old.bin", "misnamed");
    assert_eq!(have(&c), (1, 1));
    file(&c, None, "stray.bin", "unverified");
    c.execute("UPDATE titles SET wanted = 1 WHERE id = ?1", [disc.0])
        .expect("want");
    let counts = counts(&c, &["bios".to_owned()]).expect("counts");
    assert_eq!(
        counts.get("gb"),
        Some(&Counts {
            titles: 1,
            have: 1,
            wanted: 1,
            unmatched_files: 1,
            failing_check: 0,
            partial: 0
        })
    );
    assert!(!counts.contains_key("nes"));
}

/// A minimal MRA title fixture, one row per unique `(name, setname)`.
fn arcade_title<'a>(name: &'a str, setname: &'a str) -> crate::db::arcade::MraTitle<'a> {
    arcade_title_grouped(name, setname, "")
}

/// [`arcade_title`] with an explicit `group_key`, so `recompute_platform` clusters it
/// with every other title sharing the same key into one clone group.
fn arcade_title_grouped<'a>(
    name: &'a str,
    setname: &'a str,
    group_key: &'a str,
) -> crate::db::arcade::MraTitle<'a> {
    crate::db::arcade::MraTitle {
        name,
        base_name: name,
        group_key,
        regions: &[],
        languages: &[],
        revision: None,
        flags: &[],
        setname: Some(setname),
        rbf: Some("core"),
        mra_path: "x.mra",
        file_stamp: "1:1",
        run: 1,
    }
}

#[test]
fn counts_report_arcade_sets_failing_check_or_partly_present() {
    let c = conn();
    let v = crate::db::arcade::mra_version(&c, "arcade", 1).expect("version");
    // A complete set whose md5 check failed.
    let mismatched = crate::db::arcade::upsert_title(
        &c,
        "arcade",
        v,
        &arcade_title("Example Blaster", "exblast"),
        &[crate::db::arcade::MraZip {
            name: "exblast.zip",
            zip_dir: "mame",
            md5: None,
            present: true,
        }],
    )
    .expect("upsert");
    crate::db::arcade::set_check(&c, mismatched, Some("mismatch"), None, None).expect("check");
    // A set naming two zips, only one of them present.
    crate::db::arcade::upsert_title(
        &c,
        "arcade",
        v,
        &arcade_title("Example Quest", "exquest"),
        &[
            crate::db::arcade::MraZip {
                name: "exquest.zip",
                zip_dir: "mame",
                md5: None,
                present: true,
            },
            crate::db::arcade::MraZip {
                name: "exquest2.zip",
                zip_dir: "mame",
                md5: None,
                present: false,
            },
        ],
    )
    .expect("upsert");
    // A fully present, matching set: neither failing nor partial.
    let matched = crate::db::arcade::upsert_title(
        &c,
        "arcade",
        v,
        &arcade_title("Example Homebrew", "exhb"),
        &[crate::db::arcade::MraZip {
            name: "exhb.zip",
            zip_dir: "mame",
            md5: None,
            present: true,
        }],
    )
    .expect("upsert");
    crate::db::arcade::set_check(&c, matched, Some("match"), None, None).expect("check");

    let counts = counts(&c, &[]).expect("counts");
    let arcade = counts.get("arcade").expect("arcade");
    assert_eq!(arcade.titles, 3);
    assert_eq!(arcade.failing_check, 1);
    assert_eq!(arcade.partial, 1);
}

/// An MRA title never checked (its zip is absent) leaves every aggregate NULL
/// unless each is wrapped in `COALESCE`; the counts still read as zeros.
#[test]
fn counts_read_zero_for_an_unchecked_mra_title_with_its_zip_absent() {
    let c = conn();
    let v = crate::db::arcade::mra_version(&c, "arcade", 1).expect("version");
    crate::db::arcade::upsert_title(
        &c,
        "arcade",
        v,
        &arcade_title("Example Blaster", "exblast"),
        &[crate::db::arcade::MraZip {
            name: "exblast.zip",
            zip_dir: "mame",
            md5: None,
            present: false,
        }],
    )
    .expect("upsert");
    let counts = counts(&c, &[]).expect("counts");
    assert_eq!(
        counts.get("arcade"),
        Some(&Counts {
            titles: 1,
            have: 0,
            wanted: 0,
            unmatched_files: 0,
            failing_check: 0,
            partial: 0
        })
    );
}

#[test]
fn a_refused_check_does_not_count_as_failing() {
    let c = conn();
    let v = crate::db::arcade::mra_version(&c, "arcade", 1).expect("version");
    let t = crate::db::arcade::upsert_title(
        &c,
        "arcade",
        v,
        &arcade_title("Example Blaster", "exblast"),
        &[crate::db::arcade::MraZip {
            name: "exblast.zip",
            zip_dir: "mame",
            md5: None,
            present: true,
        }],
    )
    .expect("upsert");
    crate::db::arcade::set_check(&c, t, Some("refused"), None, None).expect("check");

    let counts = counts(&c, &[]).expect("counts");
    let arcade = counts.get("arcade").expect("arcade");
    assert_eq!(
        arcade.failing_check, 0,
        "a refused check is neither mismatch nor missing_part"
    );
}

/// `failing_check` and `partial` count clone groups, not raw titles: a group with a
/// have-verified visible variant never counts even when one of its other variants
/// fails, and a group with neither counts toward both when it has both kinds.
#[test]
fn failing_check_and_partial_count_clone_groups_not_titles() {
    let c = conn();
    let v = crate::db::arcade::mra_version(&c, "arcade", 1).expect("version");
    let zip = |name: &'static str, present: bool| crate::db::arcade::MraZip {
        name,
        zip_dir: "mame",
        md5: None,
        present,
    };

    // Group 1 (group_key "mra:example blaster"): a have-verified main variant
    // plus a mismatched alternate.
    let main = crate::db::arcade::upsert_title(
        &c,
        "arcade",
        v,
        &arcade_title_grouped("Example Blaster", "exblast", "mra:example blaster"),
        &[zip("exblast.zip", true)],
    )
    .expect("upsert");
    crate::db::arcade::set_check(&c, main, Some("match"), None, None).expect("check");
    let alt = crate::db::arcade::upsert_title(
        &c,
        "arcade",
        v,
        &arcade_title_grouped("Example Blaster (set 2)", "exblast2", "mra:example blaster"),
        &[zip("exblast2.zip", true)],
    )
    .expect("upsert");
    crate::db::arcade::set_check(&c, alt, Some("mismatch"), None, None).expect("check");

    // Group 2 (group_key "mra:example quest"): no have-verified variant, one
    // failing and one partly present.
    let failing = crate::db::arcade::upsert_title(
        &c,
        "arcade",
        v,
        &arcade_title_grouped("Example Quest", "exquest", "mra:example quest"),
        &[zip("exquest.zip", true)],
    )
    .expect("upsert");
    crate::db::arcade::set_check(&c, failing, Some("missing_part"), None, None).expect("check");
    let partial = crate::db::arcade::upsert_title(
        &c,
        "arcade",
        v,
        &arcade_title_grouped("Example Quest (set 2)", "exquest2", "mra:example quest"),
        &[zip("exquest2.zip", true), zip("exquest2b.zip", false)],
    )
    .expect("upsert");

    // The real grouping path: matching group_key clusters both pairs into one
    // clone group each, exactly as a catalogue run would.
    recompute_platform(&c, "arcade", &Prefs::default()).expect("recompute");
    let parent_of = |id: crate::db::titles::TitleId| -> i64 {
        c.query_row("SELECT parent_id FROM titles WHERE id = ?1", [id.0], |r| {
            r.get(0)
        })
        .expect("parent")
    };
    assert_eq!(
        parent_of(main),
        parent_of(alt),
        "the two blasters share one clone group"
    );
    assert_eq!(
        parent_of(failing),
        parent_of(partial),
        "the two quests share one clone group"
    );

    let counts = counts(&c, &[]).expect("counts");
    let arcade = counts.get("arcade").expect("arcade");
    assert_eq!(
        arcade.failing_check, 1,
        "group 1's mismatched alt does not count because its main is have"
    );
    assert_eq!(
        arcade.partial, 1,
        "only group 2, which has no have-verified variant, counts"
    );
}

#[test]
fn browse_filters_sorts_and_pages() {
    let c = conn();
    plain(&c);
    let hidden: Vec<String> = ["bios", "beta"].map(str::to_owned).to_vec();
    let names = |f: &Browse| -> (Vec<String>, u64) {
        let (rows, total) = browse(&c, "gb", f, 10, 0).expect("browse");
        (rows.into_iter().map(|r| r.base_name).collect(), total)
    };
    let base = Browse {
        hidden: hidden.clone(),
        ..Browse::default()
    };
    assert_eq!(
        names(&base),
        (vec!["Example Quest".into(), "Other Tale".into()], 2)
    );

    let (rows, _) = browse(&c, "gb", &base, 1, 0).expect("page");
    assert_eq!(
        rows[0].pick_name.as_deref(),
        Some("Example Quest (USA) (Rev 1)")
    );
    assert_eq!((rows[0].variants, rows[0].has_pick), (4, true));
    let (rows, total) = browse(&c, "gb", &base, 1, 1).expect("page 2");
    assert_eq!((rows[0].base_name.as_str(), total), ("Other Tale", 2));

    let q = Browse {
        q: Some("tale".into()),
        ..base.clone()
    };
    assert_eq!(names(&q).0, ["Other Tale"]);
    let pct = Browse {
        q: Some("%".into()),
        ..base.clone()
    };
    assert_eq!(names(&pct).1, 0, "LIKE wildcards are literal");
    let region = Browse {
        region: Some("europe".into()),
        ..base.clone()
    };
    assert_eq!(names(&region).0, ["Other Tale"]);
    let bios = Browse {
        flags: vec!["bios".into()],
        hidden: vec!["beta".into()],
        ..Browse::default()
    };
    assert_eq!(names(&bios).0, ["Example System"]);
    let beta = Browse {
        flags: vec!["beta".into()],
        hidden: vec!["bios".into()],
        ..Browse::default()
    };
    assert_eq!(names(&beta).0, ["Example Quest"]);

    let tale = id_of(&c, "Other Tale (Europe)");
    c.execute("UPDATE titles SET wanted = 1 WHERE id = ?1", [tale.0])
        .expect("want");
    let t2 = rom(&c, tale, "t.bin");
    file(&c, Some(t2), "t.bin", "verified");
    let wanted = Browse {
        wanted: Tri::Yes,
        ..base.clone()
    };
    assert_eq!(names(&wanted).0, ["Other Tale"]);
    let missing = Browse {
        have: Tri::No,
        ..base.clone()
    };
    assert_eq!(names(&missing).0, ["Example Quest"]);
    let have_first = Browse {
        sort: Sort::Have,
        ..base.clone()
    };
    assert_eq!(names(&have_first).0[0], "Other Tale");
    let v2 = version(&c, "2");
    add(&c, v2, "Newer Tale (USA)", None, &[]);
    link_parents(&c, v2, false).expect("link");
    recompute_platform(&c, "gb", &Prefs::default()).expect("recompute");
    let recent = Browse {
        sort: Sort::Recent,
        ..base
    };
    assert_eq!(names(&recent).0[0], "Newer Tale");
}

#[test]
fn hidden_and_flags_are_orthogonal() {
    let c = conn();
    plain(&c);
    let hide: Vec<String> = vec!["bios".to_owned()];
    let names = |f: &Browse| -> Vec<String> {
        browse(&c, "gb", f, 10, 0)
            .expect("browse")
            .0
            .into_iter()
            .map(|r| r.base_name)
            .collect()
    };
    // hide, no flags: the BIOS-only group is absent.
    let hide_no_flags = Browse {
        hidden: hide.clone(),
        ..Browse::default()
    };
    assert!(!names(&hide_no_flags).contains(&"Example System".to_owned()));
    // show, no flags: the BIOS-only group appears.
    let show_no_flags = Browse::default();
    assert!(names(&show_no_flags).contains(&"Example System".to_owned()));
    // hide, require bios: still absent, since hidden excludes it regardless of flags.
    let hide_flags = Browse {
        hidden: hide.clone(),
        flags: vec!["bios".into()],
        ..Browse::default()
    };
    assert!(names(&hide_flags).is_empty());
    // show, require bios: only the BIOS group appears.
    let show_flags = Browse {
        flags: vec!["bios".into()],
        ..Browse::default()
    };
    assert_eq!(names(&show_flags), ["Example System"]);
}

#[test]
fn detail_lists_variants_roms_files_and_sources() {
    let c = conn();
    plain(&c);
    let usa = id_of(&c, "Example Quest (USA)");
    let q_rom = rom(&c, usa, "q.bin");
    let f = file(&c, Some(q_rom), "Example Quest.gb", "misnamed");
    c.execute_batch(
        "INSERT INTO sources (infohash, display_name, origin_file, state, added_at)
         VALUES ('00', 'n', 'a.torrent', 'bound', 0);",
    )
    .expect("source");
    c.execute(
        "INSERT INTO torrent_files (source_id, file_index, path, size, rom_id)
         VALUES (1, 0, 'q.bin', 4, ?1)",
        [q_rom],
    )
    .expect("torrent file");
    let d = group_detail(&c, usa).expect("detail").expect("group");
    let rev = id_of(&c, "Example Quest (USA) (Rev 1)");
    assert_eq!((d.parent_id, d.pick_variant_id), (rev, Some(rev)));
    assert_eq!(d.base_name, "Example Quest");
    assert_eq!(d.variants.len(), 4);
    assert_eq!(d.variants[0].id, rev, "pick first");
    let v = d.variants.iter().find(|v| v.id == usa).expect("usa");
    assert_eq!(v.regions, ["USA"]);
    assert_eq!(v.torrent_files_available, 1);
    assert_eq!(v.roms.len(), 1);
    assert_eq!(
        (v.roms[0].file_id, v.roms[0].file_state.as_deref()),
        (Some(f), Some("misnamed"))
    );
    let beta = d
        .variants
        .iter()
        .find(|v| v.flags == ["beta"])
        .expect("beta");
    assert!(beta.inferred && !beta.is_1g1r_pick);
    assert!(group_detail(&c, TitleId(999)).expect("detail").is_none());
    assert_eq!(TitleId(3).to_string(), "3");
}

#[test]
fn want_refuses_bios_and_retired_and_unwant_cancels_queued_downloads() {
    let c = conn();
    plain(&c);
    let bios = id_of(&c, "[BIOS] Example System (USA)");
    assert_eq!(want(&c, bios).expect("want"), Err(WantRefused::Bios));
    let usa = id_of(&c, "Example Quest (USA)");
    assert_eq!(want(&c, usa).expect("want"), Ok(()));
    let rev = id_of(&c, "Example Quest (USA) (Rev 1)");
    assert_eq!(want(&c, rev).expect("want"), Ok(()));
    let q = rom(&c, usa, "q.bin");
    c.execute_batch(
        "INSERT INTO sources (infohash, display_name, origin_file, state, added_at)
         VALUES ('00', 'n', 'a.torrent', 'bound', 0);",
    )
    .expect("source");
    for state in ["queued", "transferring"] {
        c.execute(
            "INSERT INTO downloads (title_id, rom_id, source_id, file_index, state, created_at, updated_at)
             VALUES (?1, ?2, 1, 0, ?3, 0, 0)",
            params![usa.0, q, state],
        )
        .expect("download");
    }
    assert_eq!(unwant_group(&c, rev, 5).expect("unwant"), 2);
    let states: Vec<String> = c
        .prepare("SELECT state FROM downloads ORDER BY id")
        .expect("prepare")
        .query_map([], |r| r.get(0))
        .expect("query")
        .collect::<rusqlite::Result<_>>()
        .expect("rows");
    assert_eq!(states, ["cancelled", "transferring"]);

    let tale = id_of(&c, "Other Tale (Europe)");
    let v2 = version(&c, "2");
    add(&c, v2, "Example Quest (USA)", None, &[]);
    assert!(dats::retire_absent(&c, v2).expect("retire") > 0);
    assert_eq!(want(&c, tale).expect("want"), Err(WantRefused::Retired));
    assert_eq!(
        want(&c, TitleId(999)).expect("want"),
        Err(WantRefused::Missing)
    );
}

#[test]
fn browse_walks_an_index_in_every_sort_order() {
    let c = conn();
    plain(&c);
    let hidden: Vec<String> = ["bios", "beta"].map(str::to_owned).to_vec();
    for (sort, index) in [
        (Sort::Name, "title_groups_name"),
        (Sort::Have, "title_groups_have"),
        (Sort::Recent, "title_groups_recent"),
    ] {
        for shape in SearchShape::ALL {
            let filter = Browse {
                hidden: hidden.clone(),
                q: Some("quest".into()),
                sort,
                ..Browse::default()
            };
            let clause = browse_clause(&c, "gb", &filter, shape).expect("clause");
            let args = clause
                .args
                .iter()
                .cloned()
                .chain([Value::from(60), Value::from(0)]);
            let plan: Vec<String> = c
                .prepare(&format!("EXPLAIN QUERY PLAN {}", page_sql(&clause, sort)))
                .expect("prepare")
                .query_map(params_from_iter(args), |r| r.get(3))
                .expect("query")
                .collect::<rusqlite::Result<_>>()
                .expect("rows");
            let plan = plan.join("\n");
            assert!(plan.contains(index), "{sort:?}: {plan}");
            assert!(!plan.contains("TEMP B-TREE"), "{sort:?}: {plan}");
            assert!(plan.contains("titles_group_root"), "{sort:?}: {plan}");
            let fts = shape != SearchShape::Like;
            assert_eq!(
                plan.contains("LIST SUBQUERY"),
                fts,
                "{sort:?} {shape:?}: {plan}"
            );
            assert_eq!(
                plan.contains("SCAN title_search VIRTUAL TABLE"),
                fts,
                "{sort:?} {shape:?}: {plan}"
            );
        }
    }
    assert_eq!(SEARCH_SHAPE, SearchShape::FtsPlatform);
}

#[test]
fn every_search_shape_finds_the_same_groups() {
    let mut c = conn();
    crate::synth::seed(&mut c, 0.05, 2).expect("seed");
    let hidden: Vec<String> = ["bios", "beta"].map(str::to_owned).to_vec();
    for (platform, q) in [
        ("nes", "sta"),
        ("snes", "the"),
        ("gb", "an"),
        ("psx", "Vexmir"),
        ("nes", ""),
    ] {
        let filter = Browse {
            q: Some(q.to_owned()).filter(|q| !q.is_empty()),
            hidden: hidden.clone(),
            ..Browse::default()
        };
        let pages: Vec<_> = SearchShape::ALL
            .into_iter()
            .map(|s| browse_with(&c, platform, &filter, 60, 0, s).expect("browse"))
            .collect();
        assert!(pages.windows(2).all(|w| w[0] == w[1]), "{platform} {q}");
    }
    // The sentinels keep one platform's id from matching inside another's.
    for platform in ["nes", "snes", "gb", "gbc"] {
        let count = |sql: &str| -> i64 { c.query_row(sql, [platform], |r| r.get(0)).expect(sql) };
        assert_eq!(
            count(
                "SELECT COUNT(*) FROM title_search
                 WHERE title_search MATCH 'platform : \"' || char(31) || ?1 || char(31) || '\"'"
            ),
            count("SELECT COUNT(*) FROM titles WHERE platform_id = ?1"),
            "{platform}"
        );
    }
}

#[test]
fn removing_a_dat_retires_its_roms_unwants_and_cancels_queued_downloads() {
    let c = conn();
    let v = plain(&c);
    let usa = id_of(&c, "Example Quest (USA)");
    assert_eq!(want(&c, usa).expect("want"), Ok(()));
    let q = rom(&c, usa, "q.bin");
    c.execute_batch(
        "INSERT INTO sources (infohash, display_name, origin_file, state, added_at)
         VALUES ('00', 'n', 'a.torrent', 'bound', 0);",
    )
    .expect("source");
    for state in ["queued", "wanted", "transferring"] {
        c.execute(
            "INSERT INTO downloads (title_id, rom_id, source_id, file_index, state, created_at, updated_at)
             VALUES (?1, ?2, 1, 0, ?3, 0, 0)",
            params![usa.0, q, state],
        )
        .expect("download");
    }
    assert!(dats::retire(&c, v, 5).expect("retire").is_some());
    let count = |sql: &str| -> i64 { c.query_row(sql, [], |r| r.get(0)).expect("count") };
    assert_eq!(count("SELECT COUNT(*) FROM roms WHERE retired = 0"), 0);
    assert_eq!(
        count("SELECT COUNT(*) FROM titles WHERE wanted = 1 OR retired = 0"),
        0
    );
    let states: Vec<String> = c
        .prepare("SELECT state FROM downloads ORDER BY id")
        .expect("prepare")
        .query_map([], |r| r.get(0))
        .expect("query")
        .collect::<rusqlite::Result<_>>()
        .expect("rows");
    assert_eq!(states, ["cancelled", "cancelled", "transferring"]);
}

/// The reference for [`reroot`], one title at a time: a title keeps its group unless that
/// group's root title is known and in another group, and then takes the group's
/// lowest live member, or its lowest member when none is live.
fn reference_reroot(nodes: &[Node]) -> Vec<Option<i64>> {
    nodes
        .iter()
        .map(|n| {
            let label = n.target?;
            let root = nodes.iter().find(|m| m.id == label);
            if root.is_none_or(|r| r.target == Some(label)) {
                return Some(label);
            }
            let members: Vec<&Node> = nodes.iter().filter(|m| m.target == Some(label)).collect();
            let live = members.iter().filter(|m| m.live).map(|m| m.id).min();
            live.or_else(|| members.iter().map(|m| m.id).min())
        })
        .collect()
}

/// Random titles: `(target, live)` with target an index into the titles, or past
/// them for a group rooted by a title that is not loaded, and ids in random order.
fn nodes_strategy() -> impl Strategy<Value = Vec<Node>> {
    proptest::collection::vec(
        (
            proptest::option::weighted(0.95, 0usize..28),
            proptest::bool::weighted(0.8),
        ),
        1..24,
    )
    .prop_flat_map(|specs| {
        let order: Vec<i64> = (0..specs.len())
            .map(|i| i64::try_from(i).expect("i"))
            .collect();
        (Just(specs), Just(order).prop_shuffle())
    })
    .prop_map(|(specs, order)| {
        let id = |i: usize| {
            order
                .get(i)
                .map_or(1000 + i64::try_from(i).expect("i"), |&o| o * 3 + 1)
        };
        let mut nodes: Vec<Node> = specs
            .iter()
            .enumerate()
            .map(|(i, &(target, live))| Node {
                id: id(i),
                target: target.map(id),
                current: None,
                live,
            })
            .collect();
        nodes.sort_unstable_by_key(|n| n.id);
        nodes
    })
}

/// A synthetic game of [`catalogue_strategy`]: its version, a pick among the games of
/// that version for its parent, whether it is retired, and its roms' keys (`None` unhashed).
#[derive(Debug, Clone)]
struct GameSpec {
    version: usize,
    parent: usize,
    retired: bool,
    roms: Vec<Option<u8>>,
}

/// Three DAT versions' game counts, their games, and the order of the games' ids.
fn catalogue_strategy() -> impl Strategy<Value = ([i64; 3], Vec<GameSpec>, Vec<i64>)> {
    let game = (
        0usize..3,
        0usize..8,
        proptest::bool::weighted(0.15),
        proptest::collection::vec(proptest::option::weighted(0.9, 0u8..5), 1..3),
    )
        .prop_map(|(version, parent, retired, roms)| GameSpec {
            version,
            parent,
            retired,
            roms,
        });
    (
        [1i64..6, 1i64..6, 1i64..6],
        proptest::collection::vec(game, 1..14),
    )
        .prop_flat_map(|(counts, games)| {
            let order: Vec<i64> = (1..=games.len())
                .map(|i| i64::try_from(i).expect("i"))
                .collect();
            (Just(counts), Just(games), Just(order).prop_shuffle())
        })
}

/// Stores `games` on `nes` as three DAT versions, game `i` with id `ids[i]`.
fn store_catalogue(c: &Connection, counts: [i64; 3], games: &[GameSpec], ids: &[i64]) {
    for (v, count) in counts.iter().enumerate() {
        c.execute(
            "INSERT INTO dat_versions (id, platform_id, dat_name, version, source_file, loaded_at, game_count)
             VALUES (?1, 'nes', 'Test ' || ?1, '1', 't.dat', 0, ?2)",
            params![i64::try_from(v + 1).expect("v"), count],
        )
        .expect("version");
    }
    for (g, &id) in games.iter().zip(ids) {
        c.execute(
            "INSERT INTO titles (id, platform_id, dat_version_id, name, base_name, retired)
             VALUES (?1, 'nes', ?2, 'G' || ?1, 'G', ?3)",
            params![id, i64::try_from(g.version + 1).expect("v"), g.retired],
        )
        .expect("title");
    }
    for (g, &id) in games.iter().zip(ids) {
        let peers: Vec<i64> = games
            .iter()
            .zip(ids)
            .filter(|(o, _)| o.version == g.version)
            .map(|(_, &id)| id)
            .collect();
        c.execute(
            "UPDATE titles SET parent_id = ?2 WHERE id = ?1",
            [id, peers[g.parent % peers.len()]],
        )
        .expect("parent");
        for (k, key) in g.roms.iter().enumerate() {
            c.execute(
                "INSERT INTO roms (title_id, name, size, sha1) VALUES (?1, ?2, 4, ?3)",
                params![id, format!("r{k}"), key.map(|k| format!("{k:02x}"))],
            )
            .expect("rom");
        }
    }
}

/// A title as [`reference_groups`] reads it: id, parent, version, live, and its sorted
/// rom keys when every live rom has one.
type RefTitle = (i64, Option<i64>, i64, bool, Option<Vec<String>>);

/// The documented `group_root` of every `nes` title, worked out without the code under
/// test: DAT groups, then links by sorted rom keys, then the lowest live id of each
/// group left behind.
fn reference_groups(c: &Connection) -> Vec<(i64, Option<i64>)> {
    let versions: Vec<i64> = c
        .prepare(
            "SELECT id FROM dat_versions WHERE platform_id = 'nes' AND source = 'dat'
               AND retired = 0 AND superseded_by IS NULL ORDER BY game_count DESC, id",
        )
        .expect("prepare")
        .query_map([], |r| r.get(0))
        .expect("query")
        .collect::<rusqlite::Result<_>>()
        .expect("versions");
    let mut rows: Vec<RefTitle> = c
        .prepare(
            "SELECT id, parent_id, dat_version_id, retired = 0 FROM titles
             WHERE platform_id = 'nes' ORDER BY id",
        )
        .expect("prepare")
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, None))
        })
        .expect("query")
        .collect::<rusqlite::Result<_>>()
        .expect("titles");
    for row in &mut rows {
        let keys: Vec<Option<String>> = c
            .prepare(
                "SELECT COALESCE(sha1, md5, crc32 || ':' || size) FROM roms
                 WHERE title_id = ?1 AND retired = 0",
            )
            .expect("prepare")
            .query_map([row.0], |r| r.get(0))
            .expect("query")
            .collect::<rusqlite::Result<_>>()
            .expect("roms");
        let keys: Option<Vec<String>> = keys.into_iter().collect();
        row.4 = keys.filter(|k| !k.is_empty()).map(|mut k| {
            k.sort();
            k
        });
    }
    let mut nodes: Vec<Node> = rows
        .iter()
        .map(|r| Node {
            id: r.0,
            target: r.1,
            current: None,
            live: r.3,
        })
        .collect();
    if versions.len() < 2 {
        return nodes.iter().map(|n| (n.id, n.target)).collect();
    }
    let largest = versions[0];
    let mut rest = versions[1..].to_vec();
    rest.sort_unstable();
    let sig = |r: &RefTitle| r.4.clone().filter(|_| r.3);
    for (i, t) in rows.iter().enumerate() {
        let Some(s) = sig(t).filter(|_| rest.contains(&t.2)) else {
            continue;
        };
        let same = |o: &&RefTitle| sig(o).as_ref() == Some(&s);
        let target = match rows.iter().filter(|a| a.2 == largest).find(same) {
            Some(anchor) => Some(anchor.1),
            None => rest
                .iter()
                .find_map(|&v| rows.iter().filter(|o| o.2 == v).find(same))
                .filter(|first| first.2 != t.2)
                .map(|first| first.1),
        };
        if let Some(root) = target {
            nodes[i].target = root;
        }
    }
    let roots = reference_reroot(&nodes);
    nodes.iter().zip(roots).map(|(n, r)| (n.id, r)).collect()
}

fn stored_groups(c: &Connection) -> Vec<(i64, Option<i64>)> {
    c.prepare("SELECT id, group_root FROM titles WHERE platform_id = 'nes' ORDER BY id")
        .expect("prepare")
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .collect::<rusqlite::Result<_>>()
        .expect("groups")
}

/// Recomputes `nes` in a transaction and commits it, returning the rows it changed and
/// the groups it left dirty before the commit refreshed them.
fn recompute_nes(c: &mut Connection) -> (i64, i64) {
    let changes = |c: &Connection| -> i64 {
        c.query_row("SELECT total_changes()", [], |r| r.get(0))
            .expect("changes")
    };
    let tx = c.transaction().expect("tx");
    let before = changes(&tx);
    recompute_platform(&tx, "nes", &Prefs::default()).expect("recompute");
    let written = changes(&tx) - before;
    let dirty: i64 = tx
        .query_row("SELECT COUNT(*) FROM title_groups_dirty", [], |r| r.get(0))
        .expect("dirty");
    crate::db::commit(tx).expect("commit");
    (written, dirty)
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn reroot_gives_each_stranded_group_its_lowest_live_id(nodes in nodes_strategy()) {
        let expected = reference_reroot(&nodes);
        let mut got = nodes.clone();
        reroot(&mut got);
        let got: Vec<Option<i64>> = got.iter().map(|n| n.target).collect();
        prop_assert_eq!(&got, &expected);
        for (i, a) in nodes.iter().enumerate() {
            for (j, b) in nodes.iter().enumerate() {
                prop_assert_eq!(a.target == b.target, got[i] == got[j], "no group gains or loses members");
            }
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn recompute_matches_the_documented_groups_and_rewrites_nothing_unchanged(
        (counts, games, ids) in catalogue_strategy()
    ) {
        let mut c = conn();
        store_catalogue(&c, counts, &games, &ids);
        recompute_nes(&mut c);
        prop_assert_eq!(stored_groups(&c), reference_groups(&c));
        prop_assert_eq!(recompute_nes(&mut c), (0, 0), "an unchanged catalogue writes nothing");
    }
}

/// A group's new root that is itself the root of a group left behind: each group keeps
/// its own members, and neither merges into the other whatever order they are visited in.
#[test]
fn chained_re_rooting_keeps_every_group_apart() {
    let mut c = conn();
    c.execute_batch(
        "INSERT INTO dat_versions (id, platform_id, dat_name, version, source_file, loaded_at, game_count)
           VALUES (1, 'nes', 'Test A', '1', 'a.dat', 0, 9), (2, 'nes', 'Test B', '1', 'b.dat', 0, 2),
                  (3, 'nes', 'Test C', '1', 'c.dat', 0, 2);
         INSERT INTO titles (id, platform_id, dat_version_id, name, base_name)
           VALUES (50, 'nes', 1, 'A', 'A'), (40, 'nes', 2, 'X', 'X'), (41, 'nes', 2, 'B', 'B'),
                  (10, 'nes', 3, 'N', 'N'), (11, 'nes', 3, 'C', 'C');
         UPDATE titles SET parent_id = CASE id WHEN 41 THEN 40 WHEN 11 THEN 10 ELSE id END;
         INSERT INTO roms (title_id, name, size, sha1)
           VALUES (50, 'a', 4, 'aa'), (40, 'x', 4, 'aa'), (41, 'b', 4, 'bb'),
                  (10, 'n', 4, 'bb'), (11, 'c', 4, 'cc');",
    )
    .expect("rows");
    recompute_nes(&mut c);
    let groups = stored_groups(&c);
    assert_eq!(
        groups,
        [
            (10, Some(10)),
            (11, Some(11)),
            (40, Some(50)),
            (41, Some(10)),
            (50, Some(50))
        ],
        "X links to A; B and N keep a group rooted at N; C keeps its own"
    );
    assert_eq!(groups, reference_groups(&c));
    assert_eq!(recompute_nes(&mut c), (0, 0));
}
