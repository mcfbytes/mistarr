# Platforms and core adapters

The table maps DAT header names to the directory each MiSTer core reads from
and to the adapter that handles the core's quirks. Directory names are the
ones MiSTer's core documentation lists; exFAT is case-insensitive so case is
cosmetic, but write them as shown. Entries marked **verify** were taken from
igir's mapping and must be confirmed against a live board before the adapter is
considered done (see WORKPLAN WP-04).

Platform ids are the stable slugs used in the database and API.

A DAT header name binds to a row by its "DAT name matches" patterns. The DAT
name is lowercased, bracketed groups such as `(Headered)` are dropped and every
run of other non-alphanumeric characters becomes one space, so
`Sega - Mega CD & Sega CD` and `Sega - Mega CD - Sega CD` compare equal. A
pattern must match on word boundaries, and the longest match across all rows
wins: `Game Boy Color` binds to `gbc`, not `gb`, and `PC Engine CD` binds to
`pcecd`, not `pce`. BIOS file names are the ones each core's documentation
gives; confirm them on the board together with the **verify** rows.

## Cartridge and handheld

| id | DAT name matches | core dir | ext written | adapter notes |
|---|---|---|---|---|
| `nes` | `Nintendo Entertainment System`, `NES` | `NES` | `.nes` | Use the **headered** No-Intro DAT. The core needs an iNES header. If only the headerless DAT is loaded, hash with the 16-byte header stripped for matching, but never strip on disk. |
| `fds` | `Famicom Disk System`, `Family Computer Disk System` | `NES` | `.fds` | Same directory as NES. Needs BIOS `boot0.rom`: report only. |
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
| `coleco` | `ColecoVision` | `Coleco` | `.col` | Needs BIOS `boot0.rom`: report only. |
| `intv` | `Intellivision` | `Intellivision` | `.int` | Needs BIOS `boot0.rom`: report only. |
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
| `psx` | `PlayStation` at the end of the name, so later consoles do not bind | `PSX` | Needs BIOS `boot.rom`: report only. |
| `saturn` | `Sega - Saturn` | `Saturn` | Needs BIOS `boot.rom`: report only. |
| `megacd` | `Mega CD - Sega CD` | `MegaCD` | Needs BIOS `cd_bios.rom`: report only. |
| `pcecd` | `PC Engine CD - TurboGrafx-CD` | `TGFX16-CD` | Needs system card `cd_bios.rom`: report only. |
| `neocd` | `Neo Geo CD` | `NeoGeo-CD` | **verify** dir. Needs BIOS `top-sp1.bin`: report only. |

## Special adapters

| id | DAT name matches | core dir | adapter notes |
|---|---|---|---|
| `neogeo` | `Neo Geo` | `NeoGeo` | Cartridge games are romsets: a directory or zip per game whose internal layout the core expects, described by a `romsets.xml` the core ships. The adapter treats the DAT `<game>` as the unit, places the zip whole, and verifies member hashes against the DAT rather than the zip's own hash. A staged directory is placed whole as a directory. Needs BIOS `000-lo.lo`, `sfix.sfix`, `sp-s2.sp1`: report only. |
| `arcade` | `MAME`, `Arcade` | `mame` (under `games/`) with MRAs in `_Arcade` | Wanted list is derived from MRA files: each MRA names the zips it needs. The adapter parses every MRA, lists missing zips, and places zips whole. Verification uses the MRA's `md5` where present and the loaded MAME DAT otherwise. No romset rebuilding, merging or splitting. A staged directory is zipped under the set name. |

### Neo Geo `romsets.xml`

When `games/NeoGeo/romsets.xml` exists, title detail of a Neo Geo entry says
whether the file lists the entry's name as a `<romset name>` and whether
`games/NeoGeo/<name>/` or `<name>.zip` exists. The file names the BIOS files
the core expects inside an XML comment, one per line; every comment line that
is a single `name.ext` token is taken as one, and detail reports each as
present or missing in `games/NeoGeo`. Nothing else is done with them.

### MRA catalogue

The arcade catalogue job (ARCHITECTURE.md "Arcade catalogue") turns each MRA
into one `arcade` title with `source = 'mra'`: `<name>`, `<setname>` and
`<rbf>` are kept, the name is parsed for regions and flags like a DAT name,
and titles group by base name among MRA titles only, so an `_alternatives`
MRA is a variant of its main one. Each zip named by any `zip` attribute
(`|`-separated lists split) is a rom named by the zip's file name, carrying
the `md5` of the first `<rom>` that names it, or no hash. MiSTer reads a
plain zip name from `games/mame/`, a name starting with `/` from `games/`
(so `/hbmame/x.zip` is `games/hbmame/x.zip`), and resolves `..`; a name that
leaves `games/` is ignored. A title counts as have when every zip it names is
present and its md5 check is not `mismatch` or `missing_part`.

While any live MRA title exists, the arcade browse lists MRA titles only;
DAT entries on the platform still verify zip members during a scan. MRA roms
are never matched by the scanner, and wanting an MRA title creates downloads
only for its missing zips.

### MRA assembly

The md5 check follows MiSTer's loader: the digest of the bytes of every part
of a `<rom>`, in document order, before interleaving and without patches. A
`<rom>` without `zip` or with `md5="none"` is not checked. When several
`<rom>` elements share an `index`, one match is enough. The assembler builds
the rom from this subset and refuses anything else by name:

| Content | Handling |
|---|---|
| `<part name zip crc>` | the member from the part's `zip`, else the rom's, trying each of a `\|` list in order; found by exact name, then case-insensitive name, then `crc` |
| `offset`, `length`, `repeat` | numbers as C `strtoul` reads them (`0x` hex, leading-zero octal, decimal); `length="0"` takes the rest; an offset past the end is refused |
| `<part>hex</part>` | inline bytes, digit pairs separated by spaces, commas or newlines |
| `<interleave input="8" output="8..64">` | parts spread by `map`, hex digits read from the right, one per output byte: the k-th non-zero digit `d` writes input byte k of each word at output byte `first + d - 1 + gaps`, `first` being the first non-zero digit's position and `gaps` the zero digits after it so far; a missing `map` is `1`; a part that is not a whole number of words is refused |
| `<patch offset operation="xor">hex</patch>` | overwrite or exclusive-or into the assembled bytes; past the end is refused |
| `map` outside `<interleave>`, other `input` widths, `<group>` and any other element inside `<rom>` | refused |

A part in none of its zips is reported as `missing_part` with its name; it
is never sourced. Assembled roms are capped at 512 MiB.

## Thumbnail playlists

Art URLs (API.md "Art URLs") use the libretro playlist name of the platform,
the `libretro_playlist` column of the table in `mistarr-mister`.

| id | playlist | id | playlist |
|---|---|---|---|
| `nes` | Nintendo - Nintendo Entertainment System | `atari7800` | Atari - 7800 |
| `fds` | Nintendo - Family Computer Disk System | `lynx` | Atari - Lynx |
| `snes` | Nintendo - Super Nintendo Entertainment System | `coleco` | Coleco - ColecoVision |
| `n64` | Nintendo - Nintendo 64 | `intv` | Mattel - Intellivision |
| `gb` | Nintendo - Game Boy | `ws` | Bandai - WonderSwan |
| `gbc` | Nintendo - Game Boy Color | `wsc` | Bandai - WonderSwan Color |
| `gba` | Nintendo - Game Boy Advance | `ngp` | SNK - Neo Geo Pocket |
| `megadrive` | Sega - Mega Drive - Genesis | `vectrex` | GCE - Vectrex |
| `s32x` | Sega - 32X | `pokemini` | Nintendo - Pokemon Mini |
| `sms` | Sega - Master System - Mark III | `sv` | Watara - Supervision |
| `gg` | Sega - Game Gear | `psx` | Sony - PlayStation |
| `sg1000` | Sega - SG-1000 | `saturn` | Sega - Saturn |
| `pce` | NEC - PC Engine - TurboGrafx 16 | `megacd` | Sega - Mega-CD - Sega CD |
| `sgx` | NEC - PC Engine SuperGrafx | `pcecd` | NEC - PC Engine CD - TurboGrafx-CD |
| `atari2600` | Atari - 2600 | `neocd` | SNK - Neo Geo CD |
| `atari5200` | Atari - 5200 | `neogeo` | SNK - Neo Geo |
| | | `arcade` | MAME |

## Computers

Computer cores use disk and tape images with TOSEC-style DATs and inconsistent
layouts. Out of scope for the first release. The table can grow once the
cartridge and disc adapters are proven.

## Adapter contract

Every adapter implements `CoreAdapter` from ARCHITECTURE.md and must provide:

- `games_dir`: the directory above, plus any legacy aliases accepted on scan.
- `accepts`: whether a path on disk is loadable as-is (extension, zipped or
  not, header present). Cartridge rows without a content rule also accept a
  `.zip`; NES, SNES and N64 accept only files whose header or byte order they
  can check.
- `plan_placement`: the final relative path and the list of transformations.
  Transformations are limited to: unzip, zip, add header, strip header, swap
  byte order, create directory, rename. Nothing else. Staging paths in a step
  are relative to the directory holding the staged item; library paths and
  the final path are relative to `games/`. Cartridge files are unzipped and
  named `<DAT entry name>.<ext written>`. Disc tracks take the DAT rom names,
  which are the names a DAT-verified cue already references, so a cue is
  never rewritten.
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
