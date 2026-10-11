//! Reads through the shared views plan like the same reads written out over the tables;
//! see `docs/DATA-MODEL.md` "Views".

use std::cell::RefCell;

use mistarr_core::hash::HeaderRule;
use mistarr_core::{Crc32, Md5, RomId, Sha1};
use rusqlite::trace::{TraceEvent, TraceEventCodes};
use rusqlite::Connection;

use super::super::ids::{DownloadId, SourceId, TitleId};
use super::super::views::source_detail;
use super::super::{candidates, downloads, downloads_import, files, imports, roms, sources};
use super::super::{fixtures::pid, titles};
use super::{seeded, Read, FIFTY};

thread_local! {
    static RAW: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "the trace callback's signature"
)]
fn record(event: TraceEvent<'_>) {
    if let TraceEvent::Stmt(_, sql) = event {
        if !sql.starts_with("--") {
            RAW.with(|t| t.borrow_mut().push(sql.to_owned()));
        }
    }
}

/// The distinct statements `f` runs that name a shared view, unbound, in first-run order.
fn view_statements(c: &Connection, f: impl FnOnce(&Connection)) -> Vec<String> {
    RAW.with(|t| t.borrow_mut().clear());
    c.trace_v2(TraceEventCodes::SQLITE_TRACE_STMT, Some(record));
    f(c);
    c.trace_v2(TraceEventCodes::empty(), None);
    let mut out: Vec<String> = Vec::new();
    for sql in RAW.with(RefCell::take) {
        if sql.contains("_view") && !out.contains(&sql) {
            out.push(sql);
        }
    }
    out
}

/// The plan of `sql` with its parameters left unbound.
fn plan(c: &Connection, sql: &str) -> Vec<String> {
    let mut stmt = c
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .expect("prepare");
    let mut rows = stmt.raw_query();
    let mut out = Vec::new();
    while let Some(row) = rows.next().expect("row") {
        out.push(row.get(3).expect("detail"));
    }
    out
}

/// The seek, scan and sort steps of `sql`'s plan as a sorted multiset, with aliases,
/// subquery numbers and LEFT-JOIN markers left out.
fn steps(c: &Connection, sql: &str) -> Vec<String> {
    let mut out: Vec<String> = plan(c, sql)
        .into_iter()
        .filter_map(|line| {
            if line.starts_with("USE TEMP B-TREE") {
                return Some(line);
            }
            if !(line.starts_with("SEARCH ") || line.starts_with("SCAN ")) {
                return None;
            }
            let words: Vec<&str> = line
                .split_whitespace()
                .enumerate()
                .filter(|&(i, w)| i != 1 && w != "LEFT-JOIN")
                .map(|(_, w)| w)
                .collect();
            Some(words.join(" "))
        })
        .collect();
    out.sort();
    out
}

/// The same read over the tables: written out, or the view statement with names swapped.
#[derive(Clone)]
enum Out {
    Sql(String),
    Swap(&'static [(&'static str, &'static str)]),
}

const TORRENT_FILES: &[(&str, &str)] = &[("torrent_file_view", "torrent_files")];

fn tiers(live: &str, order: &str) -> Vec<Out> {
    let select = format!(
        "SELECT r.id, r.title_id, r.name, r.status, t.name
         FROM roms r JOIN titles t ON t.id = r.title_id
         JOIN dat_versions d ON d.id = t.dat_version_id
         WHERE t.platform_id = ?1 AND t.source = 'dat' AND {live}"
    );
    [
        "r.sha1 = ?2",
        "r.sha1 IS NULL AND r.md5 = ?2",
        "r.sha1 IS NULL AND r.md5 IS NULL AND r.crc32 = ?2 AND r.size = ?3",
    ]
    .map(|cond| Out::Sql(format!("{select}{cond}{order}")))
    .into()
}

const LIVE: &str = "r.retired = 0 AND t.retired = 0 AND ";
const BEST: &str = " ORDER BY (r.retired = 0 AND t.retired = 0) DESC,
         d.superseded_by IS NULL DESC, d.id DESC LIMIT 1";

/// The binding lookups by name and by base and size, written out.
fn binding() -> Vec<Out> {
    vec![
        Out::Sql(
            "SELECT t.platform_id, r.id FROM roms r JOIN titles t ON t.id = r.title_id
             WHERE r.match_name = ?1 AND t.retired = 0
               AND NOT EXISTS (SELECT 1 FROM title_flags f WHERE f.title_id = t.id AND f.flag = 'bios')
             ORDER BY r.id"
                .into(),
        ),
        Out::Sql(
            "SELECT t.platform_id, r.id
             FROM roms r INDEXED BY roms_match_base JOIN titles t ON t.id = r.title_id
             WHERE r.match_base = ?1 AND r.size = ?2 AND t.retired = 0
               AND NOT EXISTS (SELECT 1 FROM title_flags f WHERE f.title_id = t.id AND f.flag = 'bios')
             ORDER BY r.id"
                .into(),
        ),
    ]
}

/// The rom reads through `rom_view` and `live_rom_view`, each with its written-out form.
#[expect(clippy::too_many_lines, reason = "one entry per read")]
fn rom_reads() -> Vec<(&'static str, Read<'static>, Vec<Out>)> {
    let zero = (
        Some(Sha1::from_bytes([0; 20])),
        Some(Md5::from_bytes([0; 16])),
        Some(Crc32::from_u32(0)),
    );
    vec![
        (
            "best match",
            Box::new(move |c| {
                drop(roms::match_rom(c, &pid("nes"), zero.0, zero.1, zero.2, 16).expect("match"));
            }),
            tiers("", BEST),
        ),
        (
            "stored match",
            Box::new(move |c| {
                let nes = pid("nes");
                drop(roms::match_live_rom(c, &nes, zero.0, zero.1, zero.2, 16).expect("match"));
            }),
            tiers(LIVE, BEST),
        ),
        (
            "matching roms",
            Box::new(|c| {
                let h = mistarr_core::hash::hash_reader(&b"a"[..], HeaderRule::default(), None)
                    .expect("hash");
                drop(roms::roms_matching(c, &pid("nes"), &h).expect("matching"));
            }),
            tiers(LIVE, " ORDER BY r.id"),
        ),
        (
            "disc roms",
            Box::new(|c| drop(roms::disc_roms(c, TitleId::new(1)).expect("roms"))),
            vec![Out::Sql(
                "SELECT r.id, r.title_id, r.name, r.status, t.name FROM roms r JOIN titles t ON t.id = r.title_id
                 WHERE r.title_id = ?1 AND r.retired = 0 AND t.retired = 0 ORDER BY r.id"
                    .into(),
            )],
        ),
        (
            "crc candidate",
            Box::new(|c| {
                let crc = Crc32::from_u32(0);
                roms::crc_candidate_exists(c, &pid("nes"), crc, 16).expect("candidate");
            }),
            vec![Out::Sql(
                "SELECT EXISTS(
                   SELECT 1 FROM roms r JOIN titles t ON t.id = r.title_id
                   WHERE t.platform_id = ?1 AND t.source = 'dat' AND r.crc32 = ?2 AND r.size = ?3)"
                    .into(),
            )],
        ),
        (
            "rom retired",
            Box::new(|c| {
                roms::rom_retired(c, RomId::new(1)).expect("retired");
            }),
            vec![Out::Sql(
                "SELECT r.retired = 1 OR t.retired = 1 FROM roms r JOIN titles t ON t.id = r.title_id
                 WHERE r.id = ?1"
                    .into(),
            )],
        ),
        (
            "live zips",
            Box::new(|c| drop(roms::live_zip_roms(c, &pid("arcade")).expect("zips"))),
            vec![Out::Sql(
                "SELECT COALESCE(r.zip_dir, ''), r.name, r.id FROM roms r JOIN titles t ON t.id = r.title_id
                 WHERE t.platform_id = ?1 AND t.source = 'mra' AND t.retired = 0 AND r.retired = 0
                 ORDER BY r.id"
                    .into(),
            )],
        ),
        (
            "binding lookups",
            Box::new(|c| {
                use mistarr_sources::binding::DatIndex as _;
                let index = sources::SqlDatIndex::new(c);
                drop(index.by_normalized_name("example quest.nes"));
                drop(index.by_base_name_and_size("example quest", 16));
            }),
            binding(),
        ),
        (
            "size candidates",
            Box::new(|c| {
                use mistarr_sources::fuzzy::SizeIndex as _;
                let nes = pid("nes");
                drop(candidates::SqlSizeIndex::new(c, &nes).roms_of_size(40_976));
            }),
            vec![Out::Sql(
                "SELECT r.id, r.match_base, COALESCE(t.group_root, t.parent_id, t.id)
                 FROM roms r JOIN titles t ON t.id = r.title_id
                 WHERE r.size IN (?2, ?3) AND t.platform_id = ?1 AND r.retired = 0 AND t.retired = 0
                   AND r.match_base IS NOT NULL
                   AND NOT EXISTS (SELECT 1 FROM title_flags f
                                   WHERE f.title_id = t.id AND f.flag = 'bios')
                 ORDER BY r.id"
                    .into(),
            )],
        ),
        (
            "rom stamp",
            Box::new(|c| drop(candidates::rom_stamp(c, &pid("nes")).expect("stamp"))),
            vec![Out::Sql(
                "SELECT r.id, COALESCE(t.group_root, t.id)
                 FROM roms r JOIN titles t ON t.id = r.title_id
                 WHERE t.platform_id = ?1 AND r.retired = 0 AND t.retired = 0"
                    .into(),
            )],
        ),
        (
            "foreign proofs",
            Box::new(|c| {
                let nes = pid("nes");
                candidates::drop_foreign_proofs(c, SourceId::new(1), &nes).expect("drop");
            }),
            vec![Out::Sql(
                "UPDATE torrent_files SET rom_id = NULL, confidence = NULL
                 WHERE source_id = ?1 AND confidence = 'hash' AND NOT EXISTS (
                   SELECT 1 FROM roms r JOIN titles t ON t.id = r.title_id
                   WHERE r.id = torrent_files.rom_id AND t.platform_id = ?2
                     AND r.retired = 0 AND t.retired = 0)"
                    .into(),
            )],
        ),
        (
            "best file",
            Box::new(|c| {
                downloads::best_file(c, RomId::new(1)).expect("best");
            }),
            vec![Out::Sql(
                "SELECT t.platform_id FROM roms r JOIN titles t ON t.id = r.title_id WHERE r.id = ?1"
                    .into(),
            )],
        ),
    ]
}

/// The reads through `title_view`, `torrent_file_view` and `file_view`, and the title
/// detail, which reads `title_view` and `rom_view`.
#[expect(clippy::too_many_lines, reason = "one entry per read")]
fn other_reads() -> Vec<(&'static str, Read<'static>, Vec<Out>)> {
    let file_cols = files::COLUMNS
        .split(", ")
        .map(|c| format!("f.{c}"))
        .collect::<Vec<_>>()
        .join(", ");
    vec![
        (
            "title detail",
            Box::new(|c| {
                drop(titles::detail::group_detail(c, TitleId::new(1)).expect("detail"));
            }),
            vec![
                Out::Sql("SELECT platform_id, base_name FROM titles WHERE id = ?1".into()),
                Out::Sql(
                    "SELECT t.id, t.name, t.revision, t.is_1g1r_pick,
                            t.wanted, t.retired, t.inferred, t.dat_version_id, t.source
                     FROM titles t WHERE t.group_root = ?1 OR (t.id = ?1 AND t.group_root IS NULL)
                     ORDER BY t.retired, t.is_1g1r_pick DESC, t.name"
                        .into(),
                ),
                Out::Sql(
                    "SELECT r.title_id, r.id, r.name, r.size, r.crc32, r.md5, r.sha1, r.status,
                            f.id, f.state, f.rel_path
                     FROM titles t JOIN roms r ON r.title_id = t.id AND r.retired = 0
                     LEFT JOIN files f ON f.id = (
                       SELECT x.id FROM files x WHERE x.rom_id = r.id
                       ORDER BY CASE x.state WHEN 'verified' THEN 0 WHEN 'misnamed' THEN 1
                                             WHEN 'bad' THEN 2 ELSE 3 END, x.id
                       LIMIT 1)
                     WHERE t.group_root = ?1 OR (t.id = ?1 AND t.group_root IS NULL)
                     ORDER BY r.title_id, r.name"
                        .into(),
                ),
            ],
        ),
        (
            "title entry",
            Box::new(|c| drop(imports::title_entry(c, TitleId::new(1)).expect("entry"))),
            vec![Out::Sql(
                "SELECT platform_id, name, source FROM titles WHERE id = ?1".into(),
            )],
        ),
        (
            "downloads list",
            Box::new(|c| {
                drop(downloads::list(c, &[], FIFTY).expect("list"));
                drop(downloads::get(c, DownloadId::new(1)).expect("get"));
            }),
            vec![
                Out::Swap(&[("title_view t", "titles t"), ("t.platform,", "t.platform_id,")]),
                Out::Swap(&[("title_view t", "titles t"), ("t.platform,", "t.platform_id,")]),
            ],
        ),
        (
            "source detail",
            Box::new(|c| drop(source_detail::detail(c, SourceId::new(1)).expect("detail"))),
            vec![Out::Swap(TORRENT_FILES)],
        ),
        (
            "reclassify preview",
            Box::new(|c| {
                let max = source_detail::PREVIEW_SAMPLE;
                drop(source_detail::preview(c, SourceId::new(1), max).expect("preview"));
            }),
            [vec![Out::Swap(TORRENT_FILES)], binding()].concat(),
        ),
        (
            "torrent file reads",
            Box::new(|c| {
                let id = SourceId::new(1);
                drop(sources::torrent_files(c, id).expect("files"));
                drop(downloads_import::torrent_path(c, id, 0).expect("path"));
                let rom = RomId::new(1);
                downloads_import::placed_on_file(c, id, 0, rom).expect("placed");
                drop(downloads::polled(c).expect("polled"));
            }),
            vec![
                Out::Swap(TORRENT_FILES),
                Out::Swap(TORRENT_FILES),
                Out::Swap(TORRENT_FILES),
                Out::Swap(TORRENT_FILES),
            ],
        ),
        (
            "file reads",
            Box::new(|c| {
                let nes = pid("nes");
                drop(files::unidentified(c, &nes, FIFTY).expect("unidentified"));
                files::state_counts(c, &nes).expect("counts");
                drop(files::retired_matches(c, &nes, 10).expect("retired"));
                drop(files::misnamed(c).expect("misnamed"));
            }),
            vec![
                Out::Sql(
                    "SELECT COUNT(*) FROM files WHERE state = 'unidentified' AND platform_id = ?1"
                        .into(),
                ),
                Out::Sql(
                    "SELECT rel_path, size, COALESCE(reason, 'corrupt') FROM files
                     WHERE state = 'unidentified' AND platform_id = ?1
                     ORDER BY rel_path LIMIT ?2 OFFSET ?3"
                        .into(),
                ),
                Out::Sql(
                    "SELECT state, COUNT(*) FROM files WHERE platform_id = ?1 GROUP BY state".into(),
                ),
                Out::Sql(format!(
                    "SELECT {file_cols} FROM files f JOIN roms r ON r.id = f.rom_id
                     JOIN titles t ON t.id = r.title_id
                     WHERE f.platform_id = ?1 AND t.source = 'dat' AND (r.retired = 1 OR t.retired = 1)
                     ORDER BY f.id LIMIT ?2"
                )),
                Out::Sql(
                    "SELECT f.id, f.platform_id, f.rel_path, r.id, r.name, t.name
                     FROM files f JOIN roms r ON r.id = f.rom_id JOIN titles t ON t.id = r.title_id
                     WHERE f.state = 'misnamed' ORDER BY f.id"
                        .into(),
                ),
            ],
        ),
        (
            "counts",
            Box::new(|c| drop(titles::browse::counts(c, &[]).expect("counts"))),
            vec![
                Out::Sql(
                    "SELECT platform_id, COUNT(*) FROM files
                     WHERE state = 'unverified' AND platform_id <> 'arcade' GROUP BY platform_id"
                        .into(),
                ),
                Out::Sql(
                    "SELECT platform_id, COUNT(*) FROM files WHERE state = 'unidentified' GROUP BY platform_id"
                        .into(),
                ),
            ],
        ),
    ]
}

/// Each read through a shared view seeks, scans and sorts exactly as the same read written
/// out over the tables, and SQLite flattens the view rather than materializing it.
#[test]
fn reads_through_views_plan_like_the_written_out_join() {
    let c = seeded();
    let mut failures = Vec::new();
    for (name, read, outs) in rom_reads().into_iter().chain(other_reads()) {
        let found = view_statements(&c, read);
        assert_eq!(found.len(), outs.len(), "{name}: {found:#?}");
        for (via, out) in found.iter().zip(outs) {
            let written = match out {
                Out::Sql(sql) => sql,
                Out::Swap(pairs) => pairs
                    .iter()
                    .fold(via.clone(), |s, (from, to)| s.replace(from, to)),
            };
            assert!(!written.contains("_view"), "{name}: {written}");
            let plan_via = plan(&c, via);
            let flat = |l: &String| !l.contains("MATERIALIZE") && !l.contains("CO-ROUTINE");
            let (a, b) = (steps(&c, &written), steps(&c, via));
            if a != b || !plan_via.iter().all(flat) {
                failures.push(format!(
                    "{name}:\n  written {a:?}\n  view    {b:?}\n  plan    {plan_via:?}\n  {via}"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
