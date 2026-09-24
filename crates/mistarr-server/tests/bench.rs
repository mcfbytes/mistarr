//! Runs the built binary's hidden `bench-seed` and `bench-search` subcommands.

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
