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

- `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test` on x86-64.
- The `e2e` job: installs `transmission-daemon` and `rtorrent`, runs
  `make e2e` with both required, and uploads `target/e2e-logs` on failure.
- `cargo zigbuild --target armv7-unknown-linux-musleabihf`, then `file`
  asserts "statically linked".
- `npm run build` and a size check on the gzipped bundle.
- A grep gate from PRINCIPLES.md: the tree must not contain magnet URIs,
  `announce` URLs, 40-hex infohashes outside test fixtures, or the domains of
  known ROM sites (the deny list itself lives in CI config, not in the docs).
