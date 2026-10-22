//! The checked-in `schema/` directory equals what the migrations produce; see docs/TESTING.md.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use mistarr_server::db::migrate;
use rusqlite::Connection;

const REGENERATE: &str = "MISTARR_WRITE_SCHEMA=1 cargo test -p mistarr-server --test schema";
const SHADOW_SUFFIXES: [&str; 5] = ["_data", "_idx", "_content", "_docsize", "_config"];

struct Object {
    kind: String,
    name: String,
    table: String,
    sql: String,
}

fn objects(conn: &Connection) -> Vec<Object> {
    let mut stmt = conn
        .prepare(
            "SELECT type, name, tbl_name, sql FROM sqlite_schema \
             WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' \
             ORDER BY name",
        )
        .expect("prepare");
    stmt.query_map([], |r| {
        Ok(Object {
            kind: r.get(0)?,
            name: r.get(1)?,
            table: r.get(2)?,
            sql: r.get(3)?,
        })
    })
    .expect("query")
    .collect::<rusqlite::Result<_>>()
    .expect("rows")
}

/// Renders every file as a path relative to `schema/` and its text.
fn render(conn: &Connection) -> BTreeMap<String, String> {
    let all = objects(conn);
    let virtuals: Vec<&str> = all
        .iter()
        .filter(|o| o.kind == "table" && o.sql.starts_with("CREATE VIRTUAL TABLE"))
        .map(|o| o.name.as_str())
        .collect();
    let shadow = |name: &str| {
        virtuals.iter().any(|v| {
            SHADOW_SUFFIXES
                .iter()
                .any(|s| name.strip_prefix(v).is_some_and(|rest| rest == *s))
        })
    };
    let mut files = BTreeMap::new();
    for owner in all.iter().filter(|o| o.kind == "table" || o.kind == "view") {
        if owner.kind == "table" && shadow(&owner.name) {
            continue;
        }
        let dir = if owner.kind == "table" {
            "tables"
        } else {
            "views"
        };
        let mut text = format!("{};\n", owner.sql);
        for kind in ["index", "trigger"] {
            for o in all
                .iter()
                .filter(|o| o.kind == kind && o.table == owner.name)
            {
                text.push('\n');
                text.push_str(&o.sql);
                text.push_str(";\n");
            }
        }
        files.insert(format!("{dir}/{}.sql", owner.name), text);
    }
    files
}

fn read_dir_files(root: &Path) -> BTreeMap<String, String> {
    let mut files = BTreeMap::new();
    for dir in ["tables", "views"] {
        let Ok(entries) = fs::read_dir(root.join(dir)) else {
            continue;
        };
        for entry in entries {
            let path = entry.expect("entry").path();
            if path.extension().is_some_and(|e| e == "sql") {
                let name = path
                    .file_name()
                    .expect("name")
                    .to_string_lossy()
                    .into_owned();
                files.insert(
                    format!("{dir}/{name}"),
                    fs::read_to_string(&path).expect("read"),
                );
            }
        }
    }
    files
}

fn schema_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("schema")
}

fn write_dir(root: &Path, files: &BTreeMap<String, String>) {
    for dir in ["tables", "views"] {
        let _ = fs::remove_dir_all(root.join(dir));
        fs::create_dir_all(root.join(dir)).expect("mkdir");
    }
    for (name, text) in files {
        fs::write(root.join(name), text).expect("write");
    }
}

fn migrated() -> Connection {
    let mut conn = Connection::open_in_memory().expect("open");
    migrate::apply(&mut conn).expect("apply");
    conn
}

#[test]
fn schema_directory_matches_the_migrations() {
    let rendered = render(&migrated());
    let root = schema_dir();
    if std::env::var_os("MISTARR_WRITE_SCHEMA").is_some() {
        write_dir(&root, &rendered);
        return;
    }
    let on_disk = read_dir_files(&root);
    let mut problems = Vec::new();
    for (name, text) in &rendered {
        match on_disk.get(name) {
            None => problems.push(format!("missing: {name}")),
            Some(found) if found != text => problems.push(format!("different: {name}")),
            Some(_) => {}
        }
    }
    problems.extend(
        on_disk
            .keys()
            .filter(|n| !rendered.contains_key(*n))
            .map(|n| format!("extra: {n}")),
    );
    assert!(
        problems.is_empty(),
        "schema/ differs from the migrations:\n{}\nregenerate with: {REGENERATE}",
        problems.join("\n")
    );
}

#[test]
fn rendering_skips_shadow_tables_and_automatic_indexes() {
    let files = render(&migrated());
    assert!(files.contains_key("tables/title_search.sql"));
    assert!(!files.keys().any(|n| n.starts_with("tables/title_search_")));
    assert!(files
        .values()
        .all(|t| t.ends_with(";\n") && !t.contains("sqlite_autoindex")));
}
