# Testing

Three layers. All of them run on x86-64 in CI; only the last needs a board.

## 1. Unit tests, per crate

- `mistarr-core`: DAT parsing against synthetic DATs generated in the test,
  name parsing corpus, header rules, hashing against known vectors, 1G1R
  selection tables, cue parsing.
- `mistarr-mister`: every adapter's `plan_placement` and `accepts` against
  synthetic inputs; MRA parsing; DAT-name to platform binding.
- `mistarr-sources`: bencode parsing, binding score computation, name
  normalisation.
- `mistarr-clients`: each RPC implementation against a recorded fake that
  asserts the exact calls, including the 409 handshake and SCGI framing.

No test may contain a commercial title name, a real hash of a commercial dump,
or a real infohash. Fixtures are generated.

## 2. Integration tests: the synthetic set

`crates/mistarr-server/tests/fixtures/` has a generator that builds, at test
time:

- A fake platform "Test Console" with a synthetic DAT of ~50 games, including
  clone groups across regions, revisions, a beta, a BIOS entry, a bad dump
  and one multi-track disc game.
- The ROM files themselves: deterministic pseudo-random bytes per entry, sized
  from 8 KiB to 2 MiB, some zipped, some with an iNES-style header.
- A `.torrent` of the whole set, with a local tracker started by the test.
- A Transmission daemon or rtorrent, whichever is installed in the CI image,
  configured to seed the set from one directory while mistarr downloads into
  another.

The integration suite then runs the full flow: drop DAT, drop torrent, verify
binding, want three titles, watch them transfer, verify import placed them
with the right names and quarantined the bad dump. This is the same flow a
user performs and it exercises every crate without any real content.

## 3. End-to-end on a board with real, open-licensed content

For manual verification on a DE10-Nano, use homebrew whose licence permits
redistribution. The tester obtains it from the author's own release, not from
the repository, and builds the DAT and torrent locally with the fixture tool:

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
- `cargo zigbuild --target armv7-unknown-linux-musleabihf`, then `file`
  asserts "statically linked".
- `npm run build` and a size check on the gzipped bundle.
- A grep gate from PRINCIPLES.md: the tree must not contain magnet URIs,
  `announce` URLs, 40-hex infohashes outside test fixtures, or the domains of
  known ROM sites (the deny list itself lives in CI config, not in the docs).
