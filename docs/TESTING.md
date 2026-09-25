# Testing

Three layers. All of them run on x86-64 in CI; only the last needs a board.

## 1. Unit tests, per crate

- `mistarr-core`: DAT parsing against synthetic DATs generated in the test,
  name parsing corpus, header rules, hashing against known vectors, 1G1R
  selection tables, cue parsing.
- `mistarr-mister`: every adapter's `plan_placement` and `accepts` against
  synthetic inputs; MRA parsing; DAT-name to platform binding; MGL building
  and escaping, core selection and the command FIFO against a real FIFO in a
  temporary directory.
- `mistarr-sources`: bencode parsing, binding score computation, name
  normalisation.
- `mistarr-clients`: each RPC implementation against a recorded fake that
  asserts the exact calls, including the 409 handshake and SCGI framing.

No test may contain a commercial title name, a real hash of a commercial dump,
or a real infohash. Fixtures are generated.

## 2. Integration tests: the synthetic set

The `mistarr-fixture` crate builds everything at test time; nothing is
checked in. `mistarr_fixture::set::generate` writes:

- A fake platform "Test Console" bound to NES: a DAT of 48 entries with an
  explicit clone group across regions and a revision, an inferred group with
  a beta, a revision group, a BIOS entry that has no file, and a bad dump
  whose file in the set does not match its DAT hashes.
- A disc DAT bound to `PlayStation` with a two-track game (cue plus two bins)
  and a single-track one, which brings the set to 50 entries.
- The files themselves: deterministic pseudo-random bytes per entry, sized
  from 8 KiB to 2 MiB, some zipped, some with an iNES header and the rest
  headerless with the header in the DAT. The output is identical on every run.

`crates/mistarr-server/tests/e2e.rs` then runs the flow a user performs,
once per installed client (`transmission-daemon`, `rtorrent`), each test
skipped with a message when its client is missing:

1. Start the fixture tracker on `127.0.0.1` and build a `.torrent` of each
   set against it. Transmission refuses loopback peers, so the tracker hands
   out loopback announcers as the host's own address.
2. Start one client that seeds both sets from the fixture directory and a
   second one, with its own ports and session, that the app drives.
3. Boot the app on a temporary data directory, drop both DATs into `dats/`
   and wait for `dat.loaded`, drop both torrents into `sources/` and wait for
   them to bind.
4. Want three titles through the API: the 1G1R pick of the clone group (a
   zipped, headerless file), the disc game and the bad dump.
5. Wait for the downloads to pass `importing` and settle, then check the
   placement in `games/NES/` and `games/PSX/<title>/` under canonical names,
   the bad dump in `staging/quarantine/<infohash>/` with its report, the
   `import.done` events and the import log.
6. Check that both torrents left the client under seed policy `none`, that
   `POST /system/scan` finds every placed file verified, that naming a core in
   `CORENAME` sets the client's download limit to the core limit and `MENU`
   lifts it, read over the client's own RPC, and that a restart on the same
   data directory keeps downloads, sources, imports and counts.

Run it locally with either client installed:

```sh
sudo apt-get install -y transmission-daemon rtorrent
make e2e
```

`make e2e` runs `cargo test -p mistarr-server --test e2e -- --nocapture
--test-threads=1` with `MISTARR_E2E_LOGS=target/e2e-logs`, where each
client's log and the server's `mistarr.log` land. Set
`MISTARR_E2E_REQUIRE=transmission-daemon,rtorrent` to fail instead of skip
when a client is missing. Each test prints its timings on success.

The fixture tool runs on its own too:

```sh
cargo run -p mistarr-fixture -- set ./out
cargo run -p mistarr-fixture -- tracker --loopback-as auto
```

## Memory budget

`crates/mistarr-server/tests/memory.rs` holds each heavy job to the peak RSS
budget in ARCHITECTURE.md "Resource budgets". Every test generates its
inputs in a temporary directory, starts the built `mistarr` binary on it as
a child process, waits for the job through the database, and reads the
child's `VmHWM` from `/proc/<pid>/status`, so one job's peak never counts
against another and the numbers include everything the real daemon runs.

| Test | Input | Asserts |
|---|---|---|
| `arcade_catalogue_stays_under_budget` | `_Arcade` with 1 000 MRAs of 24 parts, 50 under `_alternatives`, 5 hard links, every tenth MRA in mixed tag case, one MRA repeating a 32 MiB part four times, and an `_Organized` tree of 1 000 folders holding 15 000 symlinks to them; a zip per MRA | every MRA stored and its md5 matched, each distinct MRA read once, and an unchanged rerun reads and checks none |
| `db_export_import_stays_under_budget` | a zipped No-Intro DB export of 16 MB of XML, about 13 000 NES games with two sources of a headered and a headerless file and a save item each, some bad dumps, and clone groups of three | every game stored with one headerless rom, every clone linked |
| `large_inline_mras_stay_under_budget` | `_Arcade` with 16 MRAs of about 3 MB each, one inline `<part>` of 1 MiB beside a zipped part under one md5, all in one catalogue batch | every MRA stored and its md5 matched, each read once |
| `arcade_presence_pass_stays_under_budget` | 30 000 zips of 10 members under `games/mame` and 3 000 MRAs naming every tenth, one in ten also naming an absent zip | one `files` row per named zip on disk, the catalogue with its presence pass done within 20 seconds on the host |
| `dat_and_torrent_import_stay_under_budget` | a 50 MB Logiqx DAT of about 200 000 games, then a torrent of 50 000 files named after them, every tenth only loosely (`example_game_<n>.nes`) so the fuzzy tier reads thousands of roms per size; then a start with the source's stamp stale, so `remap_sources` works out every file again | every game stored, every torrent file stored, the rest matched by name, no candidate for the ambiguous loose names, and the re-map keeps the same matches |
| `scan_stays_under_budget` | 16 000 loose and 2 000 zipped GBA files and 1 000 PSX folders of a cue and a bin | a `files` row per file and zip member |
| `a_tiny_memory_limit_is_raised_to_the_floor` | `[memory] data_limit_mib = 2` | the process runs with the 64 MiB floor, or a lower inherited limit |

Each server started also checks that `/proc/<pid>/limits` shows the default
192 MiB data limit, or the lower limit the test run inherited, and that its
SQLite temporary directory exists. Besides the 64 MiB budget, each job's peak
must stay within 12 or 16 MiB of a server that runs no job, measured once
per run, so a regression shows before it reaches the budget; the two DAT
loads, whose apply runs with the writer's 8 MiB bulk cache, within 28 MiB. The suite runs in `cargo test --workspace` in debug
builds and takes about a minute; the budget holds there on x86-64 with room
to spare, and a release build for armv7 needs less, with half the pointer
size and a smaller binary. `make memory` runs it one test at a time and
prints each peak:

```sh
make memory
```

## Writes to the card

`crates/mistarr-server/src/jobs/dat_import/sync_writes.rs` counts the write
syscalls a DAT load and its recompute make, from `/proc/thread-self/io`, with
the writer's temporary tables in memory as the board keeps them in RAM, so
only database and WAL writes count (ARCHITECTURE.md "Writes on a sync
mount"). `load_writes_alone` loads 450 disc games on a tenth of the synthetic
catalogue and fails above 1 500 writes for the load or 200 for the
recompute, which catches a writer that spills before its commit or a rom
index a load need not touch; the test that runs in CI starts it in a test
process of its own. The ignored `sync_writes_on_the_bench_catalogue` prints
the counts on the full catalogue:

```sh
cargo test -p mistarr-server --lib sync_writes_on_the_bench -- --ignored --nocapture
```

## Browse speed

`crates/mistarr-server/src/synth.rs` generates the catalogue every query
test uses: DAT-sized sets for every major platform (disc sets with a cue and
several tracks, headered cartridge sets, 2 500 arcade MRAs and an add-on
family), about 42 000 titles and 82 000 roms at full scale, in clone groups
of three variants on average with regions, languages, revisions, flags and
some files in each state. Names are synthetic words from Zipf-skewed
syllables, so common trigrams such as `the`, `sta` and `man` are common on
every platform; two placed words are rare everywhere and common everywhere
but the browsed platforms. The plan test, the browse benchmark, the random
write test and `mistarr bench-seed` all use it.

`crates/mistarr-server/tests/browse.rs` seeds the full catalogue and checks
the default page equals the reference aggregation query's and is at least
five times faster. It then times every search shape (`titles::SearchShape`)
on the NES, SNES and PSX sets for a rare word, common trigrams, two-letter
terms below the trigram length, a word common elsewhere but rare on the
browsed platform, and no search, asserts every shape returns the same page
and total, and prints the table. The default shape's worst case must stay
under 100 ms, in debug builds too. Host timings do not rank the shapes the
way the board does, so the default follows the real-DAT board numbers in
ARCHITECTURE.md "Resource budgets", and the plan tests assert it asks
`title_search` for the platform's phrase:

```sh
cargo test --release -p mistarr-server --test browse -- --include-ignored --nocapture
```

The ignored test in the same file times a 15 000-game DAT load with the
group refresh and search index, without the index, and with neither, and
prints the on-disk size of the index, the group table and the flag, region
and language tables.

On the board, two hidden subcommands measure the same queries. `bench-seed`
writes the synthetic catalogue into a new file and refuses a path that
exists, so it never touches an install's database; `bench-search` opens any
database file read-only, with the server's 1 MiB reader cache, and prints the
groups matched and the min, median and 95th percentile time of each shape
(`--shape like|fts|fts-platform`, repeatable; every shape when omitted):

```sh
mistarr bench-seed --db /tmp/bench.db            # --scale 0.5 for half the size
mistarr bench-search --db /tmp/bench.db --platform psx --term sta --iterations 50
mistarr bench-search --db /media/fat/mistarr/mistarr.db --platform snes --term the --shape like
```

`db::groups::tests` starts from a small synthetic catalogue with groups
split across platforms, runs random
sequences of writes to titles, roms, files, flags and regions through the
real triggers and commits, and after every commit compares the table, the
counts and every browse filter combination in every search shape with the
reference aggregation query kept in the test. `db::plans` prints the query
plan of every hot read and fails on a scan of a growing table.

The web e2e suite drives the Browse screen against the mock API with a
latency from localStorage `mistarr.mockDelayMs`: a number, or an object of
numbers keyed `search#page` or `search` with `*` as the default; negative
fails the request. It checks the loading bar, stale answers, a background
reload overtaken by a search, a failed next page and the error state.

## 3. End-to-end on a board with real, open-licensed content

For manual verification on a DE10-Nano, use homebrew whose licence permits
redistribution. The tester obtains it from the author's own release, not from
the repository, and builds the DAT and torrent locally with the fixture tool
(`cargo build --release -p mistarr-fixture`; it is never part of a release):

```sh
mistarr-fixture dat  --platform nes --name "Homebrew Test" ./roms > test.dat
mistarr-fixture torrent --tracker http://example.invalid:6969/announce ./roms > test.torrent
```

Suitable titles are those released by their authors under licences that
permit redistribution. Two examples with public source repositories:

- **Nova the Squirrel** (NES): code GPLv3, assets CC BY-NC-SA 4.0. Source and
  ROM at the author's GitHub repository `NovaSquirrel/NovaTheSquirrel`.
- **Nova the Squirrel 2** (SNES): same author and licensing, repository
  `NovaSquirrel/NovaTheSquirrel2`.

Packaged samples with licence evidence live in the `mistarr-samples`
repository. Internet Archive items also expose a `.torrent` per item, and items whose
metadata carries a Creative Commons or public domain licence are usable for
this purpose. Check the licence field on the item before using it. Do not add
item identifiers to this repository; the tester chooses.

The board run checks: placement into `games/NES` and `games/SNES`, loading in
the core from the MiSTer menu, RSS under budget during scan and transfer,
CORENAME pausing when a core is launched, and the Scripts menu entry.

## Performance checks

`cargo bench` in `mistarr-core` covers hashing throughput and DAT parse time.
CI records them; a regression over 20 percent fails the build. On the board,
`mistarr doctor` reports the same numbers for comparison.

## What CI runs

- `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test` on x86-64,
  which includes the memory budget suite above.
- The `e2e` job: installs `transmission-daemon` and `rtorrent`, runs
  `make e2e` with both required, and uploads `target/e2e-logs` on failure.
- `cargo zigbuild --target armv7-unknown-linux-musleabihf`, then `file`
  asserts "statically linked".
- `npm run build` and a size check on the gzipped bundle.
- A grep gate from PRINCIPLES.md: the tree must not contain magnet URIs,
  `announce` URLs, 40-hex infohashes outside test fixtures, or the domains of
  known ROM sites (the deny list itself lives in CI config, not in the docs).
