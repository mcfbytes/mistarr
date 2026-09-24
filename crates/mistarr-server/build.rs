//! Lists `migrations/NNNN_*.sql` into `$OUT_DIR/migrations.rs` and tells Cargo
//! when to re-embed `web/dist`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::{env, fs};

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let migrations = manifest.join("migrations");
    println!("cargo:rerun-if-changed={}", migrations.display());

    let web = manifest.join("../../web");
    let dist = web.join("dist");
    // A missing path marks the crate dirty on every build, so until dist exists
    // watch web/, whose scan only includes node_modules between `npm ci` and a build.
    let watched = if dist.exists() { &dist } else { &web };
    println!("cargo:rerun-if-changed={}", watched.display());

    let mut found = collect(&migrations);
    found.sort();
    let mut out = String::from("&[\n");
    for (version, name, path) in &found {
        let _ = writeln!(
            out,
            "    Migration {{ version: {version}, name: {name:?}, sql: include_str!({path:?}) }},"
        );
    }
    out.push(']');
    let target = PathBuf::from(env::var("OUT_DIR").unwrap_or_default()).join("migrations.rs");
    if let Err(e) = fs::write(&target, out) {
        panic!("cannot write {}: {e}", target.display());
    }
}

/// Returns `(version, stem, absolute path)` for every `NNNN_name.sql` file.
fn collect(dir: &Path) -> Vec<(u32, String, String)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("sql") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Some((num, _)) = stem.split_once('_') else {
            panic!("migration {stem} is not named NNNN_name.sql");
        };
        let Ok(version) = num.parse::<u32>() else {
            panic!("migration {stem} does not start with a number");
        };
        if found.iter().any(|(v, _, _)| *v == version) {
            panic!("migration number {version} is used twice");
        }
        found.push((version, stem.to_owned(), path.display().to_string()));
    }
    found
}
