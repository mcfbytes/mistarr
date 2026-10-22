//! The checked-in `schema/` directory equals what the migrations produce; see docs/TESTING.md.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use mistarr_server::db::migrate;
use rusqlite::Connection;

const REGENERATE: &str = "MISTARR_WRITE_SCHEMA=1 cargo test -p mistarr-server --test schema";

struct Object {
    kind: String,
    name: String,
    table: String,
    sql: String,
}

/// Every object with SQL, shadow tables of virtual tables and `sqlite_*` internals left out.
fn objects(conn: &Connection) -> Vec<Object> {
    let mut stmt = conn
        .prepare(
            "SELECT s.type, s.name, s.tbl_name, s.sql FROM sqlite_schema s \
             LEFT JOIN pragma_table_list t ON t.schema = 'main' AND t.name = s.name \
             WHERE s.sql IS NOT NULL AND s.name NOT LIKE 'sqlite\\_%' ESCAPE '\\' \
             AND COALESCE(t.type, '') <> 'shadow' \
             ORDER BY s.name",
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
fn render(all: &[Object]) -> BTreeMap<String, String> {
    let mut children: BTreeMap<&str, Vec<&Object>> = BTreeMap::new();
    for o in all
        .iter()
        .filter(|o| o.kind == "index" || o.kind == "trigger")
    {
        children.entry(o.table.as_str()).or_default().push(o);
    }
    let mut files = BTreeMap::new();
    for owner in all.iter().filter(|o| o.kind == "table" || o.kind == "view") {
        let dir = if owner.kind == "table" {
            "tables"
        } else {
            "views"
        };
        let mut text = format!("{};\n", owner.sql);
        let mut own = children
            .get(owner.name.as_str())
            .cloned()
            .unwrap_or_default();
        own.sort_by_key(|o| (o.kind != "index", o.name.as_str()));
        for o in own {
            text.push('\n');
            text.push_str(&o.sql);
            text.push_str(";\n");
        }
        files.insert(format!("{dir}/{}.sql", owner.name), text);
    }
    files
}

/// Every file under `root` except `README.md`, keyed by its path relative to `root`.
fn read_dir_files(root: &Path) -> BTreeMap<String, Option<String>> {
    fn walk(dir: &Path, prefix: &str, out: &mut BTreeMap<String, Option<String>>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries {
            let entry = entry.expect("entry");
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            if entry.file_type().expect("type").is_dir() {
                walk(&entry.path(), &rel, out);
            } else if rel != "README.md" {
                out.insert(rel, fs::read_to_string(entry.path()).ok());
            }
        }
    }
    let mut files = BTreeMap::new();
    walk(root, "", &mut files);
    files
}

fn schema_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("schema")
}

fn write_dir(root: &Path, files: &BTreeMap<String, String>) {
    for dir in ["tables", "views"] {
        match fs::remove_dir_all(root.join(dir)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => panic!("cannot clear schema/{dir}: {e}"),
        }
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
    let rendered = render(&objects(&migrated()));
    let root = schema_dir();
    if std::env::var("MISTARR_WRITE_SCHEMA").is_ok_and(|v| v == "1") {
        write_dir(&root, &rendered);
        return;
    }
    let on_disk = read_dir_files(&root);
    let mut problems = Vec::new();
    for (name, text) in &rendered {
        match on_disk.get(name) {
            None => problems.push(format!("missing: {name}")),
            Some(found) if found.as_ref() != Some(text) => {
                problems.push(format!("different: {name}"));
            }
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
    let all = objects(&migrated());
    assert!(all.iter().any(|o| o.name == "title_search"));
    for suffix in ["data", "idx", "content", "docsize", "config"] {
        let shadow = format!("title_search_{suffix}");
        assert!(!all.iter().any(|o| o.name == shadow), "{shadow} listed");
    }
    assert!(all
        .iter()
        .filter(|o| o.kind == "index")
        .all(|o| !o.name.starts_with("sqlite_") && !o.sql.is_empty()));
    let files = render(&all);
    assert!(files.contains_key("tables/title_search.sql"));
    assert!(files.keys().all(|n| !n.contains("sqlite_")));
}
