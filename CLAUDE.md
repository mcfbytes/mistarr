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
   commit message against the budgets in `docs/ARCHITECTURE.md`. No OpenSSL,
   no `aws-lc-rs`, no `reqwest`, no tokio multi-thread beyond 2 workers.
4. Touch only the crates your work package lists. Contracts live in the docs;
   if one is wrong, fix the doc in the same branch and call it out.

## Writing rules

- **Comments are two lines maximum.** A comment says what a non-obvious line
  does or why a constraint exists. Anything longer belongs in a doc comment on
  the item or in `docs/`. Module-level explanations go in `docs/`, referenced
  by path from a one-line `//!` header.
- **Doc comments** (`///`, `//!`) state what an item does and its invariants,
  in at most a short paragraph. Examples go in doctests, not prose.
- **No history in files.** Code and docs describe the current state only. No
  "previously", "changed from", "as of version", changelog sections, dated
  notes, or author names. Git holds the history.
- **No narration.** No TODO, FIXME or "this will be replaced" comments. Open
  work is an issue or a `docs/WORKPLAN.md` row.
- User-facing strings follow the language rules in `docs/PRINCIPLES.md`
  section 5.

## Rust practices

- Rust 2021, stable, `#![forbid(unsafe_code)]` and `#![warn(missing_docs)]`
  in every crate. `cargo clippy --all-targets -- -D warnings` is the bar,
  with `clippy::pedantic` enabled at the crate level and specific lints
  allowed only with a one-line reason.
- Errors: `thiserror` enums per crate with a variant per failure the caller
  can act on. `anyhow` only in the binary. No `unwrap` or `expect` outside
  tests and `const` contexts. No `panic!` on user input.
- Ownership: take `&str`, `&Path` and `&[T]` in signatures; return owned
  types. No `.clone()` to satisfy the borrow checker without a reason.
- Async only in `mistarr-server` and `mistarr-clients`. Core, mister and
  sources are synchronous, allocation-conscious, and stream through `Read`
  rather than loading files into memory.
- Newtypes for ids and hashes; no bare `String` or `i64` crossing a crate
  boundary as an identifier. `#[non_exhaustive]` on public enums that will
  grow.
- Every public function has a unit test. Property tests with `proptest` for
  parsers. Fixtures are synthetic and generated in the test.
- SQL lives in `crates/mistarr-server/src/db/` as typed functions, not
  scattered strings. Migrations are numbered files in `migrations/`.
- Log with `tracing`; never log file contents or hashes at info level.
- Prefer the standard library and small, well-maintained crates. Check a
  crate's size, transitive dependencies and ARMv7 support before adding it.

## Stack

- Workspace in `Cargo.toml`, crates under `crates/`.
- axum, tokio (2 workers), rusqlite bundled, quick-xml, serde, rust-embed,
  rustls with ring where TLS is unavoidable, tracing for logs.
- Web: Svelte 5, Vite, TypeScript, under `web/`. No component library.

## Commands

```sh
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
(cd web && npm ci && npm run build && npm run check)
cargo zigbuild --release --target armv7-unknown-linux-musleabihf -p mistarr-server
```

The first four must pass before a push. The fifth must pass for any change
that adds a dependency.

## Branch workflow

There are no pull requests. Each work package is developed on a branch named
`wp-NN-<short-name>` from `master`. When the package meets its acceptance
criteria and the commands above pass, the agent pushes the branch and stops.
The orchestrating session runs a code review on the branch, fixes or sends
back what the review finds, and merges into `master`. Agents never merge,
never push to `master`, and never rewrite another branch's history.

Commit messages: an imperative subject line under 60 characters, a body that
says what and why, no model names, no session narration.

## What "done" means for a package

Acceptance criteria in `docs/WORKPLAN.md` met, all commands green, docs
updated where the contract moved, no new warnings, no new dependency without
justification, no comment over two lines, no history in any file.
