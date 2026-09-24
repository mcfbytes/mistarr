Numbered SQL migrations, `NNNN_name.sql`, applied in order at startup and
recorded in `schema_version`. `build.rs` picks up every file here; add a new
number rather than editing one that has shipped. `docs/DATA-MODEL.md` shows
the schema after every migration.
