Numbered SQL migrations, `NNNN_name.sql`, applied in order at startup and
recorded in `schema_version`. `build.rs` picks up every file here; add a new
number rather than editing one that has shipped. `docs/DATA-MODEL.md` shows
the schema after every migration. A migration's pull request also regenerates
`crates/mistarr-server/schema/`, the exact current DDL; see its `README.md`.

A migration whose first line is exactly `-- foreign_keys: off` runs with foreign
keys off, and must add no row to `PRAGMA foreign_key_check`. To rebuild a
table: drop its old indexes, create the new table, `INSERT … SELECT … ORDER BY
id`, drop the old table, rename, recreate the indexes. Before the drop, drop the
triggers of other tables whose bodies name the rebuilt table and recreate them
after the rename, since a rename fails on a trigger naming a missing table.
