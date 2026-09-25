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

Build, with the versions CI uses (Node 24, the toolchain in
`rust-toolchain.toml`):

```sh
cargo install --locked cargo-zigbuild@0.23.4
rustup target add armv7-unknown-linux-musleabihf
# zig 0.15.2 itself: the `zig` binary on PATH, or `pip install ziglang==0.15.2`
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
  mistarr.prev            # the previous binary, kept by install.sh
  mistarr.prev.ok         # present once the saved rollback set is complete
  mistarr.db              # SQLite, with mistarr.db-wal and mistarr.db-shm
  mistarr.db.prev         # the database before the last upgrade, with any
                          #   mistarr.db.prev-wal and mistarr.db.prev-shm
  mistarr.lock            # held by the running server; a second server exits
  mistarr.pid             # the daemon, written by Scripts/mistarr.sh
  supervisor.pid          # the script's restart loop
  dats/                   # watched: drop DATs here
  dats/loaded/            # moved here after import
  dats/rejected/          # with a .reason.txt beside each file
  sources/                # watched: drop .torrent / .magnet here
  sources/loaded/
  staging/                # client download dir
  staging/quarantine/     # hash mismatches, with report
  staging/.import/        # importer scratch, removed after each import
  rtorrent.rc            # only if mistarr started rtorrent
  rtorrent-session/       # the same
  client-start.log        # output of client start commands
  transmission/           # transmission-daemon started without an init script
/media/fat/Scripts/mistarr.sh      # start/stop/status from the Scripts menu
/media/fat/Scripts/mistarr.sh.prev # the previous launcher, kept by install.sh
/tmp/mistarr.start.lock        # held while a start runs; a reboot clears it
/tmp/mistarr/                  # SQLite's temporary files and the DAT stage, in RAM, mode 0700;
                               #   <data>/tmp/ on the card when it cannot be written,
                               #   is a symlink or is another user's (logged at warn);
                               #   MISTARR_TEMP_DIR names another directory
```

[DATS.md](DATS.md) explains the DAT formats mistarr loads, what happens to a
file dropped into `dats/` or uploaded on the DATs screen, and what each
rejection reason means.

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
that the binary is an ARM ELF executable, stops a running mistarr, saves the
database, installs the new binary and `mistarr.sh`, starts it again and waits
for it to answer.
`mistarr.toml` and the watched directories are never touched, and
`mistarr.db` is only copied, or put back from its copy by a rollback.

**Upgrading** is the same command run again; it replaces the binary and
`mistarr.sh` in place and keeps the database and config. The new version
applies any new migrations to `mistarr.db` when it starts, after which an
older binary cannot use that database.

Before anything is overwritten, with mistarr stopped, `install.sh` saves a
rollback set:

- `mistarr.db` as `mistarr.db.prev`, and `mistarr.db-wal` and
  `mistarr.db-shm`, when present, as `mistarr.db.prev-wal` and
  `mistarr.db.prev-shm`. A saved `-wal` or `-shm` the current database lacks
  is removed, so the saved files are always one consistent set. A fresh
  install with no database saves none.
- the binary as `mistarr.prev` and `Scripts/mistarr.sh` as
  `Scripts/mistarr.sh.prev`.

When `fuser` is available and shows a process still holding `mistarr.db` or
its `-wal`, the script names the process, restarts the installed version and
exits without changing anything. It then checks free space in the data
directory, read with `stat -f` so that only that filesystem is queried; where
`stat -f` is missing it falls back to `df`, which on some BusyBox builds
queries every mount and can stall on an unreachable network mount. It copies
each file with `cp` to a `.new` name beside its saved name, for example
`mistarr.db.prev.new`. Only once every copy has succeeded does it sync,
remove `mistarr.prev.ok`, rename the copies over the old set, sync, and write
`mistarr.prev.ok` again, so a set cut short by a power loss is never
trusted. When the space is short or a copy fails, it removes the `.new`
files, restarts the installed version and exits; the binary, the launcher,
the database and the earlier rollback set are left as they were. Only one
previous version is kept: each upgrade replaces the saved set, so installing
twice leaves only the version immediately before the current one. The script
prints where the backup is and the manual rollback steps.

**Rollback**: after starting the new version, `install.sh` asks the new
binary for its listen address (`mistarr listen-addr`, which reads
`mistarr.toml` as the server does), uses the loopback address when that is
`0.0.0.0` or `[::]`, and falls back to port 8420 on loopback when the binary
gives no clean answer. It waits up to 180 s (`MISTARR_START_TIMEOUT`) for the
server to answer HTTP there.

Migrations run before the server listens, so a large one on the board can
take longer than that: every page it writes is flushed through the card's
`sync` mount (ARCHITECTURE.md "Writes on a sync mount"). Migration 17, which
rebuilds three rom indexes, writes about 3 400 pages to the database and
WAL for a 65 MB database with every rom keyed, about 90 s at 25 ms a write.
While migrations run, the server keeps `mistarr.migrating` in the data
directory, rewritten every 5 s with the versions and the process's I/O bytes
and CPU ticks, and removes it once they are applied; the log says
"migrating the database". While that file exists and keeps changing, the
script waits on, printing it every 30 s, and the 180 s count starts again
once it is gone. A migration that leaves the file unchanged for 300 s
(`MISTARR_PROGRESS_TIMEOUT`), or that runs past two hours
(`MISTARR_MIGRATE_TIMEOUT`), counts as a failed start, as does a server that
exits. If the new version fails to start, stops being reported running,
stalls, or does not answer in time, the script stops it and puts back `mistarr.prev` and
`mistarr.sh.prev`, and, when the new version was started, the saved
database set. It restores only a set marked by `mistarr.prev.ok`. The
database files are copied to `.restore` names, synced and moved into place;
with no room for those copies, it deletes the live `-wal` and `-shm` and
copies the saved files straight over the live ones, leaving the saved set
intact. If the database cannot be put back, the binary and launcher are
still restored but the previous version is not started, and the saved set
is left for a manual restore. A crash after the wait is not rolled back: the
supervisor restarts the new version.

A rollback across a migration needs the database restored with the binary.
A binary refuses a database whose recorded schema version is newer than its
own migrations, with an error naming both versions, and leaves its contents
unchanged; `mistarr doctor` reports the same. To roll back by hand, in
`/media/fat/mistarr`, and only when `mistarr.prev.ok` is present:

1. Stop mistarr: `/media/fat/Scripts/mistarr.sh stop`.
2. Copy `mistarr.prev` to `mistarr`, and `Scripts/mistarr.sh.prev` to
   `Scripts/mistarr.sh`.
3. Delete `mistarr.db-wal` and `mistarr.db-shm`, copy `mistarr.db.prev` to
   `mistarr.db`, and copy `mistarr.db.prev-wal` and `mistarr.db.prev-shm`, if
   present, to `mistarr.db-wal` and `mistarr.db-shm`.
4. Start it: `/media/fat/Scripts/mistarr.sh start`.

Anything changed in the database since the upgrade is lost by step 3. To go
back further than one version, install that version and restore a database
copy of your own made while it was installed.

## Starting it

`Scripts/mistarr.sh` starts the daemon under `nice -n 10`, prints
the URL, and offers to enable start-at-boot by appending a line to
`/media/fat/linux/user-startup.sh`. On Buildroot_MiSTer the same script works,
and the image may additionally ship an init service; either way the script is
the documented path so both images behave the same for users.

Logs go to `/media/fat/mistarr/mistarr.log`, and to stderr unless stderr is
that same file: the script appends the daemon's output to the log so that
early startup errors land there, and each event is still written once. The
file rotates at 2 MiB and keeps two generations, `mistarr.log.1` and
`mistarr.log.2`. `RUST_LOG` sets the level, `info` by default. No syslog
dependency.

Only one server runs per data directory. The server takes an exclusive lock
on `mistarr.lock` before opening the database and exits with "another
mistarr is already running" when it cannot; on a filesystem without file
locks it logs a warning and runs unlocked. `mistarr.sh start` also
serialises itself through `/tmp/mistarr.start.lock`, created exclusively and
holding the starting shell's pid. A lock whose pid is gone, or is not a
`mistarr.sh` process, is replaced by renaming a new file over it and kept
only if it still names this start a second later. A start from
`user-startup.sh` racing a manual start from the Scripts menu therefore
launches one daemon. `mistarr.pid` is removed only once the process it
names has exited or runs something other than the binary.

`mistarr.sh start` runs the daemon under a supervisor, a background copy of
the script whose pid is in `supervisor.pid`. When the daemon exits with a
non-zero status or a signal, the supervisor logs it to `mistarr.log` and
starts it again after 5 s, doubling the wait on each crash up to 5 minutes.
After 5 crashes within 10 minutes it logs that it gives up and exits. A
clean exit is not restarted. `stop` ends the supervisor first, then the
daemon; `status` reports a supervisor waiting to restart.

`[jobs] scan_interval_minutes` in `mistarr.toml` defaults to 1440: a daily
rescan of the whole library. Set it to 0 to disable the timer and rely on the
automatic and manual scans instead; the change needs a restart.

## Runtime checks on the board

`mistarr doctor` prints: binary is static, paths writable, free space,
detected client and its version, whether `rtorrent` is on `PATH`, installed
cores, CORENAME, memory available, and the result of hashing 64 MiB of zeros
for throughput (`--hash-mib N` changes the size), and whether the
`title_groups` table and search index match the catalogue. It also prints the
database's schema version against the highest this binary supports, and says
so, without opening the database for writing, when a newer mistarr migrated
it. It reads the same config as the server and needs no running server. This
is what a bug report should include. When it reports title groups out of
step, stop the server and run `mistarr doctor --rebuild-groups` to recompute
them.

## Releasing

A release publishes `mistarr-armv7.tar.gz` (the binary, `mistarr.sh` and
`install.sh`), `mistarr-armv7.tar.gz.sha256`, and `install.sh` on its own so
it can be fetched directly. No other artifacts, and the release notes contain
no links to content of any kind (PRINCIPLES.md).

**From the GitHub web UI**: open Releases, choose "Draft a new release",
create a new tag `vX.Y.Z` for it, and publish. The release's own body is used
as written; CI builds the binary and attaches the three assets to it.

**From the command line**: push an annotated tag, for example
`git tag -a v1.2.3 -m "v1.2.3" && git push origin v1.2.3`, then publish a
release for it with the GitHub CLI:
`gh release create v1.2.3 --verify-tag --notes-from-tag`, which uses the
tag's message as the release body.

The workflow runs only when a release is published, so each release builds
once. Pushing a tag alone creates no release and starts no build.
