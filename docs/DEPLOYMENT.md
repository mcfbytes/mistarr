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
(cd web && npm ci && npm run build)
cargo zigbuild --release --target armv7-unknown-linux-musleabihf -p mistarr-server
```

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
  rtorrent.rc, rtorrent.sock, rtorrent-session/   # only if mistarr started rtorrent
/media/fat/Scripts/mistarr.sh      # start/stop/status from the MiSTer Scripts menu
```

## Starting it

`Scripts/mistarr.sh` starts the daemon under `nice -n 10 ionice -c 3`, prints
the URL, and offers to enable start-at-boot by appending a line to
`/media/fat/linux/user-startup.sh`. On Buildroot_MiSTer the same script works,
and the image may additionally ship an init service; either way the script is
the documented path so both images behave the same for users.

Logs go to `/media/fat/mistarr/mistarr.log` and stderr. The file rotates at
2 MiB and keeps two generations, `mistarr.log.1` and `mistarr.log.2`. `RUST_LOG`
sets the level, `info` by default. No syslog dependency.

## Runtime checks on the board

`mistarr doctor` prints: binary is static, paths writable, free space,
detected client and its version, installed cores, CORENAME, memory available,
and the result of hashing 64 MiB of zeros for throughput (`--hash-mib N`
changes the size). It reads the same config as the server and needs no running
server. This is what a bug report should include.

## Releases

Tagged releases publish `mistarr-armv7.tar.gz` containing the binary and
`mistarr.sh`. No other artifacts. The release notes contain no links to
content of any kind (PRINCIPLES.md).
