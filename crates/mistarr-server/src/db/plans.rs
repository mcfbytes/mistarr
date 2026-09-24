//! Query plans of the hot reads; see `docs/DATA-MODEL.md` "Indexes".

use std::cell::RefCell;

use rusqlite::trace::{TraceEvent, TraceEventCodes};
use rusqlite::Connection;
use serde_json::json;

use super::downloads::{self, DownloadState};
use super::titles::{self, Browse, SearchShape, Sort, TitleId, SEARCH_SHAPE};
use super::{groups, imports, jobs, launch, sources};

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
            Box::new(|c| drop(sources::files(c, sources::SourceId(1), 50, 0).expect("files"))),
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
    ];
    let mut out = Vec::new();
    for (name, read) in reads {
        for sql in traced(&c, read) {
            let p = plan(&c, &sql);
            out.push((name, sql, p));
        }
    }
    out
}

/// The whole-table walks the hot reads may make, each bounded or inherent.
const ALLOWED_SCANS: [(&str, &str); 11] = [
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
        // The default search walks the platform's name index and never the trigram index.
        if name == "browse" && SEARCH_SHAPE == SearchShape::Like {
            assert!(!plan.iter().any(|l| l.contains("title_search")), "{sql}");
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
