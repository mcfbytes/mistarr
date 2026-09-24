# mistarr

A small, self-contained collection manager for [MiSTer FPGA](https://github.com/MiSTer-devel/Wiki_MiSTer/wiki)
that runs **on the DE10-Nano itself**. It reads the DAT files you give it, shows
you your collection per core with cover art, verifies every file against those
DATs, files verified games into the right `games/<Core>` directory with each
core's quirks handled, and can drive the torrent client that already ships with
the MiSTer Linux image to fetch missing entries from torrents you supply.

It is written in Rust and ships as one static ARMv7 binary with the web UI
embedded. It runs on the stock MiSTer image and on
[Buildroot_MiSTer](https://github.com/mcfbytes/Buildroot_MiSTer).

## What it does

- **Catalog.** Import No-Intro, Redump or any Logiqx-format DAT. Browse every
  title per installed core, grouped one-game-one-ROM with region and revision
  preferences, with box art loaded by your browser from the libretro thumbnail
  server. Nothing is stored on the SD card except the database.
- **Verify.** Hash what is already in `games/`, match it against the DATs, and
  show what is verified, unverified, misnamed or missing.
- **Place.** Rename and move verified files into the directory and format each
  MiSTer core expects: iNES headers, big-endian N64, multi-track discs kept
  together, Neo Geo romset layout, arcade zips beside their MRA.
- **Acquire.** Drop `.torrent` or `.magnet` files into a watched directory.
  mistarr binds each torrent to a platform by matching its file list against
  your DATs, and when you mark a title as wanted it asks Transmission or rtorrent
  to fetch just that file, verifies it, and files it.
- **Stay out of the way.** Hashing and transfers pause while a core is running,
  I/O is throttled for the SD card, and memory is budgeted for a board that has
  under half a gigabyte to share with MiSTer.

## What it deliberately does not do

mistarr contains no sources. It ships with no torrents, magnets, tracker
addresses, site definitions, DAT files or BIOS images, and it never suggests
any. Everything it acquires comes from files you placed in its watched
directories. Seeding is a per-source setting you choose. See
[docs/PRINCIPLES.md](docs/PRINCIPLES.md) for the design rules that keep it that
way and why they are not negotiable.

## Install

On the board, over SSH:

```sh
curl -fsSL https://github.com/mcfbytes/mistarr/releases/latest/download/install.sh | sh
```

`/bin/sh` on the stock MiSTer image is BusyBox and runs this as written; a
`bash` present on the board works too. From the MiSTer Scripts menu, copy
`install.sh` from a release to `/media/fat/Scripts/mistarr_install.sh` and run
it from there instead. See [docs/DEPLOYMENT.md](docs/DEPLOYMENT.md) for
upgrading, rollback and how releases are built.

## Status

Design stage. The architecture and work packages are written; the code is a
skeleton. Start with [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and
[docs/WORKPLAN.md](docs/WORKPLAN.md).

## Documentation

| Document | Contents |
|---|---|
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | Components, crate layout, runtime flows, resource budgets |
| [docs/PRINCIPLES.md](docs/PRINCIPLES.md) | Content-neutrality rules every contributor and agent must follow |
| [docs/DATA-MODEL.md](docs/DATA-MODEL.md) | SQLite schema and state machines |
| [docs/API.md](docs/API.md) | HTTP API and event stream |
| [docs/PLATFORMS.md](docs/PLATFORMS.md) | DAT to core directory table and per-core adapters |
| [docs/VERIFICATION.md](docs/VERIFICATION.md) | DAT parsing, hashing, matching, 1G1R selection |
| [docs/DOWNLOAD-CLIENTS.md](docs/DOWNLOAD-CLIENTS.md) | Transmission and rtorrent integration |
| [docs/UI.md](docs/UI.md) | Screens, states and the SPA build |
| [docs/DEPLOYMENT.md](docs/DEPLOYMENT.md) | Cross-compiling, installing on MiSTer, startup |
| [docs/TESTING.md](docs/TESTING.md) | Fixtures, open-licensed test content, end-to-end runs |
| [docs/WORKPLAN.md](docs/WORKPLAN.md) | Work packages for parallel development |
| [CLAUDE.md](CLAUDE.md) | Instructions for coding agents working in this repository |

## Licence

MIT. See [LICENSE](LICENSE). This software is provided as is, without warranty
of any kind. It is a file organiser and torrent-client front end; what you load
into it is your responsibility.
