//! Query plans of the hot reads; see `docs/DATA-MODEL.md` "Indexes".

use std::cell::RefCell;

use rusqlite::trace::{TraceEvent, TraceEventCodes};
use rusqlite::Connection;
use serde_json::json;

use super::downloads::{self, DownloadState};
use super::titles::{self, Browse, SearchShape, Sort, TitleId, SEARCH_SHAPE};
use super::{candidates, chd, files, groups, imports, jobs, launch, source_detail, sources};

thread_local! {
    static TRACED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn record(event: TraceEvent<'_>) {
    // Trigger subprograms report as `-- TRIGGER name`; only top-level statements count.
    if let TraceEvent::Stmt(stmt, sql) = event {
        if !sql.starts_with("--") {
            let sql = stmt.expanded_sql().unwrap_or_else(|| sql.to_owned());
            TRACED.with(|t| t.borrow_mut().push(sql));
        }
    }
}

/// The statements `f` runs, with their parameters bound.
fn traced(c: &Connection, f: impl FnOnce(&Connection)) -> Vec<String> {
    TRACED.with(|t| t.borrow_mut().clear());
    c.trace_v2(TraceEventCodes::SQLITE_TRACE_STMT, Some(record));
    f(c);
    c.trace_v2(TraceEventCodes::empty(), None);
    TRACED.with(RefCell::take)
}

fn plan(c: &Connection, sql: &str) -> Vec<String> {
    c.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .expect("prepare")
        .query_map([], |r| r.get(3))
        .expect("query")
        .collect::<rusqlite::Result<_>>()
        .expect("rows")
}

fn seeded() -> Connection {
    let mut c = Connection::open_in_memory().expect("open");
    super::migrate::apply(&mut c).expect("migrate");
    super::platforms::seed(&mut c, &mistarr_mister::platforms::PLATFORMS).expect("seed");
    crate::synth::seed(&mut c, 0.01, 4).expect("catalogue");
    c.execute_batch(
        "INSERT INTO sources (infohash, display_name, origin_file, platform_id, state, added_at)
           VALUES ('00', 'Synthetic', 's.torrent', 'nes', 'bound', 0);
         INSERT INTO torrent_files (source_id, file_index, path, size, rom_id)
           VALUES (1, 0, 'track.bin', 16, 1);
         INSERT INTO downloads (title_id, rom_id, source_id, file_index, state, created_at, updated_at)
           VALUES (1, 1, 1, 0, 'transferring', 0, 0);",
    )
    .expect("sources");
    jobs::insert(&c, "import", &json!({"download_id": 1}), "background", 0).expect("job");
    imports::log(
        &c,
        0,
        Some(1),
        Some(1),
        imports::ImportAction::Placed,
        &json!({}),
    )
    .expect("log");
    groups::flush(&c).expect("flush");
    c
}

/// A read under test.
type Read<'a> = Box<dyn FnOnce(&Connection) + 'a>;

/// The reads of matching stored hashes, binding, a disc directory's tracks and the
/// recent jobs.
fn matching_reads() -> Vec<(&'static str, Read<'static>)> {
    vec![
        (
            "recent jobs",
            Box::new(|c| drop(jobs::recent_finished(c, 10).expect("recent"))),
        ),
        (
            "unmatched files",
            Box::new(|c| {
                let nes = mistarr_core::PlatformId("nes".into());
                drop(files::unmatched_after(c, &nes, files::FileId(0), 256).expect("unmatched"));
            }),
        ),
        (
            "stored match",
            Box::new(|c| {
                let nes = mistarr_core::PlatformId("nes".into());
                let (sha1, md5) = ("0".repeat(40), "0".repeat(32));
                files::match_live_rom(c, &nes, &sha1, &md5, "00000000", 16).expect("match");
            }),
        ),
        (
            "crc candidate",
            Box::new(|c| {
                let nes = mistarr_core::PlatformId("nes".into());
                files::crc_candidate_exists(c, &nes, "00000000", 16).expect("candidate");
            }),
        ),
        (
            "binding lookups",
            Box::new(|c| {
                use mistarr_sources::binding::DatIndex as _;
                let index = sources::SqlDatIndex::new(c);
                drop(index.by_normalised_name("example quest.nes"));
                drop(index.by_base_name_and_size("example quest", 16));
            }),
        ),
        (
            "size candidates",
            Box::new(|c| {
                use mistarr_sources::fuzzy::SizeIndex as _;
                let nes = mistarr_core::PlatformId("nes".into());
                drop(candidates::SqlSizeIndex::new(c, &nes).roms_of_size(40_976));
            }),
        ),
        (
            "directory tracks",
            Box::new(|c| {
                let psx = mistarr_core::PlatformId("psx".into());
                drop(files::in_directory(c, &psx, "psx/Example Disc").expect("tracks"));
            }),
        ),
        (
            "chd lookups",
            Box::new(|c| {
                let psx = mistarr_core::PlatformId("psx".into());
                files::chd_rom_sized(c, &psx, 4_704).expect("chd rom");
                chd::layout_known(c, &psx, &[4_704]).expect("layout");
            }),
        ),
    ]
}

/// The reads of a source's detail view: its file filters, summary and preview.
fn source_reads() -> Vec<(&'static str, Read<'static>)> {
    vec![
        (
            "source file filters",
            Box::new(|c| {
                for filter in [
                    source_detail::FileFilter::Matched,
                    source_detail::FileFilter::Unmatched,
                    source_detail::FileFilter::Wanted,
                ] {
                    let query = source_detail::FileQuery {
                        filter: Some(filter),
                        q: Some("track".into()),
                    };
                    let id = sources::SourceId(1);
                    drop(source_detail::files(c, id, &query, 50, 0).expect("files"));
                }
            }),
        ),
        (
            "source detail",
            Box::new(|c| drop(source_detail::detail(c, sources::SourceId(1)).expect("detail"))),
        ),
        (
            "reclassify preview",
            Box::new(|c| {
                let max = source_detail::PREVIEW_SAMPLE;
                drop(source_detail::preview(c, sources::SourceId(1), max).expect("preview"));
            }),
        ),
    ]
}

/// Every hot read with the plan of each statement it runs.
fn hot_reads() -> Vec<(&'static str, String, Vec<String>)> {
    let c = seeded();
    let hide: Vec<String> = ["bios", "beta"].map(str::to_owned).to_vec();
    let search = Browse {
        q: Some("sta".into()),
        hidden: hide.clone(),
        region: Some("USA".into()),
        sort: Sort::Have,
        ..Browse::default()
    };
    let reads: Vec<(&str, Read)> = vec![
        (
            "browse",
            Box::new(|c| drop(titles::browse(c, "nes", &search, 60, 0).expect("browse"))),
        ),
        (
            "counts",
            Box::new(|c| drop(titles::counts(c, &hide).expect("counts"))),
        ),
        (
            "title detail",
            Box::new(|c| drop(titles::group_detail(c, TitleId(1)).expect("detail"))),
        ),
        (
            "launch",
            Box::new(|c| drop(launch::title(c, TitleId(1)).expect("launch"))),
        ),
        (
            "best file",
            Box::new(|c| {
                downloads::best_file(c, 1).expect("best");
            }),
        ),
        (
            "want",
            Box::new(|c| drop(downloads::want_title(c, TitleId(1), 0).expect("want"))),
        ),
        (
            "sources list",
            Box::new(|c| drop(sources::list(c, 50, 0).expect("sources"))),
        ),
        (
            "source files",
            Box::new(|c| {
                let all = source_detail::FileQuery::default();
                drop(source_detail::files(c, sources::SourceId(1), &all, 50, 0).expect("files"));
            }),
        ),
        (
            "downloads list",
            Box::new(|c| drop(downloads::list(c, &[], 50, 0).expect("downloads"))),
        ),
        (
            "open downloads",
            Box::new(|c| {
                let open = [DownloadState::Queued, DownloadState::Transferring];
                drop(downloads::list(c, &open, 50, 0).expect("downloads"));
            }),
        ),
        (
            "activity jobs",
            Box::new(|c| drop(jobs::list_active(c, 50, 0).expect("jobs"))),
        ),
        (
            "job dedupe",
            Box::new(|c| {
                jobs::find_open(c, "import", &json!({"download_id": 1})).expect("find");
            }),
        ),
        (
            "activity imports",
            Box::new(|c| drop(imports::list(c, 50, 0).expect("imports"))),
        ),
        (
            "group refresh",
            Box::new(|c| {
                c.execute("INSERT INTO title_groups_dirty (parent_id) VALUES (1)", [])
                    .expect("mark");
                groups::flush(c).expect("flush");
            }),
        ),
    ];
    let mut out = Vec::new();
    for (name, read) in reads
        .into_iter()
        .chain(matching_reads())
        .chain(source_reads())
    {
        for sql in traced(&c, read) {
            let p = plan(&c, &sql);
            out.push((name, sql, p));
        }
    }
    out
}

/// The whole-table walks the hot reads may make, each bounded or inherent.
const ALLOWED_SCANS: [(&str, &str); 18] = [
    // The sort over one group's availability rows, which the union gathers by rom.
    ("title detail", "SCAN (subquery-"),
    // A refresh walks at most one chunk of the dirty list and that chunk's grouped rows.
    ("group refresh", "SCAN title_groups_dirty"),
    ("group refresh", "SCAN b"),
    ("group refresh", "SCAN g"),
    // Every platform's counts read every group once, and MRA titles through their
    // partial index; the tables have one row per group and per MRA.
    ("counts", "SCAN g"),
    ("counts", "SCAN p"),
    ("counts", "SCAN t USING INDEX titles_mra_path"),
    ("counts", "SCAN m"),
    // Pages in rowid order stop at the limit; unfiltered totals count a whole table.
    ("sources list", "SCAN s"),
    ("sources list", "SCAN sources"),
    ("activity imports", "SCAN import_log"),
    ("downloads list", "SCAN d USING COVERING INDEX"),
    ("downloads list", "SCAN d USING INDEX downloads_recent"),
    // A single-row subquery and the search index's own lookup.
    ("browse", "SCAN CONSTANT ROW"),
    ("browse", "SCAN title_search VIRTUAL TABLE"),
    ("crc candidate", "SCAN CONSTANT ROW"),
    // The platform table is the fixed list the binary seeds, a few dozen rows.
    ("reclassify preview", "SCAN p"),
    ("chd lookups", "SCAN CONSTANT ROW"),
];

/// Every hot read seeks through indexes except the scans in [`ALLOWED_SCANS`], and the
/// unfiltered downloads page reads in index order instead of sorting every download.
#[test]
fn hot_reads_walk_indexes_not_growing_tables() {
    let mut failures = Vec::new();
    for (name, sql, plan) in hot_reads() {
        let sql = sql.split_whitespace().collect::<Vec<_>>().join(" ");
        eprintln!("== {name}\n{sql}\n{}\n", plan.join("\n"));
        for line in &plan {
            let scan = line.starts_with("SCAN ")
                && !ALLOWED_SCANS
                    .iter()
                    .any(|(n, p)| *n == name && line.starts_with(p));
            let sort = line.starts_with("USE TEMP B-TREE") && name == "downloads list";
            if scan || sort {
                failures.push(format!("{name}: {line} in {sql}"));
            }
        }
        // The default search asks the trigram index for the platform's phrase.
        if name == "browse" && sql.contains("title_groups g") {
            assert_eq!(SEARCH_SHAPE, SearchShape::FtsPlatform);
            assert!(sql.contains("platform : \"\u{1f}nes\u{1f}\""), "{sql}");
            assert!(
                plan.iter()
                    .any(|l| l.contains("SCAN title_search VIRTUAL TABLE")),
                "{sql}"
            );
            assert!(
                plan.iter().any(|l| l.contains("title_groups_split")),
                "{sql}"
            );
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The lines of every plan `name` runs whose statement contains `sql`.
fn plan_of(reads: &[(&str, String, Vec<String>)], name: &str, sql: &str) -> Vec<String> {
    let lines: Vec<String> = reads
        .iter()
        .filter(|(n, s, _)| *n == name && s.contains(sql))
        .flat_map(|(_, _, p)| p.iter().cloned())
        .collect();
    assert!(!lines.is_empty(), "{name}: no statement with {sql}");
    lines
}

/// Title detail reaches torrent files and candidates through the group's roms, never
/// by walking a bound source's files.
#[test]
fn title_detail_seeks_torrent_rows_by_rom() {
    let reads = hot_reads();
    let plan = plan_of(&reads, "title detail", "torrent_candidates");
    let has = |p: &str| plan.iter().any(|l| l.contains(p));
    assert!(
        has("SEARCH tf USING INDEX torrent_files_rom (rom_id=?)"),
        "{plan:?}"
    );
    assert!(
        has("SEARCH c USING INDEX torrent_candidates_rom (rom_id=?)"),
        "{plan:?}"
    );
    assert!(!has("sources_state"), "{plan:?}");
    let first = plan
        .iter()
        .find(|l| l.contains(" tf ") || l.contains(" c "))
        .expect("torrent rows");
    assert!(first.contains("(rom_id=?)"), "{plan:?}");
}

/// A source's detail and file pages reach its files as one range of the primary key
/// and their downloads through `downloads_source`; the preview finds DATs by platform.
#[test]
fn source_detail_seeks_the_source_files_and_downloads() {
    let reads = hot_reads();
    for name in ["source files", "source file filters", "source detail"] {
        let plan = plan_of(&reads, name, "torrent_files f");
        let has = |p: &str| plan.iter().any(|l| l.contains(p));
        assert!(
            has("SEARCH f USING INDEX sqlite_autoindex_torrent_files_1 (source_id=?)"),
            "{name}: {plan:?}"
        );
        assert!(!has("SCAN f"), "{name}: {plan:?}");
    }
    for name in ["source files", "source file filters"] {
        let plan = plan_of(&reads, name, "LEFT JOIN downloads d");
        let seek = "downloads_source (source_id=? AND file_index=?)";
        assert!(plan.iter().any(|l| l.contains(seek)), "{name}: {plan:?}");
    }
    let plan = plan_of(&reads, "source detail", "FROM downloads d JOIN");
    assert!(
        plan.iter()
            .any(|l| l.contains("downloads_source (source_id=?)")),
        "{plan:?}"
    );
    let plan = plan_of(&reads, "reclassify preview", "FROM platforms p");
    let seek = "titles_source (platform_id=? AND source=?)";
    assert!(plan.iter().any(|l| l.contains(seek)), "{plan:?}");
}

/// The group refresh reaches titles by group root and files by rom.
#[test]
fn the_group_refresh_seeks_by_group_root_and_rom() {
    let reads = hot_reads();
    let plan = plan_of(&reads, "group refresh", "INSERT INTO title_groups (");
    let has = |p: &str| plan.iter().any(|l| l.contains(p));
    assert!(has("titles_group_root (group_root=?)"), "{plan:?}");
    assert!(has("files_rom (rom_id=?)"), "{plan:?}");
}

/// Each rom lookup seeks the index made for it, the partial ones included, and a disc
/// directory's tracks are one range of the files' unique key.
#[test]
fn rom_lookups_and_directory_tracks_seek_their_own_index() {
    let reads = hot_reads();
    let expect = |name: &str, sql: &str, index: &str| {
        let plan = plan_of(&reads, name, sql);
        assert!(plan.iter().any(|l| l.contains(index)), "{name}: {plan:?}");
    };
    expect("stored match", "r.sha1 = ", "roms_sha1 (sha1=?)");
    expect("stored match", "r.md5 = ", "roms_md5 (md5=?)");
    expect(
        "stored match",
        "r.crc32 = ",
        "roms_crc (crc32=? AND size=?)",
    );
    expect(
        "binding lookups",
        "r.match_name = ",
        "roms_match_name (match_name=?)",
    );
    expect(
        "binding lookups",
        "r.match_base = ",
        "roms_match_base (match_base=? AND size=?)",
    );
    expect("size candidates", "r.size IN", "roms_size (size=?)");
    expect("chd lookups", "LIKE '%.chd'", "roms_chd_size (size=?)");
    expect("chd lookups", "r.size % 2352", "roms_track_size (size=?)");
    expect(
        "directory tracks",
        "FROM files",
        "sqlite_autoindex_files_1 (platform_id=? AND rel_path>? AND rel_path<?)",
    );
}

/// The recent jobs seek finished rows by state and sort only those, which the scheduler's
/// prune keeps to a few hundred, so they need no index of their own.
#[test]
fn recent_jobs_sort_only_finished_rows() {
    let reads = hot_reads();
    let plan = plan_of(&reads, "recent jobs", "FROM jobs");
    let seek = "SEARCH jobs USING INDEX jobs_state (state=?)";
    assert!(plan.iter().any(|l| l.starts_with(seek)), "{plan:?}");
}
