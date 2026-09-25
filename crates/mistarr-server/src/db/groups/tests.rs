use std::collections::HashMap;

use proptest::prelude::*;
use rusqlite::params;

use super::*;
use mistarr_core::select::Prefs;

use crate::db::dats::{self, DatVersionId};
use crate::db::titles::{self, Browse, Counts, GroupRow, SearchShape, Sort, TitleId, Tri};

/// The reference aggregation query for `title_groups`, as a view the table must equal.
const REFERENCE: &str = "CREATE TEMP VIEW reference_groups AS
SELECT g.parent_id, g.platform_id, p.base_name, p.name,
       g.variants, g.have_verified, g.wanted, g.has_pick, g.pick_id, g.newest_id
FROM (
  SELECT v.platform_id, v.parent_id,
         COUNT(*) AS variants,
         SUM(v.roms > 0 AND v.roms_verified = v.roms) AS have_verified,
         SUM(v.wanted) AS wanted,
         MAX(v.is_1g1r_pick) AS has_pick,
         MAX(CASE WHEN v.is_1g1r_pick = 1 THEN v.id END) AS pick_id,
         MAX(v.id) AS newest_id
  FROM (
    SELECT t.platform_id, t.group_root AS parent_id, t.id, t.wanted, t.is_1g1r_pick,
           COUNT(DISTINCT r.id) AS roms,
           COUNT(DISTINCT CASE WHEN f.state = 'verified'
                                 OR (r.present = 1
                                     AND COALESCE(t.mra_check, '') NOT IN ('mismatch', 'missing_part'))
                               THEN r.id END) AS roms_verified
    FROM titles t
    LEFT JOIN roms r ON r.title_id = t.id AND r.retired = 0
    LEFT JOIN files f ON f.rom_id = r.id
    WHERE t.retired = 0
    GROUP BY t.platform_id, t.group_root, t.id
  ) v
  GROUP BY v.platform_id, v.parent_id
) g
JOIN titles p ON p.id = g.parent_id;";

/// The reference browse filter, over the reference aggregation query, reading each
/// variant's flags and regions from their tables where it read the JSON columns.
const REFERENCE_BROWSE_WHERE: &str = "
    g.platform_id = ?1
    AND (?2 IS NULL OR g.base_name LIKE ?2 ESCAPE '\\')
    AND (?3 = 'any' OR (?3 = 'yes') = (g.have_verified > 0))
    AND (?4 = 'any' OR (?4 = 'yes') = (g.wanted > 0))
    AND EXISTS (
      SELECT 1 FROM titles v WHERE v.group_root = g.parent_id AND v.retired = 0
        AND NOT EXISTS (SELECT 1 FROM (SELECT flag AS value FROM title_flags WHERE title_id = v.id) f
                        WHERE f.value IN (SELECT value FROM json_each(?5)))
        AND (?6 IS NULL OR EXISTS (SELECT 1 FROM (SELECT region AS value FROM title_regions
                                                  WHERE title_id = v.id) r
                                   WHERE r.value = ?6 COLLATE NOCASE))
        AND NOT EXISTS (SELECT 1 FROM json_each(?7) w
                        WHERE NOT EXISTS (SELECT 1 FROM (SELECT flag AS value FROM title_flags
                                                         WHERE title_id = v.id) f
                                          WHERE f.value = w.value)))";

const REFERENCE_MRA_ONLY: &str =
    "(EXISTS (SELECT 1 FROM titles s WHERE s.id = g.parent_id AND s.source = 'mra')
    OR NOT EXISTS (SELECT 1 FROM titles m WHERE m.platform_id = g.platform_id
                   AND m.source = 'mra' AND m.retired = 0))";

fn conn() -> Connection {
    let mut c = Connection::open_in_memory().expect("open");
    crate::db::migrate::apply(&mut c).expect("migrate");
    crate::db::platforms::seed(&mut c, &mistarr_mister::platforms::PLATFORMS).expect("seed");
    c.pragma_update(None, "foreign_keys", false).expect("fk");
    c.execute_batch(REFERENCE).expect("reference");
    c.execute(
        "INSERT INTO dat_versions (id, platform_id, dat_name, version, source_file, loaded_at, game_count)
         VALUES (1, 'gb', 'Test', '1', 't.dat', 0, 0)",
        [],
    )
    .expect("version");
    c
}

/// [`conn`] with a small synthetic catalogue for random writes to land among.
fn seeded_conn() -> Connection {
    let mut c = conn();
    crate::synth::seed(&mut c, 0.002, 11).expect("catalogue");
    // Groups split across platforms from the start, so every search shape meets them.
    let tx = c.transaction().expect("tx");
    for (from, to) in [("nes", "gb"), ("gb", "nes"), ("psx", "nes")] {
        tx.execute(
            "UPDATE titles SET platform_id = ?2 WHERE id = (SELECT MIN(group_root) FROM titles
             WHERE platform_id = ?1 AND group_root <> id AND retired = 0)",
            [from, to],
        )
        .expect("split");
    }
    crate::db::commit(tx).expect("commit");
    let split: i64 = c
        .query_row(
            "SELECT COUNT(DISTINCT platform_id) FROM title_groups WHERE split",
            [],
            |r| r.get(0),
        )
        .expect("split");
    assert!(split >= 2, "{split}");
    c
}

type Summary = (
    i64,
    String,
    String,
    String,
    i64,
    i64,
    i64,
    i64,
    Option<i64>,
    i64,
);

fn rows(c: &Connection, from: &str) -> Vec<Summary> {
    c.prepare(&format!(
        "SELECT parent_id, platform_id, base_name, name, variants, have_verified, wanted,
                has_pick, pick_id, newest_id FROM {from} ORDER BY parent_id, platform_id"
    ))
    .expect("prepare")
    .query_map([], |r| {
        Ok((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
            r.get(6)?,
            r.get(7)?,
            r.get(8)?,
            r.get(9)?,
        ))
    })
    .expect("query")
    .collect::<rusqlite::Result<_>>()
    .expect("rows")
}

fn json(xs: &[String]) -> String {
    serde_json::to_string(xs).expect("json")
}

fn like(q: &str) -> String {
    let mut out = String::from("%");
    for ch in q.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('%');
    out
}

/// The reference browse query, run against the reference aggregation query.
fn reference_browse(
    c: &Connection,
    platform: &str,
    f: &Browse,
    limit: u32,
    offset: u32,
) -> (Vec<GroupRow>, u64) {
    let q =
        f.q.as_deref()
            .map(str::trim)
            .filter(|q| !q.is_empty())
            .map(like);
    let tri = |t: Tri| match t {
        Tri::Any => "any",
        Tri::Yes => "yes",
        Tri::No => "no",
    };
    let args = params![
        platform,
        q,
        tri(f.have),
        tri(f.wanted),
        json(&f.hidden),
        f.region,
        json(&f.flags),
        limit,
        offset
    ];
    let total: i64 = c
        .query_row(
            &format!("SELECT COUNT(*) FROM reference_groups g WHERE {REFERENCE_BROWSE_WHERE} AND {REFERENCE_MRA_ONLY}"),
            &args[..7],
            |r| r.get(0),
        )
        .expect("reference total");
    let order = match f.sort {
        Sort::Name => "g.base_name COLLATE NOCASE, g.parent_id",
        Sort::Have => "g.have_verified > 0 DESC, g.base_name COLLATE NOCASE, g.parent_id",
        Sort::Recent => "g.newest_id DESC",
    };
    let items = c
        .prepare(&format!(
            "SELECT g.parent_id, g.platform_id, g.base_name, g.name, g.pick_id, k.name,
                    g.variants, g.have_verified, g.wanted, g.has_pick,
                    NOT EXISTS (SELECT 1 FROM titles v WHERE v.group_root = g.parent_id
                        AND v.retired = 0 AND NOT EXISTS (SELECT 1 FROM title_flags f
                            WHERE f.title_id = v.id AND f.flag = 'bios'))
             FROM reference_groups g LEFT JOIN titles k ON k.id = g.pick_id
             WHERE {REFERENCE_BROWSE_WHERE} AND {REFERENCE_MRA_ONLY} ORDER BY {order} LIMIT ?8 OFFSET ?9"
        ))
        .expect("prepare")
        .query_map(args, |r| {
            Ok(GroupRow {
                parent_id: TitleId(r.get(0)?),
                platform_id: mistarr_core::PlatformId(r.get(1)?),
                base_name: r.get(2)?,
                name: r.get(3)?,
                pick_id: r.get::<_, Option<i64>>(4)?.map(TitleId),
                pick_name: r.get(5)?,
                variants: u64::try_from(r.get::<_, i64>(6)?).unwrap_or(0),
                have_verified: u64::try_from(r.get::<_, i64>(7)?).unwrap_or(0),
                wanted: u64::try_from(r.get::<_, i64>(8)?).unwrap_or(0),
                has_pick: r.get(9)?,
                bios: r.get(10)?,
            })
        })
        .expect("query")
        .collect::<rusqlite::Result<_>>()
        .expect("rows");
    (items, u64::try_from(total).unwrap_or(0))
}

/// The reference per-platform counts, without the unverified-file count.
fn reference_counts(c: &Connection, hidden: &[String]) -> HashMap<String, (u64, u64, u64)> {
    let mut stmt = c
        .prepare(&format!(
            "SELECT g.platform_id, COUNT(*), SUM(g.have_verified > 0), SUM(g.wanted > 0)
             FROM reference_groups g
             WHERE {REFERENCE_MRA_ONLY} AND EXISTS (
               SELECT 1 FROM titles v WHERE v.group_root = g.parent_id AND v.retired = 0
                 AND NOT EXISTS (SELECT 1 FROM title_flags f
                                 WHERE f.title_id = v.id AND f.flag IN (SELECT value FROM json_each(?1))))
             GROUP BY g.platform_id"
        ))
        .expect("prepare");
    stmt.query_map([json(hidden)], |r| {
        let n = |i: usize| r.get::<_, i64>(i).map(|v| u64::try_from(v).unwrap_or(0));
        Ok((r.get(0)?, (n(1)?, n(2)?, n(3)?)))
    })
    .expect("query")
    .collect::<rusqlite::Result<_>>()
    .expect("rows")
}

fn new_counts(c: &Connection, hidden: &[String]) -> HashMap<String, (u64, u64, u64)> {
    titles::counts(c, hidden)
        .expect("counts")
        .into_iter()
        .filter(|(_, v)| v.titles > 0)
        .map(
            |(
                k,
                Counts {
                    titles,
                    have,
                    wanted,
                    ..
                },
            )| (k, (titles, have, wanted)),
        )
        .collect()
}

const PLATFORMS: [&str; 3] = ["gb", "nes", "arcade"];
const FLAGS: [&str; 6] = [
    "bios",
    "beta",
    "proto",
    "unl",
    "other:Aftermarket",
    "disc:1",
];
const REGIONS: [&str; 6] = ["USA", "Europe", "Japan", "usa", "Atlantis", "Hong Kong"];
const BASES: [&str; 6] = [
    "Example Quest",
    "example quest",
    "Other Tale",
    "Zeta 100%",
    "Alpha_Beta",
    "Échelle",
];
const CHECKS: [Option<&str>; 4] = [None, Some("match"), Some("mismatch"), Some("missing_part")];
const STATES: [&str; 5] = ["verified", "unverified", "misnamed", "bad", "pending"];

fn pick(pool: &[&str], mask: u8) -> Vec<String> {
    pool.iter()
        .enumerate()
        .filter(|(i, _)| mask & (1 << i) != 0)
        .map(|(_, s)| (*s).to_owned())
        .collect()
}

/// Asserts the table, the bits and every browse and count equal the reference query.
fn assert_matches_reference(c: &Connection) {
    assert_eq!(rows(c, "title_groups"), rows(c, "reference_groups"));
    assert_eq!(check(c).expect("check"), Drift::default());
    let lists = |xs: &[&[&str]]| -> Vec<Vec<String>> {
        xs.iter()
            .map(|l| l.iter().map(|s| (*s).to_owned()).collect())
            .collect()
    };
    let hiddens = lists(&[
        &[],
        &["bios", "beta", "proto", "demo", "sample", "program"],
        &["bios", "beta"],
        &["bios", "other:Aftermarket"],
        &["proto"],
    ]);
    let requireds = lists(&[
        &[],
        &["beta"],
        &["bios"],
        &["other:Aftermarket"],
        &["beta", "disc:1"],
    ]);
    let regions = [
        None,
        Some("USA"),
        Some("europe"),
        Some("Atlantis"),
        Some("hong kong"),
        Some("Nowhere"),
    ];
    let qs = [
        None,
        Some("quest"),
        Some("%"),
        Some("a_"),
        Some("É"),
        Some("sta"),
        Some("an"),
    ];
    let tris = [Tri::Any, Tri::Yes, Tri::No];
    let sorts = [Sort::Name, Sort::Have, Sort::Recent];
    let mut n = 0usize;
    for hidden in &hiddens {
        assert_eq!(
            new_counts(c, hidden),
            reference_counts(c, hidden),
            "counts {hidden:?}"
        );
        for flags in &requireds {
            for region in regions {
                n += 1;
                let f = Browse {
                    q: qs[n % qs.len()].map(str::to_owned),
                    have: tris[n % 3],
                    wanted: tris[(n / 3) % 3],
                    region: region.map(str::to_owned),
                    flags: flags.clone(),
                    hidden: hidden.clone(),
                    sort: sorts[(n / 2) % 3],
                };
                let platform = PLATFORMS[n % PLATFORMS.len()];
                let (limit, offset) = if n.is_multiple_of(4) { (3, 1) } else { (50, 0) };
                let want = reference_browse(c, platform, &f, limit, offset);
                for shape in SearchShape::ALL {
                    let got =
                        titles::browse_with(c, platform, &f, limit, offset, shape).expect("browse");
                    assert_eq!(got, want, "{platform} {shape:?} {f:?}");
                }
                let unfiltered = Browse {
                    q: None,
                    have: Tri::Any,
                    wanted: Tri::Any,
                    ..f
                };
                let got = titles::browse(c, platform, &unfiltered, 50, 0).expect("browse");
                assert_eq!(
                    got,
                    reference_browse(c, platform, &unfiltered, 50, 0),
                    "{platform} {unfiltered:?}"
                );
            }
        }
    }
}

/// One random write to an input of `title_groups`.
#[derive(Debug, Clone)]
enum Op {
    AddTitle {
        platform: usize,
        base: usize,
        flags: u8,
        regions: u8,
        parent: Option<usize>,
        mra: bool,
        retired: u8,
        wanted: bool,
    },
    SetParent {
        title: usize,
        parent: usize,
    },
    Link {
        title: usize,
        root: usize,
    },
    Recompute {
        platform: usize,
    },
    Retire {
        title: usize,
        retired: u8,
    },
    Want {
        title: usize,
        wanted: bool,
    },
    Pick {
        title: usize,
        pick: bool,
    },
    Check {
        title: usize,
        check: usize,
    },
    Rename {
        title: usize,
        base: usize,
    },
    Tags {
        title: usize,
        flags: u8,
        regions: u8,
    },
    Move {
        title: usize,
        platform: usize,
    },
    Source {
        title: usize,
        mra: bool,
    },
    DeleteTitle {
        title: usize,
    },
    AddRom {
        title: usize,
        retired: bool,
        present: bool,
    },
    SetRom {
        rom: usize,
        retired: bool,
        present: bool,
    },
    MoveRom {
        rom: usize,
        title: usize,
    },
    DeleteRom {
        rom: usize,
    },
    AddFile {
        rom: Option<usize>,
        state: usize,
    },
    SetFile {
        file: usize,
        state: usize,
        rom: Option<usize>,
    },
    DeleteFile {
        file: usize,
    },
    Commit,
}

fn op() -> impl Strategy<Value = Op> {
    let i = || 0usize..64;
    prop_oneof![
        4 => (0..3usize, 0..6usize, any::<u8>(), any::<u8>(), proptest::option::of(i()), proptest::bool::weighted(0.2), 0..3u8, any::<bool>())
            .prop_map(|(platform, base, flags, regions, parent, mra, retired, wanted)| Op::AddTitle {
                platform, base, flags: flags & 0x3f, regions: regions & 0x3f, parent, mra, retired, wanted
            }),
        2 => (i(), i()).prop_map(|(title, parent)| Op::SetParent { title, parent }),
        2 => (i(), i()).prop_map(|(title, root)| Op::Link { title, root }),
        1 => (0..3usize).prop_map(|platform| Op::Recompute { platform }),
        1 => (i(), 0..3u8).prop_map(|(title, retired)| Op::Retire { title, retired }),
        1 => (i(), any::<bool>()).prop_map(|(title, wanted)| Op::Want { title, wanted }),
        1 => (i(), any::<bool>()).prop_map(|(title, pick)| Op::Pick { title, pick }),
        1 => (i(), 0..4usize).prop_map(|(title, check)| Op::Check { title, check }),
        1 => (i(), 0..6usize).prop_map(|(title, base)| Op::Rename { title, base }),
        1 => (i(), any::<u8>(), any::<u8>()).prop_map(|(title, flags, regions)| Op::Tags { title, flags: flags & 0x3f, regions: regions & 0x3f }),
        1 => (i(), 0..3usize).prop_map(|(title, platform)| Op::Move { title, platform }),
        1 => (i(), any::<bool>()).prop_map(|(title, mra)| Op::Source { title, mra }),
        1 => i().prop_map(|title| Op::DeleteTitle { title }),
        4 => (i(), proptest::bool::weighted(0.2), any::<bool>()).prop_map(|(title, retired, present)| Op::AddRom { title, retired, present }),
        1 => (i(), any::<bool>(), any::<bool>()).prop_map(|(rom, retired, present)| Op::SetRom { rom, retired, present }),
        1 => (i(), i()).prop_map(|(rom, title)| Op::MoveRom { rom, title }),
        1 => i().prop_map(|rom| Op::DeleteRom { rom }),
        4 => (proptest::option::weighted(0.9, i()), 0..5usize).prop_map(|(rom, state)| Op::AddFile { rom, state }),
        2 => (i(), 0..5usize, proptest::option::weighted(0.9, i())).prop_map(|(file, state, rom)| Op::SetFile { file, state, rom }),
        1 => i().prop_map(|file| Op::DeleteFile { file }),
        2 => Just(Op::Commit),
    ]
}

/// The id of the `k`th row of `table` modulo its size.
fn nth(c: &Connection, table: &str, k: usize) -> Option<i64> {
    let n: i64 = c
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .expect("count");
    if n == 0 {
        return None;
    }
    let k = i64::try_from(k).expect("k") % n;
    c.query_row(
        &format!("SELECT id FROM {table} ORDER BY id LIMIT 1 OFFSET ?1"),
        [k],
        |r| r.get(0),
    )
    .ok()
}

#[allow(clippy::too_many_lines)] // One arm per kind of write.
fn apply(c: &Connection, op: &Op, seq: &mut u32) {
    let title = |k| nth(c, "titles", k);
    let rom = |k| nth(c, "roms", k);
    let run = |sql: &str, p: &[&dyn rusqlite::ToSql]| {
        c.execute(sql, p).expect(sql);
    };
    match *op {
        Op::AddTitle {
            platform,
            base,
            flags,
            regions,
            parent,
            mra,
            retired,
            wanted,
        } => {
            *seq += 1;
            let parent = parent.and_then(title);
            run(
                "INSERT INTO titles (platform_id, dat_version_id, name, base_name, source, retired,
                   wanted) VALUES (?1, 1, ?2, ?3, ?4, ?5, ?6)",
                &[
                    &PLATFORMS[platform],
                    &format!("{} {seq}", BASES[base]),
                    &BASES[base],
                    &if mra { "mra" } else { "dat" },
                    &retired,
                    &wanted,
                ],
            );
            let id = c.last_insert_rowid();
            run(
                "UPDATE titles SET parent_id = COALESCE(?2, id) WHERE id = ?1",
                &[&id, &parent],
            );
            titles::store_lists(c, id, &pick(&REGIONS, regions), &[], &pick(&FLAGS, flags))
                .expect("lists");
        }
        Op::SetParent { title: t, parent } => {
            if let (Some(t), Some(p)) = (title(t), title(parent)) {
                run("UPDATE titles SET parent_id = ?2 WHERE id = ?1", &[&t, &p]);
            }
        }
        Op::Link { title: t, root } => {
            if let (Some(t), Some(r)) = (title(t), title(root)) {
                run("UPDATE titles SET group_root = ?2 WHERE id = ?1", &[&t, &r]);
            }
        }
        Op::Recompute { platform } => {
            titles::recompute_platform(c, PLATFORMS[platform], &Prefs::default())
                .expect("recompute");
        }
        Op::Retire { title: t, retired } => {
            if let Some(t) = title(t) {
                run(
                    "UPDATE titles SET retired = ?2 WHERE id = ?1",
                    &[&t, &retired],
                );
            }
        }
        Op::Want { title: t, wanted } => {
            if let Some(t) = title(t) {
                run(
                    "UPDATE titles SET wanted = ?2 WHERE id = ?1",
                    &[&t, &wanted],
                );
            }
        }
        Op::Pick { title: t, pick } => {
            if let Some(t) = title(t) {
                run(
                    "UPDATE titles SET is_1g1r_pick = ?2 WHERE id = ?1",
                    &[&t, &pick],
                );
            }
        }
        Op::Check { title: t, check: k } => {
            if let Some(t) = title(t) {
                run(
                    "UPDATE titles SET mra_check = ?2 WHERE id = ?1",
                    &[&t, &CHECKS[k]],
                );
            }
        }
        Op::Rename { title: t, base } => {
            if let Some(t) = title(t) {
                run(
                    "UPDATE titles SET base_name = ?2, name = ?2 || ' #' || id WHERE id = ?1",
                    &[&t, &BASES[base]],
                );
            }
        }
        Op::Tags {
            title: t,
            flags,
            regions,
        } => {
            if let Some(t) = title(t) {
                titles::store_lists(c, t, &pick(&REGIONS, regions), &[], &pick(&FLAGS, flags))
                    .expect("lists");
            }
        }
        Op::Move { title: t, platform } => {
            if let Some(t) = title(t) {
                run(
                    "UPDATE titles SET platform_id = ?2 WHERE id = ?1",
                    &[&t, &PLATFORMS[platform]],
                );
            }
        }
        Op::Source { title: t, mra } => {
            if let Some(t) = title(t) {
                run(
                    "UPDATE titles SET source = ?2 WHERE id = ?1",
                    &[&t, &if mra { "mra" } else { "dat" }],
                );
            }
        }
        Op::DeleteTitle { title: t } => {
            if let Some(t) = title(t) {
                run("DELETE FROM titles WHERE id = ?1", &[&t]);
            }
        }
        Op::AddRom {
            title: t,
            retired,
            present,
        } => {
            if let Some(t) = title(t) {
                *seq += 1;
                run(
                    "INSERT INTO roms (title_id, name, size, retired, present) VALUES (?1, ?2, 4, ?3, ?4)",
                    &[&t, &format!("r{seq}"), &retired, &present],
                );
            }
        }
        Op::SetRom {
            rom: r,
            retired,
            present,
        } => {
            if let Some(r) = rom(r) {
                run(
                    "UPDATE roms SET retired = ?2, present = ?3 WHERE id = ?1",
                    &[&r, &retired, &present],
                );
            }
        }
        Op::MoveRom { rom: r, title: t } => {
            if let (Some(r), Some(t)) = (rom(r), title(t)) {
                run(
                    "UPDATE OR IGNORE roms SET title_id = ?2 WHERE id = ?1",
                    &[&r, &t],
                );
            }
        }
        Op::DeleteRom { rom: r } => {
            if let Some(r) = rom(r) {
                run("DELETE FROM roms WHERE id = ?1", &[&r]);
            }
        }
        Op::AddFile { rom: r, state } => {
            *seq += 1;
            let r = r.and_then(rom);
            run(
                "INSERT INTO files (platform_id, rel_path, size, mtime, rom_id, state, scanned_at)
                 VALUES ('gb', ?1, 4, 0, ?2, ?3, 0)",
                &[&format!("f{seq}"), &r, &STATES[state]],
            );
        }
        Op::SetFile {
            file,
            state,
            rom: r,
        } => {
            if let Some(f) = nth(c, "files", file) {
                let r = r.and_then(rom);
                run(
                    "UPDATE files SET state = ?2, rom_id = ?3 WHERE id = ?1",
                    &[&f, &STATES[state], &r],
                );
            }
        }
        Op::DeleteFile { file } => {
            if let Some(f) = nth(c, "files", file) {
                run("DELETE FROM files WHERE id = ?1", &[&f]);
            }
        }
        Op::Commit => {}
    }
}

/// Runs `ops`, committing through [`crate::db::commit`] at each `Commit` and at the end,
/// and compares everything with the reference query after every commit.
fn run_ops(ops: &[Op]) {
    let mut c = seeded_conn();
    let mut seq = 0;
    for batch in ops.split(|o| matches!(o, Op::Commit)) {
        let tx = c.transaction().expect("tx");
        for op in batch {
            apply(&tx, op, &mut seq);
        }
        crate::db::commit(tx).expect("commit");
        assert!(!pending(&c).expect("pending"));
        assert_matches_reference(&c);
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 32,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn random_writes_keep_the_table_equal_to_the_reference_query(ops in proptest::collection::vec(op(), 1..60)) {
        run_ops(&ops);
    }
}

#[test]
fn a_fixed_sequence_covers_every_write() {
    let mut ops = Vec::new();
    for i in 0..12 {
        ops.push(Op::AddTitle {
            platform: i % 2,
            base: i % 6,
            flags: u8::try_from(i % 7).expect("u8") * 5 % 64,
            regions: u8::try_from(i).expect("u8") * 3 % 64,
            parent: (i % 3 == 0).then_some(0),
            mra: i == 7,
            retired: 0,
            wanted: i % 4 == 0,
        });
        ops.push(Op::AddRom {
            title: i,
            retired: false,
            present: i % 5 == 0,
        });
        ops.push(Op::AddFile {
            rom: Some(i),
            state: i % 5,
        });
    }
    ops.extend([
        Op::Commit,
        Op::Pick {
            title: 3,
            pick: true,
        },
        Op::Check { title: 5, check: 2 },
        Op::Tags {
            title: 4,
            flags: 1,
            regions: 16,
        },
        Op::Commit,
        Op::SetParent {
            title: 5,
            parent: 1,
        },
        Op::Move {
            title: 2,
            platform: 2,
        },
        Op::Source {
            title: 0,
            mra: true,
        },
        Op::Rename { title: 0, base: 3 },
        Op::Commit,
        Op::SetFile {
            file: 1,
            state: 0,
            rom: Some(4),
        },
        Op::MoveRom { rom: 2, title: 9 },
        Op::SetRom {
            rom: 3,
            retired: true,
            present: true,
        },
        Op::Retire {
            title: 6,
            retired: 2,
        },
        Op::DeleteFile { file: 0 },
        Op::DeleteRom { rom: 5 },
        Op::DeleteTitle { title: 0 },
        Op::Want {
            title: 8,
            wanted: true,
        },
    ]);
    run_ops(&ops);
}

#[test]
fn a_large_transaction_refreshes_in_chunks() {
    let mut c = conn();
    let tx = c.transaction().expect("tx");
    let mut seq = 0;
    for i in 0..usize::try_from(2 * FLUSH_CHUNK + 50).expect("usize") {
        apply(
            &tx,
            &Op::AddTitle {
                platform: i % 2,
                base: i % 6,
                flags: u8::try_from(i % 64).expect("u8"),
                regions: u8::try_from(i * 7 % 64).expect("u8"),
                parent: (i % 4 == 0).then_some(i / 2),
                mra: false,
                retired: 0,
                wanted: i % 9 == 0,
            },
            &mut seq,
        );
    }
    let dirty: i64 = tx
        .query_row("SELECT COUNT(*) FROM title_groups_dirty", [], |r| r.get(0))
        .expect("dirty");
    assert!(dirty > 2 * FLUSH_CHUNK);
    crate::db::commit(tx).expect("commit");
    assert_eq!(rows(&c, "title_groups"), rows(&c, "reference_groups"));
    assert!(check(&c).expect("check").is_consistent());
}

#[test]
fn refresh_rebuild_and_check_agree() {
    let mut c = conn();
    let mut seq = 0;
    for i in 0..6 {
        apply(
            &c,
            &Op::AddTitle {
                platform: 0,
                base: i,
                flags: 0,
                regions: 1,
                parent: None,
                mra: false,
                retired: 0,
                wanted: false,
            },
            &mut seq,
        );
    }
    assert!(pending(&c).expect("pending"));
    assert_eq!(flush(&c).expect("flush"), 6);
    assert!(check(&c).expect("check").is_consistent());
    c.execute("DELETE FROM title_groups", []).expect("empty");
    let ids: Vec<TitleId> = (1..=6).map(TitleId).collect();
    assert_eq!(refresh_groups(&c, &ids).expect("refresh"), 6);
    assert!(check(&c).expect("check").is_consistent());

    c.execute("UPDATE title_groups SET wanted = 7 WHERE parent_id = 1", [])
        .expect("corrupt");
    c.execute(
        "UPDATE title_groups SET flag_union = 5 WHERE parent_id = 3",
        [],
    )
    .expect("corrupt");
    c.execute(
        "INSERT INTO title_search (title_search, rowid, base_name)
         SELECT 'delete', id, base_name FROM titles WHERE id = 4",
        [],
    )
    .expect("corrupt search");
    let drift = check(&c).expect("check");
    assert_eq!(
        (drift.stale, drift.missing, drift.search),
        (2, 2, true),
        "{drift:?}"
    );
    let tx = c.transaction().expect("tx");
    assert_eq!(rebuild(&tx).expect("rebuild"), 6);
    crate::db::commit(tx).expect("commit");
    assert!(check(&c).expect("check").is_consistent());
}

#[test]
fn bits_follow_the_known_tables() {
    let c = conn();
    let flags: Vec<(String, i64)> = c
        .prepare("SELECT name, bit FROM known_flags ORDER BY bit")
        .expect("prepare")
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .collect::<rusqlite::Result<_>>()
        .expect("rows");
    let want: Vec<(String, i64)> = KNOWN_FLAGS
        .iter()
        .map(|f| ((*f).to_owned(), flag_bit(f).expect("bit")))
        .collect();
    assert_eq!(flags, want);
    let regions: Vec<(String, i64)> = c
        .prepare("SELECT name, bit FROM known_regions ORDER BY bit")
        .expect("prepare")
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .collect::<rusqlite::Result<_>>()
        .expect("rows");
    let want: Vec<(String, i64)> = KNOWN_REGIONS
        .iter()
        .map(|r| ((*r).to_owned(), region_bit(r).expect("bit")))
        .collect();
    assert_eq!(regions, want);
    for r in KNOWN_REGIONS {
        let got = mistarr_core::naming::Region::from_name(r).map(|x| x.name().to_owned());
        assert_eq!(got.as_deref(), Some(r), "{r} is a canonical region name");
    }

    let mut c = c;
    let mut seq = 0;
    let tx = c.transaction().expect("tx");
    apply(
        &tx,
        &Op::AddTitle {
            platform: 0,
            base: 0,
            flags: 0b11_0001,
            regions: 0b01_1001,
            parent: None,
            mra: false,
            retired: 0,
            wanted: false,
        },
        &mut seq,
    );
    crate::db::commit(tx).expect("commit");
    let summary = |c: &Connection| -> (i64, i64, i64, i64) {
        c.query_row(
            "SELECT lean_flags, unflagged_regions, flag_union, region_union FROM title_groups",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .expect("summary")
    };
    assert_eq!(summary(&c), (8, 0, 8 | OTHER, 2 | OTHER));
    let tx = c.transaction().expect("tx");
    titles::store_lists(&tx, 1, &["usa".to_owned()], &[], &["other:x".to_owned()]).expect("lists");
    crate::db::commit(tx).expect("commit");
    assert_eq!(summary(&c), (0, 2, OTHER, 2));
}

#[test]
fn the_default_hide_list_is_the_high_flag_bits() {
    let hide = crate::config::PrefsConfig::default().hide;
    let bits = hide
        .iter()
        .map(|f| flag_bit(f).expect("known"))
        .fold(0, |a, b| a | b);
    assert_eq!(bits, HIDDEN_BY_DEFAULT);
    let low = KNOWN_FLAGS
        .iter()
        .map(|f| flag_bit(f).expect("bit"))
        .filter(|b| b & HIDDEN_BY_DEFAULT == 0);
    assert!(low.into_iter().all(|b| b < 8));
    let mut c = Clause::default();
    visible(&mut c, &hide, None, &[]);
    assert_eq!(c.sql(), format!("(g.lean_flags & {HIDDEN_BY_DEFAULT} = 0)"));
}

#[test]
fn a_group_whose_parent_is_on_another_platform_is_found_by_every_shape() {
    let mut c = conn();
    let tx = c.transaction().expect("tx");
    tx.execute_batch(
        "INSERT INTO titles (id, platform_id, dat_version_id, name, base_name, parent_id)
           VALUES (1, 'gb', 1, 'Starla (USA)', 'Starla', 1),
                  (2, 'nes', 1, 'Starla (Japan)', 'Starla', 1),
                  (3, 'nes', 1, 'Other (USA)', 'Other', 3);",
    )
    .expect("titles");
    crate::db::commit(tx).expect("commit");
    let split: Vec<(i64, String)> = c
        .prepare("SELECT parent_id, platform_id FROM title_groups WHERE split")
        .expect("prepare")
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .collect::<rusqlite::Result<_>>()
        .expect("rows");
    assert_eq!(split, [(1, "nes".to_owned())]);
    let f = Browse {
        q: Some("starla".into()),
        ..Browse::default()
    };
    for shape in SearchShape::ALL {
        let (rows, total) = titles::browse_with(&c, "nes", &f, 10, 0, shape).expect("browse");
        assert_eq!(total, 1, "{shape:?}");
        assert_eq!(rows[0].parent_id, TitleId(1), "{shape:?}");
    }
    let tx = c.transaction().expect("tx");
    tx.execute("UPDATE titles SET platform_id = 'nes' WHERE id = 1", [])
        .expect("move");
    crate::db::commit(tx).expect("commit");
    let splits: i64 = c
        .query_row("SELECT COUNT(*) FROM title_groups WHERE split", [], |r| {
            r.get(0)
        })
        .expect("count");
    assert_eq!(splits, 0);
    let (_, total) =
        titles::browse_with(&c, "nes", &f, 10, 0, SearchShape::FtsPlatform).expect("browse");
    assert_eq!(total, 1);
}

#[test]
fn the_migration_builds_the_table_from_existing_rows() {
    let mut c = Connection::open_in_memory().expect("open");
    c.pragma_update(None, "foreign_keys", false).expect("fk");
    c.execute_batch(
        "CREATE TABLE schema_version (version INTEGER PRIMARY KEY, name TEXT NOT NULL,
                                      applied_at INTEGER NOT NULL);",
    )
    .expect("schema_version");
    for m in crate::db::migrate::MIGRATIONS
        .iter()
        .filter(|m| m.version < 16)
    {
        c.execute_batch(m.sql).expect(m.name);
        c.execute(
            "INSERT INTO schema_version VALUES (?1, ?2, 0)",
            params![m.version, m.name],
        )
        .expect("record");
    }
    c.execute_batch(
        "INSERT INTO dat_versions (id, platform_id, dat_name, version, source_file, loaded_at, game_count)
           VALUES (1, 'gb', 'Test', '1', 't.dat', 0, 0);
         INSERT INTO titles (id, platform_id, dat_version_id, name, base_name, regions, languages, flags, parent_id, wanted)
           VALUES (1, 'gb', 1, 'A (USA)', 'A', '[\"USA\"]', '[]', '[]', 1, 1),
                  (2, 'gb', 1, 'A (Japan) (Beta)', 'A', '[\"Japan\"]', '[]', '[\"beta\"]', 1, 0),
                  (3, 'gb', 1, 'B (Europe)', 'B', '[\"Europe\"]', '[]', '[\"bios\"]', 3, 0),
                  (4, 'gb', 1, 'A (Europe)', 'A', '[\"Europe\"]', '[]', '[]', 4, 0);
         UPDATE titles SET group_root = 1 WHERE id = 4;
         INSERT INTO roms (id, title_id, name, size) VALUES (1, 1, 'a.bin', 4), (2, 3, 'b.bin', 4);
         INSERT INTO files (platform_id, rel_path, size, mtime, rom_id, state, scanned_at)
           VALUES ('gb', 'a.bin', 4, 0, 1, 'verified', 0);",
    )
    .expect("rows");
    let before = rows(&c, "title_groups");
    crate::db::migrate::apply(&mut c).expect("migrate");
    assert_eq!(rows(&c, "title_groups"), before);
    assert_eq!(before.len(), 2);
    assert!(check(&c).expect("check").is_consistent());
    let flags: Vec<(i64, String)> = c
        .prepare("SELECT title_id, flag FROM title_flags ORDER BY title_id")
        .expect("prepare")
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .collect::<rusqlite::Result<_>>()
        .expect("rows");
    assert_eq!(flags, [(2, "beta".to_owned()), (3, "bios".to_owned())]);
    let hits: Vec<i64> = c
        .prepare("SELECT rowid FROM title_search WHERE title_search MATCH '\"B\"' ORDER BY rowid")
        .expect("prepare")
        .query_map([], |r| r.get(0))
        .expect("query")
        .collect::<rusqlite::Result<_>>()
        .expect("rows");
    assert!(hits.is_empty(), "trigram needs three characters: {hits:?}");
}

#[test]
fn autocommit_writes_are_settled_by_the_writer() {
    let (_dir, db) = crate::db::testutil::db();
    db.write_blocking(|c| {
        crate::db::platforms::seed(c, &mistarr_mister::platforms::PLATFORMS)?;
        c.execute(
            "INSERT INTO dat_versions (id, platform_id, dat_name, version, source_file, loaded_at, game_count)
             VALUES (1, 'gb', 'Test', '1', 't.dat', 0, 0)",
            [],
        )?;
        c.execute(
            "INSERT INTO titles (platform_id, dat_version_id, name, base_name)
             VALUES ('gb', 1, 'A', 'A')",
            [],
        )?;
        c.execute("UPDATE titles SET parent_id = id", [])?;
        Ok(())
    })
    .expect("write");
    let (pending, groups): (bool, i64) = db
        .read_blocking(|c| {
            let n = c.query_row("SELECT COUNT(*) FROM title_groups", [], |r| r.get(0))?;
            Ok((pending(c)?, n))
        })
        .expect("read");
    assert!(!pending);
    assert_eq!(groups, 1);
}

#[test]
fn every_write_transaction_commits_through_the_group_refresh() {
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).expect("read_dir").flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    walk(&src, &mut files);
    let allowed = [src.join("db/mod.rs"), src.join("db/migrate.rs")];
    let needle = format!(".{}()", "commit");
    let offenders: Vec<String> = files
        .iter()
        .filter(|p| !allowed.contains(p))
        .filter(|p| std::fs::read_to_string(p).expect("read").contains(&needle))
        .map(|p| p.display().to_string())
        .collect();
    assert!(
        offenders.is_empty(),
        "commit through crate::db::commit: {offenders:?}"
    );
}

#[test]
fn visibility_uses_the_summary_before_the_variants() {
    let strings = |xs: &[&str]| xs.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    let mut c = Clause::default();
    visible(&mut c, &[], None, &[]);
    assert_eq!(c.sql(), "1");
    let mut c = Clause::default();
    visible(&mut c, &[], Some("usa"), &[]);
    assert_eq!(c.sql(), "(g.region_union & 2 != 0)");
    let mut c = Clause::default();
    visible(&mut c, &strings(&["bios", "beta"]), None, &[]);
    assert!(
        c.sql().starts_with("((g.lean_flags & 24 = 0 OR EXISTS"),
        "{}",
        c.sql()
    );
    assert!(!c.sql().contains("json_each"), "{}", c.sql());
    let mut c = Clause::default();
    visible(
        &mut c,
        &strings(&["bios", "other:x"]),
        Some("Atlantis"),
        &strings(&["demo", "other:y"]),
    );
    assert_eq!(c.args.len(), 5);
    assert!(c.sql().contains("g.flag_union & 64 = 64"), "{}", c.sql());
}

/// Loads two DAT versions of one platform whose titles share roms, links them, then
/// retires the larger one; the table follows `group_root` through every step.
#[test]
fn linking_across_dats_and_retiring_one_keeps_the_table_equal_to_the_reference() {
    let mut c = conn();
    let tx = c.transaction().expect("tx");
    tx.execute_batch(
        "INSERT INTO dat_versions (id, platform_id, dat_name, version, source_file, loaded_at, game_count)
           VALUES (2, 'nes', 'Test A', '1', 'a.dat', 0, 3), (3, 'nes', 'Test B', '1', 'b.dat', 0, 2);
         INSERT INTO titles (id, platform_id, dat_version_id, name, base_name, parent_id)
           VALUES (10, 'nes', 2, 'Alpha Quest (USA)', 'Alpha Quest', 10),
                  (11, 'nes', 2, 'Alpha Quest (Japan)', 'Alpha Quest', 10),
                  (12, 'nes', 2, 'Beta Star (USA)', 'Beta Star', 12),
                  (20, 'nes', 3, 'Alpha Quest (World)', 'Alpha Quest', 20),
                  (21, 'nes', 3, 'Alpha Quest (Europe)', 'Alpha Quest', 20);
         INSERT INTO roms (title_id, name, size, sha1)
           VALUES (10, 'a.nes', 4, 'aa'), (11, 'b.nes', 4, 'bb'), (12, 'c.nes', 4, 'cc'),
                  (20, 'a.nes', 4, 'aa'), (21, 'd.nes', 4, 'dd');",
    )
    .expect("rows");
    titles::recompute_platform(&tx, "nes", &Prefs::default()).expect("recompute");
    crate::db::commit(tx).expect("commit");
    let root = |c: &Connection, id: i64| -> i64 {
        c.query_row("SELECT group_root FROM titles WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .expect("root")
    };
    assert_eq!((root(&c, 20), root(&c, 21)), (10, 21));
    assert_matches_reference(&c);

    let tx = c.transaction().expect("tx");
    dats::retire(&tx, DatVersionId(2), 0).expect("retire");
    titles::recompute_platform(&tx, "nes", &Prefs::default()).expect("recompute");
    crate::db::commit(tx).expect("commit");
    assert_eq!((root(&c, 20), root(&c, 21)), (20, 20));
    assert_matches_reference(&c);
}
