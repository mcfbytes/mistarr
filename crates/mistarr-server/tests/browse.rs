//! Browse and search speed on a synthetic catalogue the size of a large collection; see
//! `docs/TESTING.md` "Browse speed". Run with `--nocapture` to print the timings.

use std::path::Path;
use std::time::{Duration, Instant};

use mistarr_core::naming::{group_key, parse_name};
use mistarr_server::db::dat_stage::{self, StagedGame, StagedRom};
use mistarr_server::db::titles::{self, Browse, Sort};
use mistarr_server::db::{self, dats, groups};
use rusqlite::{params, Connection};

/// Titles per platform: one of 15 000 and five of 9 000, 60 000 in all.
const PLATFORMS: [(&str, usize); 6] = [
    ("nes", 15_000),
    ("snes", 9_000),
    ("gb", 9_000),
    ("gba", 9_000),
    ("megadrive", 9_000),
    ("psx", 9_000),
];

/// Synthetic words; none contains a search term below.
const WORDS: [&str; 24] = [
    "Amber", "Bolt", "Cinder", "Delta", "Ember", "Frost", "Glimmer", "Harbor", "Ivory", "Jolt",
    "Lumen", "Marble", "Nimbus", "Orbit", "Pebble", "Quill", "Ripple", "Summit", "Tundra", "Umber",
    "Vortex", "Willow", "Yonder", "Zephyr",
];

/// Default `prefs.hide`.
const HIDE: [&str; 6] = ["bios", "beta", "proto", "demo", "sample", "program"];

/// Search terms: one in a handful of titles, one in about a quarter of every platform,
/// and one common elsewhere but in three groups of the large platform.
const RARE: &str = "Quokka";
const COMMON: &str = "sta";
const ELSEWHERE: &str = "Kart";

/// The `title_groups` view the table replaced, with its per-request browse filter.
const OLD_VIEW: &str = "CREATE TEMP VIEW old_title_groups AS
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

const OLD_WHERE: &str = "g.platform_id = ?1
    AND (?2 IS NULL OR g.base_name LIKE ?2 ESCAPE '\\')
    AND EXISTS (
      SELECT 1 FROM titles v WHERE v.parent_id = g.parent_id AND v.retired = 0
        AND NOT EXISTS (SELECT 1 FROM title_flags f
                        WHERE f.title_id = v.id AND f.flag IN (SELECT value FROM json_each(?3))))
    AND (EXISTS (SELECT 1 FROM titles s WHERE s.id = g.parent_id AND s.source = 'mra')
         OR NOT EXISTS (SELECT 1 FROM titles m WHERE m.platform_id = g.platform_id
                        AND m.source = 'mra' AND m.retired = 0))";

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(n).expect("n")).expect("usize")
    }
}

/// The base name of group `g` of platform `p`.
fn base_name(rng: &mut Rng, p: usize, g: usize) -> String {
    let mut words = vec![WORDS[rng.below(WORDS.len())], WORDS[rng.below(WORDS.len())]];
    if rng.below(4) == 0 {
        words.push("Star");
    }
    if (p > 0 && rng.below(3) == 0) || (p == 0 && g < 3) {
        words.push(ELSEWHERE);
    }
    if g % 3_000 == 7 {
        words.push(RARE);
    }
    format!("{} {g}", words.join(" "))
}

/// Builds the catalogue in one transaction the way the importers would leave it: clone
/// groups of three regions, a beta in every 50th group, a BIOS-only group in every 200th,
/// one rom per title, and 60 000 files on the large platform.
fn build(conn: &mut Connection) -> Duration {
    let start = Instant::now();
    let tx = conn.transaction().expect("tx");
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for (p, (platform, count)) in PLATFORMS.iter().enumerate() {
        exec(
            &tx,
            "INSERT INTO dat_versions (platform_id, dat_name, version, source_file, loaded_at, game_count)
             VALUES (?1, ?1 || ' synthetic', '1', 'synthetic.dat', 0, ?2)",
            params![platform, i64::try_from(*count).expect("count")],
        );
        let version = tx.last_insert_rowid();
        let mut parent = 0;
        let mut base = String::new();
        for i in 0..*count {
            let g = i / 3;
            if i % 3 == 0 {
                base = base_name(&mut rng, p, g);
            }
            let region = ["USA", "Europe", "Japan"][i % 3];
            let mut flags: Vec<String> = Vec::new();
            if g % 200 == 199 {
                flags.push("bios".into());
            } else if g % 50 == 49 && i % 3 == 2 {
                flags.push("beta".into());
            }
            let name = format!("{base} ({region})");
            exec(
                &tx,
                "INSERT INTO titles (platform_id, dat_version_id, name, base_name, is_1g1r_pick, wanted)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![platform, version, name, base, i % 3 == 0, i % 97 == 0],
            );
            let id = tx.last_insert_rowid();
            if i % 3 == 0 {
                parent = id;
            }
            exec(
                &tx,
                "UPDATE titles SET parent_id = ?2 WHERE id = ?1",
                params![id, parent],
            );
            titles::set_flags(&tx, titles::TitleId(id), &flags).expect("flags");
            exec(
                &tx,
                "INSERT INTO title_regions (title_id, pos, region) VALUES (?1, 0, ?2)",
                params![id, region],
            );
            exec(
                &tx,
                "INSERT INTO roms (title_id, name, size, status) VALUES (?1, ?2, 16, 'good')",
                params![id, format!("{name}.bin")],
            );
            let rom = tx.last_insert_rowid();
            let files: &[(&str, bool)] = if p == 0 {
                &[
                    ("verified", true),
                    ("misnamed", true),
                    ("bad", true),
                    ("unverified", false),
                ]
            } else if i % 2 == 0 {
                &[("verified", true)]
            } else {
                &[]
            };
            for (k, (state, linked)) in files.iter().enumerate() {
                let state = if *state == "verified" && i % 5 == 4 {
                    "misnamed"
                } else {
                    state
                };
                exec(
                    &tx,
                    "INSERT INTO files (platform_id, rel_path, size, mtime, rom_id, state, scanned_at)
                     VALUES (?1, ?2, 16, 0, ?3, ?4, 0)",
                    params![platform, format!("{platform}/{id}-{k}.bin"), linked.then_some(rom), state],
                );
            }
        }
    }
    db::commit(tx).expect("commit");
    start.elapsed()
}

/// Runs `sql` through the statement cache.
fn exec(c: &Connection, sql: &str, args: impl rusqlite::Params) {
    c.prepare_cached(sql)
        .expect("prepare")
        .execute(args)
        .expect(sql);
}

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
    format!("{:.2} ms", d.as_secs_f64() * 1000.0)
}

fn hide() -> Vec<String> {
    HIDE.iter().map(|s| (*s).to_owned()).collect()
}

/// One page and the total of the old view on `platform`, as the old browse ran them.
fn old_browse(c: &Connection, platform: &str, q: Option<&str>) -> (Vec<i64>, i64) {
    let like = q.map(|q| format!("%{q}%"));
    let hidden = serde_json::to_string(&HIDE).expect("json");
    let total: i64 = c
        .query_row(
            &format!("SELECT COUNT(*) FROM old_title_groups g WHERE {OLD_WHERE}"),
            params![platform, like, hidden],
            |r| r.get(0),
        )
        .expect("old total");
    let ids = c
        .prepare(&format!(
            "SELECT g.parent_id FROM old_title_groups g LEFT JOIN titles k ON k.id = g.pick_id
             WHERE {OLD_WHERE} ORDER BY g.base_name COLLATE NOCASE, g.parent_id LIMIT 60"
        ))
        .expect("prepare")
        .query_map(params![platform, like, hidden], |r| r.get(0))
        .expect("query")
        .collect::<rusqlite::Result<_>>()
        .expect("rows");
    (ids, total)
}

fn new_browse(c: &Connection, platform: &str, q: Option<&str>, sort: Sort) -> (Vec<i64>, u64) {
    let filter = Browse {
        q: q.map(str::to_owned),
        hidden: hide(),
        sort,
        ..Browse::default()
    };
    let (rows, total) = titles::browse(c, platform, &filter, 60, 0).expect("browse");
    (rows.into_iter().map(|r| r.parent_id.0).collect(), total)
}

/// A search shape: the `FROM` and `WHERE` of one page and of its count.
struct Shape {
    name: &'static str,
    from: &'static str,
    filter: &'static str,
}

const SHAPES: [Shape; 3] = [
    Shape {
        name: "LIKE on the platform's names",
        from: "title_groups g",
        filter: "g.platform_id = ?2 AND g.base_name LIKE ?3 ESCAPE '\\' AND ?1 IS NOT NULL",
    },
    Shape {
        name: "(a) FTS drives rowid seeks",
        from: "title_search s CROSS JOIN title_groups g",
        filter: "s.title_search MATCH ?1 AND g.parent_id = s.rowid AND g.platform_id = ?2
                 AND g.base_name LIKE ?3 ESCAPE '\\'",
    },
    Shape {
        name: "(b) platform index probes FTS rowids",
        from: "title_groups g",
        filter: "g.platform_id = ?2
                 AND g.parent_id IN (SELECT rowid FROM title_search WHERE title_search MATCH ?1)
                 AND g.base_name LIKE ?3 ESCAPE '\\'",
    },
];

fn shape(c: &Connection, s: &Shape, term: &str) -> (Vec<i64>, i64) {
    let args = params![format!("\"{term}\""), "nes", format!("%{term}%")];
    let total: i64 = c
        .query_row(
            &format!("SELECT COUNT(*) FROM {} WHERE {}", s.from, s.filter),
            args,
            |r| r.get(0),
        )
        .expect("count");
    let ids = c
        .prepare(&format!(
            "SELECT g.parent_id FROM {} WHERE {} ORDER BY g.base_name COLLATE NOCASE, g.parent_id LIMIT 60",
            s.from, s.filter
        ))
        .expect("prepare")
        .query_map(args, |r| r.get(0))
        .expect("query")
        .collect::<rusqlite::Result<_>>()
        .expect("rows");
    (ids, total)
}

fn open(dir: &Path) -> Connection {
    let mut c = Connection::open(dir.join("browse.db")).expect("open");
    c.pragma_update(None, "journal_mode", "WAL").expect("wal");
    c.pragma_update(None, "cache_size", -1024).expect("cache");
    db::migrate::apply(&mut c).expect("migrate");
    db::platforms::seed(&mut c, &mistarr_mister::platforms::PLATFORMS).expect("seed");
    c
}

#[test]
fn browse_and_search_stay_fast_on_a_large_platform() {
    for w in WORDS {
        for term in [RARE, COMMON, ELSEWHERE] {
            assert!(
                !w.to_lowercase().contains(&term.to_lowercase()),
                "{w} contains {term}"
            );
        }
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let mut c = open(dir.path());
    let built = build(&mut c);
    c.execute_batch(OLD_VIEW).expect("old view");
    let groups_rows: i64 = c
        .query_row("SELECT COUNT(*) FROM title_groups", [], |r| r.get(0))
        .expect("groups");
    eprintln!(
        "built 60 000 titles, {groups_rows} groups, in {}",
        ms(built)
    );

    let (old, (old_ids, old_total)) = time(3, || old_browse(&c, "nes", None));
    let (new, (new_ids, new_total)) = time(9, || new_browse(&c, "nes", None, Sort::Name));
    assert_eq!(
        (new_ids, new_total),
        (old_ids, u64::try_from(old_total).expect("total"))
    );
    eprintln!(
        "default page and total on nes: old view {}, table {}",
        ms(old),
        ms(new)
    );
    for sort in [Sort::Have, Sort::Recent] {
        let (t, _) = time(9, || new_browse(&c, "nes", None, sort));
        eprintln!("  sort {sort:?}: {}", ms(t));
    }
    let (t, _) = time(9, || titles::counts(&c, &hide()).expect("counts"));
    eprintln!("counts of every platform: {}", ms(t));
    assert!(new * 5 < old, "table {} against view {}", ms(new), ms(old));
    assert!(
        new < Duration::from_millis(100),
        "default browse took {}",
        ms(new)
    );

    for term in [RARE, COMMON, ELSEWHERE] {
        let (old, (old_ids, old_total)) = time(3, || old_browse(&c, "nes", Some(term)));
        let (new, (new_ids, new_total)) = time(9, || new_browse(&c, "nes", Some(term), Sort::Name));
        assert_eq!(
            (new_ids, new_total),
            (old_ids, u64::try_from(old_total).expect("total"))
        );
        eprintln!(
            "search {term:?} ({new_total} groups): old view {}, browse {}",
            ms(old),
            ms(new)
        );
        let mut expected = None;
        for s in &SHAPES {
            let (t, got) = time(9, || shape(&c, s, term));
            expected.get_or_insert_with(|| got.clone());
            assert_eq!(Some(&got), expected.as_ref(), "{}", s.name);
            eprintln!("  {}: {}", s.name, ms(t));
        }
        assert!(
            new < Duration::from_millis(100),
            "search {term} took {}",
            ms(new)
        );
    }
    assert!(groups::check(&c).expect("check").is_consistent());
}

/// 15 000 staged games with clone groups and flags, for a timed DAT load.
fn stage(c: &Connection) {
    let mut rng = Rng(7);
    let games: Vec<StagedGame> = (0..15_000)
        .map(|i| {
            let region = ["USA", "Europe", "Japan"][i % 3];
            let tag = if i % 150 == 149 { " (Beta)" } else { "" };
            let name = format!("{} ({region}){tag}", base_name(&mut rng, 1, i / 3));
            let parsed = parse_name(&name);
            StagedGame {
                base_name: parsed.base_name.clone(),
                group_key: group_key(&parsed),
                clone_of: None,
                regions: parsed.regions.iter().map(|r| r.name().to_owned()).collect(),
                languages: parsed.languages.clone(),
                revision: None,
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
        stage(&c);
        let first = load(&mut c, maintained);
        stage(&c);
        let again = load(&mut c, maintained);
        eprintln!(
            "15 000-game DAT, {label}: first load {}, reload {}",
            ms(first),
            ms(again)
        );
    }

    let mut c = open(dir.path());
    build(&mut c);
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
        "60 000 titles: database {} KiB, title_search {} KiB, title_groups {} KiB, flag and region tables {} KiB",
        total / 1024,
        search / 1024,
        summary / 1024,
        lists / 1024
    );
}
