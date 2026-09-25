# Work plan

Work packages are sized so that one agent can complete one in a single
session against the contracts in ARCHITECTURE.md, DATA-MODEL.md and API.md.
Each has a branch name, the crates it may touch, its dependencies, and
acceptance criteria that CI can check. Packages in the same wave have no
dependencies on each other and are meant to run in parallel.

Rules for every package:

- Read CLAUDE.md and docs/PRINCIPLES.md first. Both are binding.
- Touch only the crates listed. If a contract in the docs is wrong, change the
  doc on the same branch and say so in the commit message; do not silently diverge.
- Every public function has a unit test. Fixtures are synthetic.
- `cargo fmt`, `cargo clippy -D warnings`, `cargo test` green before pushing.
- Work on branch `wp-NN-<short-name>` from `master`, push it, and stop. The
  orchestrator opens the pull request, reviews it and merges it.
- Comments two lines maximum, no history in files, Rust practices from
  CLAUDE.md.

## Model sizing

Each package names the smallest model that can complete it reliably. The
orchestrator runs reviews with a stronger model than the one that wrote the
code. Guidance:

| Model | Use for |
|---|---|
| Opus | Parsers with many edge cases, protocol clients, adapters with per-platform quirks, server assembly, anything touching the DownloadClient or CoreAdapter contracts |
| Sonnet | Well-specified pure logic with a clear test table, the SPA shell, hashing, build tooling |
| Haiku | Mechanical work: CI config, grep gates, scripts, doc table updates, renames |

## Wave 0: foundation

- WP-00 Workspace skeleton, docs, CI config stub.

## Wave 1: core contracts, no I/O

| WP | Name | Model | Crates | Acceptance |
|---|---|---|---|---|
| WP-01 | DAT parser and name parser | Opus | `mistarr-core` | Parses a synthetic Logiqx DAT and a zipped pack; name corpus of at least 200 synthetic names parses into the documented fields; unknown tags never fail. |
| WP-02 | Hashing and header rules | Sonnet | `mistarr-core` | One-pass CRC32/MD5/SHA1 matches reference vectors; each header rule from PLATFORMS.md tested; zip central-directory pre-check; bench in place. |
| WP-03 | 1G1R and clone inference | Sonnet | `mistarr-core` | Selection table from VERIFICATION.md reproduced in tests; deterministic ties; inferred groups flagged. |
| WP-04 | Platform table and adapters | Opus | `mistarr-mister` | Every row in PLATFORMS.md has an adapter; `plan_placement` tested per row including N64 byte order, SMC header, disc directories; rows marked verify carry a `#[ignore]` board test and a tracking issue. |
| WP-05 | Torrent parsing and binding | Sonnet | `mistarr-sources` | Bencode parser for .torrent v1 and hybrid; magnet parsing; binding score against synthetic DATs with threshold behaviour; name normalisation matches VERIFICATION.md. |
| WP-06 | Client trait and Transmission | Opus | `mistarr-clients` | Trait as in ARCHITECTURE.md; Transmission implementation passes a recorded-fake test for every operation including 409 handshake and the >2000 files path. |
| WP-07 | rtorrent client | Opus | `mistarr-clients` | SCGI framing tested byte-exact; every operation against a recorded fake; multicall chunking; ratio policy via poll. |
| WP-08 | SPA shell | Sonnet | `web/` | Vite project, router, theme, SSE store, API client generated from API.md, all screens as static mocks with fixture data, bundle size check. |

## Wave 2: server assembly

| WP | Name | Model | Crates | Depends | Acceptance |
|---|---|---|---|---|---|
| WP-09 | Server, DB, migrations, config | Opus | `mistarr-server` | 01 | axum app boots, migrations from DATA-MODEL.md apply, config parses, `/system/status` and `/events` work, SPA embedded. |
| WP-10 | DAT import job and catalog API | Opus | `mistarr-server` | 01, 03, 09 | Watched `dats/` flow end to end; `/platforms`, `/platforms/{id}/titles`, `/titles/{id}` return documented shapes; supersession tested. |
| WP-11 | Library scan job | Sonnet | `mistarr-server` | 02, 04, 09 | Incremental scan with resumable progress; states per VERIFICATION.md; `file.changed` throttling; CORENAME gate honoured. |
| WP-12 | Source import and binding job | Opus | `mistarr-server` | 05, 09 | Watched `sources/` flow; magnet resolving via client; `/sources` API; unbound picker. |
| WP-13 | Want, transfer and poll | Opus | `mistarr-server` | 06, 07, 09, 12 | Want to transferring to checking with per-file progress over SSE; rate limits switch with CORENAME; remote path map applied. |
| WP-14 | Importer | Opus | `mistarr-server` | 02, 04, 11, 13 | Hash, match, quarantine, plan, place, log; same-filesystem rename asserted; existing-verified kept. |
| WP-15 | SPA wired to live API | Sonnet | `web/` | 08, 09 | Every screen against the running server; wizard completes a first run; phone width verified with a Playwright screenshot per screen. |

## Wave 3: hardening and release

| WP | Name | Model | Depends | Acceptance |
|---|---|---|---|---|
| WP-16 | Integration suite with synthetic set and local tracker | Opus | 10 to 15 | TESTING.md layer 2 passes in CI against Transmission and rtorrent. |
| WP-17 | Cross build, `doctor`, scripts, release workflow | Sonnet | 09 | Static armv7 artifact produced by CI; `mistarr.sh`; `doctor` output; `file` gate. |
| WP-18 | Board verification | Opus | 16, 17 | The verify rows in PLATFORMS.md confirmed or corrected on a DE10-Nano; RSS and throughput recorded in DEPLOYMENT.md. |
| WP-19 | Arcade and Neo Geo adapters | Opus | 04, 14 | MRA-driven wanted list; romset placement; verification against MRA md5. |
| WP-20 | Principles gate in CI | Haiku | none | The grep gate from TESTING.md, with the deny list in CI config only. |
| WP-21 | Sample set | Sonnet | 14 | The `mistarr-samples` repository holds open-licensed entries with DATs, torrents and licence evidence; every entry documented per its README rules. |
| WP-22 | Install script and release trigger | Sonnet | 17 | `install.sh` installs or upgrades on the board from a GitHub release with checksum verification and rollback; the release workflow runs from a release created in the web UI or a pushed tag. |
| WP-23 | Show-hidden browse semantics | Sonnet | 10, 15 | `hidden=show` disables the hide list independently of `flags`; the Browse screen has a Show hidden checkbox and a require-flags selector. |
| WP-24 | Import wanted MRA zips | Opus | 14, 19 | A wanted arcade zip is placed whole and verified by MRA md5 assembly, by MAME DAT members, or recorded unverified; multi-zip MRAs complete in any order. |
| WP-25 | Scan automation and wizard cores | Sonnet | 11, 19 | Scans run after a DAT loads for a platform with a games directory, once when the wizard completes, and daily by default; the wizard's cores step calls `POST /system/cores`. |
| WP-26 | Launch games and cores | Opus | 14, 19, 25 | `POST /titles/{id}/launch` starts an entry in the collection through an MGL or its MRA and `POST /platforms/{id}/launch-core` starts the newest core, via a non-blocking write to MiSTer Main's FIFO; 404, 409 and 503 as API.md says; `prefs.launch` turns it off; the Title and Browse screens offer Play and Start core with an inline reason when unavailable. |
| WP-27 | First board test fixes | Opus | 10, 12, 13, 15, 17 | DAT and source parsing run in an ungated background lane while a core is loaded, and held heavy jobs are listed with a Run now action; the wizard and Sources list the files in `dats/` and `sources/` with their state live; an installed but stopped client can be started and is re-detected; path mappings can be removed and blank ones are refused; the wizard opens once; unbound sources carry a suggested platform and rebind when its DAT loads; each log line is written once and a second instance refuses to start. |
| WP-28 | Memory budget at scale | Opus | 10 to 14, 19 | `tests/memory.rs` holds the arcade catalogue (1 000 MRAs plus an `_Organized` tree of 15 000 symlinks), a 50 MB DAT, a 50 000-file torrent and a 20 000-file scan under 64 MiB peak RSS, each in its own process; the catalogue reads each distinct MRA once and an unchanged rerun reads none; MRA markup is read case-insensitively; `[memory] data_limit_mib` sets a soft `RLIMIT_DATA` at startup. |
| WP-29 | Arcade scan and counts | Sonnet | 11, 19, 24, 25 | A library scan, its fan-out and the DAT-triggered auto-scan skip the arcade platform, identified by `Platform::is_arcade`; a manual `POST /system/scan` of `arcade` queues only the arcade catalogue job; the catalogue's presence pass gives each `games/mame`/`games/hbmame` zip a live MRA names one row to promote, from a stat alone, never downgrades import rows of an unchanged zip, and prunes `files` rows whose zip is gone, within budget on 30 000 zips; a migration deletes arcade rows a library scan wrote with no rom; `Counts` reports `unmatched_files` for a scan's unmatched files on disk (never arcade) and separate group-based `failing_check`/`partial` arcade-catalogue states; the Platforms screen shows `have · wanted · titles` plus any nonzero extra clause. |
| WP-30 | DAT export format, DAT screen, large MRAs | Opus | 10, 15, 19, 28 | A No-Intro DB export, plain or zipped, loads as the same game and rom model as a Logiqx DAT, with parents from archive numbers, headerless roms for header-rule platforms named `<game>.<ext>`, and its name from the file name; a proptest holds both forms equal; unknown XML is rejected naming both forms; a DATs screen uploads DATs, lists incoming files with rejection reasons, retries or deletes a rejected file, lists loaded versions grouped by DAT family and removes a loaded version after a confirmation; a load supersedes only the current version of its DAT family on its platform, in either form, and titles two live families list with the same roms share a clone group; MRAs up to 16 MiB are read with inline part data streamed, never held; `tests/memory.rs` holds a 16 MB export and 16 MRAs of about 3 MB in one batch under budget. |
| WP-31 | Fuzzy torrent-to-rom mapping | Opus | 12, 13, 14, 15, 28 | A torrent file unmatched by name gets fuzzy (same size, shared significant words) and narrow size-only candidates in `torrent_candidates`, one file serving several roms; want picks by confidence; a file that hashes to another version of the wanted entry is placed as that version and the wanted download ends `bad` saying so; the title lists each file with its confidence; the Nova case passes end to end and the 50 000-file torrent stays under budget. |
| WP-32 | Fast browse and search | Opus | 10, 15, 23 | `title_groups` is a table keyed on `titles.group_root` that the same transaction refreshes through `db::commit` in bounded chunks, with a doctor check, `--rebuild-groups`, a migration that builds it and a proptest against the reference aggregation query; titles' flags, regions and languages are rows of their own tables and nothing filters on JSON; jobs dedupe on `jobs.subject`; search takes the trigram FTS5 shape filtered by a sentinel-wrapped platform column, the best worst case on the board with real DATs, and finds groups whose root title is on another platform; every shape returns the same rows on a synthetic catalogue of every major platform's DAT; hidden `bench-seed` and `bench-search` measure the same on the board; every hot read seeks through an index, asserted by a plan test; the Browse screen debounces search, cancels stale requests and reloads, never skips a page after a failure, and shows loading and error states. |
| WP-33 | Safe rollback | Sonnet | 22 | `install.sh` saves `mistarr.db` and any `-wal`/`-shm` with the binary and launcher as a rollback set after stopping mistarr and before overwriting anything, through `.new` copies renamed over the old set only when all succeeded; it refuses a database another process holds, skips the database on a fresh install, checks free space, and on a failed copy aborts leaving the earlier set intact; it marks a complete set with `mistarr.prev.ok` and restores only a marked one; it waits for the new version to answer HTTP at the address the binary's hidden `listen-addr` reports, and restores the binary and launcher, plus the database once the new version has run, through `.restore` copies or a direct copy when space is short, when it exits, fails or times out; a binary refuses a database whose schema version is newer than its migrations, naming both versions, leaving its contents unchanged, and `doctor` reports it; the shell harness covers the stop order, the backup, the fresh install, slow and failed starts, a failed restore and failed backups. |
| WP-34 | Platform art | Opus | 15 | Every platform id draws original SVG art from its motif family, falling back by kind, with a stylised drawing of its hardware in front that PRINCIPLES.md section 8 allows, and a generic keyboard computer, cabinet or console box for ids without their own; the same id draws the same art, and siblings differ in hue and composition; focal elements stay visible at every banner aspect; no image files, network, logos, printed text or brand colours; dark and light variants; the Platforms cards and the Browse header show it `aria-hidden` behind a scrim that keeps AA contrast; under 120 SVG elements per tile and 60 for its hardware; e2e checks each card's art and its determinism. |
| WP-35 | Match files already on disk | Opus | 11, 30 | Loading a DAT or changing a platform's titles matches the platform's unmatched files from their stored hashes without reading them, arcade aside, paging by file id so a file that stays unmatched is read once; a rescan matches an unchanged unmatched file from its stored hashes without hashing it; the CRC32 tier of a stored match allows for the header a platform's rule strips; a scan's outcome is in its progress; Activity lists recent finished jobs from `GET /system/jobs/recent` and the Scan button says it was queued and then what the scan found. |

## Suggested fan-out

Wave 1 is eight independent agents. Wave 2 needs WP-09 first, then six in
parallel with the listed dependencies. Wave 3 is sequential except WP-19 and
WP-20, which can start any time after their dependencies.

## Review and merge

When a branch is pushed the orchestrator opens a pull request, runs a code
review at high effort, applies or requests fixes, and merges the pull request
with a merge commit once CI is green. Findings outside the package's scope
become new work-plan rows, not scope extensions.
