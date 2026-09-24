# Platforms and core adapters

The table maps DAT header names to the directory each MiSTer core reads from
and to the adapter that handles the core's quirks. Directory names are the
ones MiSTer's core documentation lists; exFAT is case-insensitive so case is
cosmetic, but write them as shown. Entries marked **verify** were taken from
igir's mapping and must be confirmed against a live board before the adapter is
considered done (see WORKPLAN WP-04).

Platform ids are the stable slugs used in the database and API.

## Cartridge and handheld

| id | DAT name matches | core dir | ext written | adapter notes |
|---|---|---|---|---|
| `nes` | `Nintendo Entertainment System`, `NES` | `NES` | `.nes` | Use the **headered** No-Intro DAT. The core needs an iNES header. If only the headerless DAT is loaded, hash with the 16-byte header stripped for matching, but never strip on disk. |
| `fds` | `Famicom Disk System` | `NES` | `.fds` | Same directory as NES. Needs FDS BIOS: report only. |
| `snes` | `Super Nintendo Entertainment System`, `Super Famicom`, `Satellaview` | `SNES` | `.sfc` | Strip 512-byte copier headers on `.smc` when hashing and on disk. |
| `n64` | `Nintendo 64` | `N64` | `.z64` | Use the **BigEndian** DAT. Convert `.v64`/`.n64` to big-endian on placement. |
| `gb` | `Game Boy` (not Color/Advance) | `GAMEBOY` | `.gb` | **verify** dir name. |
| `gbc` | `Game Boy Color` | `GAMEBOY` | `.gbc` | Same core as GB. **verify** whether a `GBC` dir is honoured. |
| `gba` | `Game Boy Advance` | `GBA` | `.gba` | |
| `megadrive` | `Mega Drive - Genesis` | `Genesis` | `.md` | igir writes `MegaDrive`; current core reads `Genesis`. **verify** and accept both on scan. |
| `s32x` | `32X` | `S32X` | `.32x` | |
| `sms` | `Master System - Mark III` | `SMS` | `.sms` | |
| `gg` | `Game Gear` | `SMS` | `.gg` | Same core as SMS. |
| `sg1000` | `SG-1000` | `SG1000` | `.sg` | |
| `pce` | `PC Engine - TurboGrafx-16` | `TGFX16` | `.pce` | |
| `sgx` | `SuperGrafx` | `TGFX16` | `.sgx` | |
| `atari2600` | `Atari 2600` | `Atari2600` | `.a26` | |
| `atari5200` | `Atari 5200` | `Atari5200` | `.a52` | |
| `atari7800` | `Atari 7800` | `Atari7800` | `.a78` | Headered DAT. |
| `lynx` | `Atari Lynx` | `AtariLynx` | `.lnx` | Headered DAT. |
| `coleco` | `ColecoVision` | `Coleco` | `.col` | Needs BIOS: report only. |
| `intv` | `Intellivision` | `Intellivision` | `.int` | Needs BIOS: report only. |
| `ws` | `WonderSwan` | `WonderSwan` | `.ws` | |
| `wsc` | `WonderSwan Color` | `WonderSwan` | `.wsc` | |
| `ngp` | `Neo Geo Pocket` | `NGP` | `.ngp` | **verify** dir name; igir has no entry. |
| `vectrex` | `Vectrex` | `Vectrex` | `.vec` | |
| `pokemini` | `Pokemon Mini` | `PokemonMini` | `.min` | |
| `sv` | `Supervision` | `SuperVision` | `.sv` | |

## Disc

Disc entries in Redump DATs are one `<game>` with several `<rom>` rows: a
`.cue` plus `.bin` tracks, or a single `.iso`. The adapter places the whole
set in its own directory `games/<Core>/<Title>/` and multi-disc games share
that directory, which is what the cores want for disc swapping. Disc images are
never zipped. CHD is accepted on scan but not produced.

| id | DAT name matches | core dir | adapter notes |
|---|---|---|---|
| `psx` | `Sony - PlayStation` | `PSX` | Needs BIOS: report only. |
| `saturn` | `Sega - Saturn` | `Saturn` | Needs BIOS: report only. |
| `megacd` | `Sega - Mega CD - Sega CD` | `MegaCD` | Needs BIOS: report only. |
| `pcecd` | `PC Engine CD - TurboGrafx-CD` | `TGFX16-CD` | Needs system card: report only. |
| `neocd` | `Neo Geo CD` | `NeoGeo-CD` | **verify** dir. |

## Special adapters

| id | core dir | adapter notes |
|---|---|---|
| `neogeo` | `NeoGeo` | Cartridge games are romsets: a directory or zip per game whose internal layout the core expects, described by a `romsets.xml` the core ships. The adapter treats the DAT `<game>` as the unit, places the zip whole, and verifies member hashes against the DAT rather than the zip's own hash. Needs BIOS: report only. |
| `arcade` | `mame` (under `games/`) with MRAs in `_Arcade` | Wanted list is derived from MRA files: each MRA names the zips it needs. The adapter parses every MRA, lists missing zips, and places zips whole. Verification uses the MRA's `md5` where present and the loaded MAME DAT otherwise. No romset rebuilding, merging or splitting. |

## Computers

Computer cores use disk and tape images with TOSEC-style DATs and inconsistent
layouts. Out of scope for the first release. The table can grow once the
cartridge and disc adapters are proven.

## Adapter contract

Every adapter implements `CoreAdapter` from ARCHITECTURE.md and must provide:

- `games_dir`: the directory above, plus any legacy aliases accepted on scan.
- `accepts`: whether a path on disk is loadable as-is (extension, zipped or
  not, header present).
- `plan_placement`: the final relative path and the list of transformations.
  Transformations are limited to: unzip, zip, add header, strip header, swap
  byte order, create directory, rename. Nothing else.
- `requires_bios`: the BIOS filename the core documents, for the status
  screen. The adapter never handles the file.

## Header rules

Hashing rules keyed by platform, applied in `hash_reader`:

| rule | behaviour |
|---|---|
| `none` | hash whole file |
| `ines` | if the file starts with `NES\x1a`, hash from byte 16 when matching a headerless DAT |
| `smc` | if size mod 1024 is 512, skip the first 512 bytes |
| `a78` | skip 128-byte header when matching a headerless DAT |
| `lnx` | skip 64-byte header when present |
| `n64` | detect byte order from the first four bytes and normalise to big-endian while hashing |

Each rule is a pure function with unit tests in `mistarr-core`.
