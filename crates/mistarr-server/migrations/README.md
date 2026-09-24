Numbered SQL migrations, `NNNN_name.sql`, applied in order at startup and
recorded in `schema_version`. `build.rs` picks up every file here; add a new
number rather than editing one that has shipped. `0001_initial.sql` is the
schema in `docs/DATA-MODEL.md` verbatim.
