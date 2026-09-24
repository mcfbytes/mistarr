use super::*;
use crate::db::dats::{self, NewVersion};
use mistarr_core::naming::group_key;

const DAT: &str = "Maker - Game Boy";

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
    let (dv, regions): (i64, String) = c
        .query_row(
            "SELECT dat_version_id, regions FROM titles WHERE id = ?1",
            [id.0],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("title");
    assert_eq!((dv, regions.as_str()), (v2.0, r#"["Europe"]"#));
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

#[test]
fn browse_uses_indexes_for_the_group_lookups() {
    let c = conn();
    let plan: Vec<String> = c
        .prepare(&format!(
            "EXPLAIN QUERY PLAN SELECT COUNT(*) FROM title_groups g WHERE {BROWSE_WHERE}"
        ))
        .expect("prepare")
        .query_map(
            params![
                "gb",
                None::<String>,
                "any",
                "any",
                "[]",
                None::<String>,
                "[]"
            ],
            |r| r.get(3),
        )
        .expect("query")
        .collect::<rusqlite::Result<_>>()
        .expect("rows");
    let plan = plan.join("\n");
    assert!(plan.contains("titles_group_root"), "{plan}");
    assert!(plan.contains("files_rom"), "{plan}");
}
