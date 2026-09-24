//! Browse and search speed on the synthetic full catalogue; see `docs/TESTING.md`
//! "Browse speed". Run with `--nocapture` to print the timings.

use std::path::Path;
use std::time::{Duration, Instant};

use mistarr_core::naming::{group_key, parse_name};
use mistarr_server::db::dat_stage::{self, StagedGame, StagedRom};
use mistarr_server::db::titles::{self, Browse, SearchShape, Sort, SEARCH_SHAPE};
use mistarr_server::db::{self, dats, groups};
use mistarr_server::synth::{self, BROWSED, ELSEWHERE, RARE};
use rusqlite::{params, Connection};

/// Default `prefs.hide`.
const HIDE: [&str; 6] = ["bios", "beta", "proto", "demo", "sample", "program"];

/// The reference aggregation query for `title_groups`, computed per request.
const REFERENCE: &str = "CREATE TEMP VIEW reference_groups AS
SELECT g.parent_id, g.platform_id, p.base_name, p.name,
       g.variants, g.have_verified, g.wanted, g.has_pick, g.pick_id, g.newest_id
FROM (
  SELECT v.platform_id, v.parent_id, COUNT(*) AS variants,
         SUM(v.roms > 0 AND v.roms_verified = v.roms) AS have_verified,
         SUM(v.wanted) AS wanted, MAX(v.is_1g1r_pick) AS has_pick,
         MAX(CASE WHEN v.is_1g1r_pick = 1 THEN v.id END) AS pick_id, MAX(v.id) AS newest_id
  FROM (
    SELECT t.platform_id, t.parent_id, t.id, t.wanted, t.is_1g1r_pick,
           COUNT(DISTINCT r.id) AS roms,
           COUNT(DISTINCT CASE WHEN f.state = 'verified'
                                 OR (r.present = 1
                                     AND COALESCE(t.mra_check, '') NOT IN ('mismatch', 'missing_part'))
                               THEN r.id END) AS roms_verified
    FROM titles t
    LEFT JOIN roms r ON r.title_id = t.id AND r.retired = 0
    LEFT JOIN files f ON f.rom_id = r.id
    WHERE t.retired = 0
    GROUP BY t.platform_id, t.parent_id, t.id
  ) v
  GROUP BY v.platform_id, v.parent_id
) g
JOIN titles p ON p.id = g.parent_id;";

const REFERENCE_WHERE: &str = "g.platform_id = ?1
    AND EXISTS (
      SELECT 1 FROM titles v WHERE v.parent_id = g.parent_id AND v.retired = 0
        AND NOT EXISTS (SELECT 1 FROM title_flags f
                        WHERE f.title_id = v.id AND f.flag IN (SELECT value FROM json_each(?2))))";

/// The median of `runs` timings of `f`, with its last result.
fn time<T>(runs: usize, mut f: impl FnMut() -> T) -> (Duration, T) {
    let mut times = Vec::with_capacity(runs);
    let mut last = None;
    for _ in 0..runs {
        let start = Instant::now();
        last = Some(f());
        times.push(start.elapsed());
    }
    times.sort();
    (times[runs / 2], last.expect("ran"))
}

fn ms(d: Duration) -> String {
    format!("{:.2}", d.as_secs_f64() * 1000.0)
}

fn hide() -> Vec<String> {
    HIDE.iter().map(|s| (*s).to_owned()).collect()
}

/// The default page and total of the reference query on `platform`.
fn reference_browse(c: &Connection, platform: &str) -> (Vec<i64>, u64) {
    let hidden = serde_json::to_string(&HIDE).expect("json");
    let total: i64 = c
        .query_row(
            &format!("SELECT COUNT(*) FROM reference_groups g WHERE {REFERENCE_WHERE}"),
            params![platform, hidden],
            |r| r.get(0),
        )
        .expect("reference total");
    let ids = c
        .prepare(&format!(
            "SELECT g.parent_id FROM reference_groups g WHERE {REFERENCE_WHERE}
             ORDER BY g.base_name COLLATE NOCASE, g.parent_id LIMIT 60"
        ))
        .expect("prepare")
        .query_map(params![platform, hidden], |r| r.get(0))
        .expect("query")
        .collect::<rusqlite::Result<_>>()
        .expect("rows");
    (ids, u64::try_from(total).expect("total"))
}

fn page(
    c: &Connection,
    platform: &str,
    q: &str,
    sort: Sort,
    shape: SearchShape,
) -> (Vec<i64>, u64) {
    let filter = Browse {
        q: Some(q.to_owned()).filter(|q| !q.is_empty()),
        hidden: hide(),
        sort,
        ..Browse::default()
    };
    let (rows, total) = titles::browse_with(c, platform, &filter, 60, 0, shape).expect("browse");
    (rows.into_iter().map(|r| r.parent_id.0).collect(), total)
}

/// A file database with the full catalogue and the board's reader cache size.
fn open(dir: &Path) -> Connection {
    let mut c = Connection::open(dir.join("browse.db")).expect("open");
    c.pragma_update(None, "journal_mode", "WAL").expect("wal");
    c.pragma_update(None, "cache_size", -1024).expect("cache");
    db::migrate::apply(&mut c).expect("migrate");
    db::platforms::seed(&mut c, &mistarr_mister::platforms::PLATFORMS).expect("seed");
    c
}

/// Every search the benchmark runs: rare, common trigrams, below trigram, common
/// elsewhere, and none.
fn terms() -> Vec<&'static str> {
    vec![RARE, "the", "sta", "man", "st", "an", ELSEWHERE, ""]
}

#[test]
fn browse_and_search_stay_fast_on_a_full_catalogue() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut c = open(dir.path());
    let start = Instant::now();
    let seeded = synth::seed(&mut c, 1.0, 1).expect("seed");
    eprintln!(
        "seeded {} titles, {} roms, {} files in {} ms",
        seeded.titles,
        seeded.roms,
        seeded.files,
        ms(start.elapsed())
    );
    let runs = if cfg!(debug_assertions) { 3 } else { 15 };

    c.execute_batch(REFERENCE).expect("reference query");
    let (reference, reference_page) = time(1, || reference_browse(&c, "psx"));
    let (new, new_page) = time(runs, || page(&c, "psx", "", Sort::Name, SEARCH_SHAPE));
    assert_eq!(new_page, reference_page);
    eprintln!(
        "default psx page and total: reference query {} ms, table {} ms",
        ms(reference),
        ms(new)
    );
    assert!(
        new * 5 < reference,
        "table {} against the reference query {}",
        ms(new),
        ms(reference)
    );
    for sort in [Sort::Have, Sort::Recent] {
        let (t, _) = time(runs, || page(&c, "psx", "", sort, SEARCH_SHAPE));
        eprintln!("  sort {sort:?}: {} ms", ms(t));
    }
    let (t, _) = time(runs, || titles::counts(&c, &hide()).expect("counts"));
    eprintln!("counts of every platform: {} ms", ms(t));

    let mut worst = [Duration::ZERO; SearchShape::ALL.len()];
    eprintln!(
        "platform term     groups  {}",
        SearchShape::ALL
            .map(|s| format!("{:>14}", s.name()))
            .join("")
    );
    for platform in BROWSED {
        for term in terms() {
            let mut line = String::new();
            let mut expected = None;
            for (i, shape) in SearchShape::ALL.into_iter().enumerate() {
                let (t, got) = time(runs, || page(&c, platform, term, Sort::Name, shape));
                let want = expected.get_or_insert_with(|| got.clone());
                assert_eq!(&got, want, "{platform} {term:?} {shape:?}");
                worst[i] = worst[i].max(t);
                line.push_str(&format!("{:>14}", ms(t)));
            }
            let total = expected.map_or(0, |e| e.1);
            eprintln!(
                "{platform:<8} {:<9} {total:>6}  {line}",
                format!("{term:?}")
            );
        }
    }
    let chosen = worst[SearchShape::ALL
        .iter()
        .position(|s| *s == SEARCH_SHAPE)
        .expect("chosen shape")];
    eprintln!(
        "worst case: {}; default {}",
        SearchShape::ALL
            .iter()
            .zip(worst)
            .map(|(s, w)| format!("{} {} ms", s.name(), ms(w)))
            .collect::<Vec<_>>()
            .join(", "),
        SEARCH_SHAPE.name()
    );
    // Host timings do not rank the shapes as the board does; only the absolute bound holds.
    assert!(
        chosen < Duration::from_millis(100),
        "worst search {} ms",
        ms(chosen)
    );
    assert!(groups::check(&c).expect("check").is_consistent());
}

/// A staged DAT of `count` games named like the synthetic catalogue.
fn stage(c: &Connection, count: usize) {
    let games: Vec<StagedGame> = synth::game_names(count, 9)
        .into_iter()
        .enumerate()
        .map(|(i, name)| {
            let parsed = parse_name(&name);
            StagedGame {
                base_name: parsed.base_name.clone(),
                group_key: group_key(&parsed),
                clone_of: None,
                regions: parsed.regions.iter().map(|r| r.name().to_owned()).collect(),
                languages: parsed.languages.clone(),
                revision: parsed.revision.as_ref().map(|r| r.label.clone()),
                flags: parsed.flag_labels(),
                roms: vec![StagedRom {
                    name: format!("{name}.sfc"),
                    size: 16,
                    crc32: Some(format!("{i:08x}")),
                    md5: None,
                    sha1: None,
                    status: "good".into(),
                    header: None,
                }],
                name,
            }
        })
        .collect();
    for chunk in games.chunks(2_000) {
        dat_stage::append(c, chunk).expect("stage");
    }
}

/// Applies the staged DAT as the import job does, in one transaction, and times it.
fn load(c: &mut Connection, maintained: bool) -> Duration {
    let start = Instant::now();
    let tx = c.transaction().expect("tx");
    let v = dats::NewVersion {
        dat_name: "Synthetic - Super",
        version: "1",
        source_file: "s.dat",
        platform: Some("snes"),
        now: 1,
    };
    let plan = dats::upsert_version(&tx, &v).expect("version");
    dats::begin_load(&tx, plan.id).expect("begin");
    dat_stage::apply(&tx, "snes", plan.id, v.dat_name).expect("apply");
    titles::link_parents(&tx, plan.id, false).expect("link");
    dats::retire_absent(&tx, plan.id).expect("retire");
    titles::recompute_platform(&tx, "snes", &mistarr_core::select::Prefs::default())
        .expect("recompute");
    dat_stage::clear(&tx).expect("clear");
    if maintained {
        db::commit(tx).expect("commit");
    } else {
        tx.commit().expect("commit");
    }
    start.elapsed()
}

/// The bytes a table and its indexes take, by dropping them from a copy and vacuuming.
fn size_of(dir: &Path, drop: &str) -> u64 {
    let copy = dir.join("size.db");
    std::fs::remove_file(&copy).ok();
    let c = Connection::open(dir.join("browse.db")).expect("open");
    c.execute("VACUUM INTO ?1", [copy.to_string_lossy()])
        .expect("copy");
    let before = std::fs::metadata(&copy).expect("size").len();
    let c = Connection::open(&copy).expect("open copy");
    c.execute_batch(&format!("{drop}; VACUUM;")).expect("drop");
    before - std::fs::metadata(&copy).expect("size").len()
}

#[test]
#[ignore = "measurement only; run with --ignored --nocapture in release"]
fn measure_dat_load_and_index_size() {
    let dir = tempfile::tempdir().expect("tempdir");
    let without_search = "DROP TRIGGER titles_insert_groups; DROP TRIGGER titles_rename_search;
        DROP TRIGGER titles_delete_groups;
        CREATE TRIGGER titles_insert_groups AFTER INSERT ON titles BEGIN
          INSERT INTO title_groups_dirty (parent_id)
            SELECT DISTINCT x FROM (SELECT NEW.parent_id AS x UNION SELECT NEW.id)
            WHERE x IS NOT NULL AND x NOT IN (SELECT parent_id FROM title_groups_dirty);
        END;";
    let unmaintained = "SELECT name FROM sqlite_master WHERE type = 'trigger'";
    for (label, setup, maintained) in [
        ("groups and search index", "", true),
        ("groups, no search index", without_search, true),
        ("neither", unmaintained, false),
    ] {
        let path = dir.path().join(format!("{}.db", label.len()));
        let mut c = Connection::open(&path).expect("open");
        db::migrate::apply(&mut c).expect("migrate");
        db::platforms::seed(&mut c, &mistarr_mister::platforms::PLATFORMS).expect("seed");
        if maintained {
            c.execute_batch(setup).expect("setup");
        } else {
            let triggers: Vec<String> = c
                .prepare(setup)
                .expect("prepare")
                .query_map([], |r| r.get(0))
                .expect("query")
                .collect::<rusqlite::Result<_>>()
                .expect("rows");
            for t in triggers {
                c.execute_batch(&format!("DROP TRIGGER {t}")).expect("drop");
            }
        }
        stage(&c, 15_000);
        let first = load(&mut c, maintained);
        stage(&c, 15_000);
        let again = load(&mut c, maintained);
        eprintln!(
            "15 000-game DAT, {label}: first load {} ms, reload {} ms",
            ms(first),
            ms(again)
        );
    }

    let mut c = open(dir.path());
    let seeded = synth::seed(&mut c, 1.0, 1).expect("seed");
    drop(c);
    let search = size_of(dir.path(), "DROP TABLE title_search");
    let summary = size_of(dir.path(), "DROP TABLE title_groups");
    let lists = size_of(
        dir.path(),
        "DROP TABLE title_flags; DROP TABLE title_regions; DROP TABLE title_languages",
    );
    let total = std::fs::metadata(dir.path().join("browse.db"))
        .expect("size")
        .len();
    eprintln!(
        "{} titles: database {} KiB, title_search {} KiB, title_groups {} KiB, flag, region and language tables {} KiB",
        seeded.titles,
        total / 1024,
        search / 1024,
        summary / 1024,
        lists / 1024
    );
}
