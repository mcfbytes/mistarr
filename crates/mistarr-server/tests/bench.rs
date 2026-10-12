//! Runs the built binary's hidden `bench-seed`, `bench-search` and `bench-preview` subcommands.

use std::path::Path;
use std::process::{Command, Output};

fn mistarr(data: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mistarr"))
        .arg("--data")
        .arg(data)
        .args(args)
        .output()
        .expect("run")
}

#[test]
fn bench_seed_writes_a_new_file_and_bench_search_times_every_shape() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("bench.db");
    let db_arg = db.to_string_lossy().into_owned();
    let seed = mistarr(
        dir.path(),
        &["bench-seed", "--db", &db_arg, "--scale", "0.01"],
    );
    assert!(
        seed.status.success(),
        "{}",
        String::from_utf8_lossy(&seed.stderr)
    );
    assert!(String::from_utf8_lossy(&seed.stdout).contains("titles"));

    let again = mistarr(dir.path(), &["bench-seed", "--db", &db_arg]);
    assert!(!again.status.success());
    assert!(String::from_utf8_lossy(&again.stderr).contains("exists"));

    let before = std::fs::metadata(&db)
        .expect("db")
        .modified()
        .expect("mtime");
    let search = mistarr(
        dir.path(),
        &[
            "bench-search",
            "--db",
            &db_arg,
            "--platform",
            "psx",
            "--term",
            "sta",
            "--iterations",
            "3",
        ],
    );
    assert!(
        search.status.success(),
        "{}",
        String::from_utf8_lossy(&search.stderr)
    );
    let text = String::from_utf8(search.stdout).expect("utf8");
    for shape in ["like ", "fts ", "fts-platform "] {
        assert!(
            text.lines().any(|l| l.starts_with(shape)),
            "{shape}: {text}"
        );
    }
    assert_eq!(
        std::fs::metadata(&db)
            .expect("db")
            .modified()
            .expect("mtime"),
        before
    );

    let one = mistarr(
        dir.path(),
        &[
            "bench-search",
            "--db",
            &db_arg,
            "--platform",
            "nes",
            "--shape",
            "like",
        ],
    );
    let text = String::from_utf8(one.stdout).expect("utf8");
    assert_eq!(
        text.lines().filter(|l| l.starts_with("fts")).count(),
        0,
        "{text}"
    );

    let missing = dir.path().join("missing.db");
    let none = mistarr(
        dir.path(),
        &[
            "bench-search",
            "--db",
            &missing.to_string_lossy(),
            "--platform",
            "nes",
        ],
    );
    assert!(!none.status.success());
    assert!(!missing.exists());
}

#[test]
fn bench_preview_times_each_source_and_its_base_tier() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("bench.db");
    let db_arg = db.to_string_lossy().into_owned();
    let seed = mistarr(
        dir.path(),
        &["bench-seed", "--db", &db_arg, "--scale", "0.01"],
    );
    assert!(seed.status.success());
    let c = rusqlite::Connection::open(&db).expect("open");
    c.execute_batch(
        "INSERT INTO sources (infohash, display_name, origin_file, state, added_at)
           VALUES ('0c', 'Synthetic Preview', 'p.torrent', 'unbound', 0);
         INSERT INTO torrent_dirs (source_id, path) VALUES (1, 'Set/');
         INSERT INTO torrent_files (source_id, file_index, dir_id, leaf, size)
           SELECT 1, id - 1,
                  (SELECT id FROM torrent_dirs WHERE source_id = 1 AND path = 'Set/'),
                  name, size FROM roms ORDER BY id LIMIT 600;
         UPDATE sources SET file_count = 600;",
    )
    .expect("source");
    drop(c);
    let run = mistarr(
        dir.path(),
        &["bench-preview", "--db", &db_arg, "--iterations", "2"],
    );
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let text = String::from_utf8(run.stdout).expect("utf8");
    let row = text
        .lines()
        .find(|l| l.starts_with("1 "))
        .unwrap_or_else(|| panic!("{text}"));
    assert_eq!(row.split_whitespace().nth(1), Some("600"), "{text}");
    assert!(text.contains("base%total"), "{text}");
}
