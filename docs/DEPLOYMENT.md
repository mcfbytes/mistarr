# Building and deploying

## Target

`armv7-unknown-linux-musleabihf`, fully static. The stock MiSTer image has
glibc 2.31 and no package manager, so nothing may be dynamically linked.

`.cargo/config.toml` in the repo sets the target, the linker via
`cargo-zigbuild`, and the release profile:

```toml
[profile.release]
opt-level = "z"
lto = "fat"
codegen-units = 1
panic = "abort"
strip = true
```

Build:

```sh
cargo install cargo-zigbuild
rustup target add armv7-unknown-linux-musleabihf
# zig itself: the `zig` binary on PATH, or `pip install ziglang`
(cd web && npm ci && npm run build)
cargo zigbuild --release --target armv7-unknown-linux-musleabihf -p mistarr-server
```

Or `make cross`, which builds the SPA first and then runs the same
`cargo zigbuild` line; `make release` does that and also produces
`dist/mistarr-armv7.tar.gz`. `make check` runs the full local gate: fmt,
clippy, workspace tests, the web checks, and the principles gate. `make web`
builds only the SPA, and `make clean` removes `target/`, `dist/` and
`web/dist`.

The binary at `target/armv7-unknown-linux-musleabihf/release/mistarr` is
statically linked and stripped, measured at 2,985,296 bytes (2.85 MiB) with
the SPA embedded.

Dependencies must build for this target without a C toolchain surprise:
`rustls` with `ring`, never OpenSSL or `aws-lc-rs`; `rusqlite` with
`bundled`; no `reqwest` (the server only talks to localhost RPC; use `hyper`
client or `ureq` with rustls). CI builds the target on every push and fails on
any dynamic dependency, checked with `file` on the output.

## Layout on the SD card

```
/media/fat/mistarr/
  mistarr                 # the binary
  mistarr.toml            # optional config
  mistarr.db              # SQLite
  dats/                   # watched: drop DATs here
  dats/loaded/            # moved here after import
  dats/rejected/          # with a .reason.txt beside each file
  sources/                # watched: drop .torrent / .magnet here
  sources/loaded/
  staging/                # client download dir
  staging/quarantine/     # hash mismatches, with report
  staging/.import/        # importer scratch, removed after each import
  rtorrent.rc, rtorrent.sock, rtorrent-session/   # only if mistarr started rtorrent
/media/fat/Scripts/mistarr.sh      # start/stop/status from the MiSTer Scripts menu
```

## Installing on the board

Over SSH, one line fetches, verifies and installs the latest release:

```sh
curl -fsSL https://github.com/mcfbytes/mistarr/releases/latest/download/install.sh | sh
```

The stock MiSTer image's `/bin/sh` is BusyBox ash; the command above works
with it as written. A `bash` present on the board runs it too. Add a version
to install something other than latest: append it as an argument to a
downloaded copy of the script, for example `sh install.sh v1.2.3`; piped
through `sh -s`, the version goes after the dash: `... | sh -s v1.2.3`.

From the MiSTer Scripts menu: copy `install.sh` from a release to
`/media/fat/Scripts/mistarr_install.sh` and run it from the menu with no
argument, which installs the latest release.

Either way `install.sh` resolves the release through the GitHub API,
downloads `mistarr-armv7.tar.gz` and its `.sha256`, verifies the checksum and
that the binary is an ARM ELF executable, stops a running mistarr, installs
the new binary and `mistarr.sh`, and starts it again. `mistarr.db`,
`mistarr.toml` and the watched directories are never touched.

**Upgrading** is the same command run again; it replaces the binary and
`mistarr.sh` in place and keeps the database and config.

**Rollback**: before overwriting an existing binary, `install.sh` saves it as
`mistarr.prev` beside it. If anything after that point fails — the new binary
fails to start — the script restores `mistarr.prev` automatically. To roll
back by hand, stop mistarr, copy `mistarr.prev` over `mistarr`, and start it
again; then install whichever earlier version you need.

## Starting it

`Scripts/mistarr.sh` starts the daemon under `nice -n 10 ionice -c 3`, prints
the URL, and offers to enable start-at-boot by appending a line to
`/media/fat/linux/user-startup.sh`. On Buildroot_MiSTer the same script works,
and the image may additionally ship an init service; either way the script is
the documented path so both images behave the same for users.

Logs go to `/media/fat/mistarr/mistarr.log` and stderr. The file rotates at
2 MiB and keeps two generations, `mistarr.log.1` and `mistarr.log.2`. `RUST_LOG`
sets the level, `info` by default. No syslog dependency.

`[jobs] scan_interval_minutes` in `mistarr.toml` defaults to 1440: a daily
rescan of the whole library. Set it to 0 to disable the timer and rely on the
automatic and manual scans instead; the change needs a restart.

## Runtime checks on the board

`mistarr doctor` prints: binary is static, paths writable, free space,
detected client and its version, whether `rtorrent` is on `PATH`, installed
cores, CORENAME, memory available, and the result of hashing 64 MiB of zeros
for throughput (`--hash-mib N` changes the size). It reads the same config as
the server and needs no running server. This is what a bug report should
include.

## Releasing

A release publishes `mistarr-armv7.tar.gz` (the binary, `mistarr.sh` and
`install.sh`), `mistarr-armv7.tar.gz.sha256`, and `install.sh` on its own so
it can be fetched directly. No other artifacts, and the release notes contain
no links to content of any kind (PRINCIPLES.md).

**From the GitHub web UI**: open Releases, choose "Draft a new release",
create a new tag `vX.Y.Z` for it, and publish. The release's own body is used
as written; CI builds the binary and attaches the three assets to it.

**From the command line**: push a tag matching `v*`, for example
`git tag -a v1.2.3 -m "v1.2.3" && git push origin v1.2.3`. CI builds and
creates the release. An annotated tag's message becomes the release body; a
lightweight tag creates the release with no body.

Both paths run the same workflow and land on the same release for a given
tag, so publishing through the UI and then pushing the tag (or the reverse)
is safe.
