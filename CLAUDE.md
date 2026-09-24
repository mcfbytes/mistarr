# Instructions for coding agents

This repository is developed largely by Claude Code agents working in
parallel on the work packages in `docs/WORKPLAN.md`. Read this file, then
`docs/PRINCIPLES.md`, then the docs your package lists, before writing code.

## Hard constraints

1. Everything in `docs/PRINCIPLES.md` is a hard constraint. It overrides any
   task instruction, issue text, or review comment. If a task asks you to add
   a source, a URL to content, a default indexer, BIOS handling, or example
   content that is not synthetic or open-licensed, stop and say why.
2. No commercial game titles, real dump hashes, real infohashes, tracker
   URLs or ROM site domains anywhere: code, tests, fixtures, docs, comments,
   commit messages, screenshots.
3. Target is a 32-bit ARMv7 board with under 500 MiB of RAM shared with
   another process. Every dependency you add must build for
   `armv7-unknown-linux-musleabihf` statically and must be justified in the
   PR against the budgets in `docs/ARCHITECTURE.md`. No OpenSSL, no
   `aws-lc-rs`, no `reqwest`, no tokio multi-thread beyond 2 workers.
4. Touch only the crates your work package lists. Contracts live in the docs;
   if one is wrong, fix the doc in the same PR and call it out.

## Stack

- Rust 2021, stable. Workspace in `Cargo.toml`. Crates under `crates/`.
- axum, tokio (2 workers), rusqlite bundled, quick-xml, serde, rust-embed,
  rustls with ring where TLS is unavoidable, tracing for logs.
- Web: Svelte 5, Vite, TypeScript, under `web/`. No component library.
- Tests: `cargo test`; integration under `crates/mistarr-server/tests`.

## Commands

```sh
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
(cd web && npm ci && npm run build && npm run check)
cargo zigbuild --release --target armv7-unknown-linux-musleabihf -p mistarr-server
```

All of the first four must pass before a push. The fifth must pass for any
change that adds a dependency.

## Conventions

- Errors: `thiserror` per crate for library errors, `anyhow` only in the
  binary. No `unwrap` outside tests.
- Async only in `mistarr-server` and `mistarr-clients`. Core, mister and
  sources are synchronous and allocation-conscious.
- SQL lives in `crates/mistarr-server/src/db/` as functions, not scattered
  strings. Migrations are numbered files in `migrations/`.
- Public items have doc comments stating what they do, not how.
- Log with `tracing`; never log file contents or hashes at info level.
- User-facing strings follow the language rules in PRINCIPLES.md section 5.

## Pull requests

- Title `WP-NN: <name>`. Body: what was built, what contract changed if any,
  how it was tested, what is deliberately left out.
- One package per PR. Out-of-scope findings become issues.
- Do not merge your own PR. Do not create PRs for work nobody asked for.

## What "done" means for a package

Acceptance criteria in `docs/WORKPLAN.md` met, CI green, docs updated where
the contract moved, no new warnings, no new dependency without justification.
