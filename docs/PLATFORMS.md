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
| `arcade` | `MAME`, `Arcade` | `mame` and `hbmame` (under `games/`) with MRAs in `_Arcade` | Wanted list is derived from MRA files: each MRA names the zips it needs. The adapter parses every MRA, lists missing zips, and places zips whole; see "MRA import". Verification uses the MRA's `md5` where present and the loaded MAME DAT otherwise. No romset rebuilding, merging or splitting. A staged directory is zipped under the set name for a DAT entry. |

### Neo Geo `romsets.xml`

When `games/NeoGeo/romsets.xml` exists, title detail of a Neo Geo entry says
whether the file lists the entry's name as a `<romset name>` and whether
`games/NeoGeo/<name>/` or `<name>.zip` exists. The file names the BIOS files
the core expects inside an XML comment, one per line; every comment line that
is a single `name.ext` token is taken as one, and detail reports each as
present or missing in `games/NeoGeo`. Nothing else is done with them.

### MRA catalogue

The catalogue reads the real MRA files once each. Under `_Arcade` it skips
symlinked folders, any folder named `_Organized` in any letter case, which
the Arcade Organizer fills with thousands of symlinks to the same MRAs, and
a second path to a file already listed (a hard link or a symlink to it, same
device and inode). A symlink to an MRA file elsewhere is followed.
`_alternatives` holds distinct MRAs and is read. An MRA is refused unread
above 1 MiB.

An MRA is read again only when its size or modification time changes, or
when the parser version changes. exFAT and FAT keep modification times to
2 seconds, so a rewrite that keeps the size within the same 2 seconds goes
unnoticed until the next change; a scan from `POST /system/scan` after such
an edit rereads nothing either, and touching the file later picks it up.

MRA markup is read the way MiSTer's loader reads it: element and attribute
names in any letter case, so `<ROM>` closed by `</rom>` is one element; an
end tag closes the innermost open element of its name, a stray end tag is
ignored and an unknown entity is kept as written. A file that ends inside
an element is refused. `<name>`, `<setname>` and `<rbf>` keep at most 256
bytes, and a `<rom>` inside one left open never adds to it.

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

A library scan never walks the arcade platform's directories: hashing every
member of every MAME zip on every scan is needless CPU and SD reads, since
presence and verification for arcade come only from the arcade catalogue's
md5 check and, for a zip mistarr placed, the import path. A DAT entry loaded
for `arcade` (`source = 'dat'`) is therefore never marked `have` by a scan;
only a zip mistarr imports for it, through an MRA's "MRA import" `dat`
fallback, is ever verified. While any live MRA title exists, the arcade
browse lists MRA titles only, and wanting an MRA title creates downloads
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
| `offset`, `length`, `repeat` | numbers as C `strtoul` reads them (`0x` hex, leading-zero octal, decimal); `length="0"` takes the rest; `repeat="0"` emits nothing and reads nothing; the md5 check streams a named part again from its zip for each repeat, holding no part in memory; an offset past the end, `repeat` above 4096, an empty part repeated, and any offset or length that overflows are refused |
| `<part>hex</part>` | inline bytes, digit pairs separated by spaces, commas or newlines |
| `<interleave input="8" output="8..64">` | parts spread by `map`, hex digits read from the right, one per output byte: the k-th non-zero digit `d` writes input byte k of each word at output byte `first + d - 1 + gaps`, `first` being the first non-zero digit's position and `gaps` the zero digits after it so far; a missing `map` is `1`; a part that is not a whole number of words is refused |
| `<patch offset operation="xor">hex</patch>` | overwrite or exclusive-or into the assembled bytes; past the end is refused |
| `map` outside `<interleave>`, other `input` widths, `<group>` and any other element inside `<rom>` | refused |

A part in none of its zips is reported as `missing_part` with its name; it
is never sourced. Assembled roms are capped at 512 MiB.

### MRA import

A download of an MRA title is one zip the MRA names, staged whole. The
importer reads the MRA again and checks the zip in this order, recording
which check applied as `verification` in `import_log`:

1. Every named part whose zips include this one must be in one of them;
   parts that may still come from a zip not yet on disk are not counted.
   Otherwise the zip is quarantined, the report and the download's error
   naming the missing members.
2. `mra_md5`, when every `<rom>` index the zip feeds has a `<rom>` with an
   `md5`: those roms are assembled as in "MRA assembly" from the staged zip
   and the sibling zips already on disk. When every index matches, the
   members read by the `<rom>` alternatives that matched are `verified` and
   the rest `unverified`; a mismatch quarantines the zip;
   content the assembler refuses fails the download with the assembler's
   reason and leaves the zip in staging. While a part may come from a zip
   not yet on disk, the zip is placed with `unverified` members and the
   reason names the zips the check waits for; when the last of them lands
   the check runs over all of them and marks the members read from the
   earlier zips `verified`.
3. `dat`, when a loaded DAT of the platform has a live entry named as the
   zip without `.zip`, looked up by directory: a zip in `games/hbmame/`
   only in DATs whose header name contains `HBMAME`, any other zip only in
   the other DATs. Members are verified against it as a romset, and a zip
   that does not match exactly is quarantined. An entry flagged `bios`
   refuses the download on this path only; a zip the md5 covers is never
   looked up in a DAT.
4. `none`: the zip is placed with `unverified` members and the reason `no
   hash source`.

The zip goes whole, never unpacked, to `games/mame/` or `games/hbmame/` as
its MRA path says, under the MRA's file name; a zip read from any other
directory is refused. `files` gets one row per member, linked to the DAT
rom for `dat` and to the MRA title's zip rom otherwise, with the size and
mtime a scan would record, so a following scan keeps them. Then the
presence and md5 check of every MRA title naming the zip are redone, so a
title shows have once all its zips are present and the check has not
failed. Wanting a title creates one download per zip not on disk, and the
zips may land in any order.

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

## Launch parameters

Starting a DAT entry (ARCHITECTURE.md "Launching") writes an MGL file:

```xml
<mistergamedescription>
  <rbf>_Console/NES</rbf>
  <file delay="2" type="f" index="1" path="../../../../../media/fat/games/NES/Example Quest (USA).nes"/>
</mistergamedescription>
```

Each row names its launch cores in order of preference, each with its own
parameters, in the `launch` column of the table in `mistarr-mister`. The
first core with an installed `.rbf` is used, and of its files the newest by
the `_YYYYMMDD` date in the name; undated files rank last. Files are looked
for in `_Console`, `_Computer`, `_Other` and `_Arcade` and one folder below,
but a file under `_Arcade` only for a core the row marks as living there;
installed-core detection then marks that row present as well as `arcade`.
`rbf` is the chosen file's path relative to the SD root without its date
suffix and extension. `path` climbs from the core's games folder to `/` and
names the file absolutely. Attribute values are XML-escaped and paths with
control characters are refused.

| column | meaning |
|---|---|
| type | `f` loads the file into the core, `s` mounts it as a disc or disk image |
| index | the core's file-slot index |
| delay | seconds Main waits after loading the core before handing it the file |

The file handed over is:

- for a disc, the first cue sheet among the entry's files whose `FILE`
  entries all exist beside it, else its `.chd` or `.iso`; every track must
  be `verified`;
- for a romset, its zip, or its set directory `games/NeoGeo/<set>`;
- otherwise its file. Only the first `.zip#`, compared case-insensitively,
  separates a zip from its member, which Main opens as `a.zip/b.nes`; any
  other `#` is part of a name.

Arcade titles have no row: an MRA title starts with `load_core` on its
`.mra`. The community convention for the Neo Geo core loads `.neo` files
through an MGL; mistarr places romsets as zips or directories, so the
`neogeo` row hands over the romset itself and is the least certain of all.

Every row is **verify**: the values follow the community launcher tables
and are confirmed on the board. `board_verify_launch_cores` in
`platforms.rs` checks that each row finds a core on the card; the slot
values are confirmed by starting a game of each platform from the UI.

| id | core | type | index | delay |
|---|---|---|---|---|
| `nes` | NES | `f` | 1 | 2 |
| `fds` | NES | `f` | 1 | 2 |
| `snes` | SNES | `f` | 0 | 2 |
| `n64` | N64 | `f` | 1 | 1 |
| `gb` | Gameboy | `f` | 1 | 2 |
| `gbc` | Gameboy | `f` | 1 | 2 |
| `gba` | GBA | `f` | 1 | 2 |
| `megadrive` | MegaDrive, then Genesis | `f` | 1 | 1 |
| `s32x` | S32X | `f` | 1 | 1 |
| `sms` | SMS | `f` | 1 | 1 |
| `gg` | SMS | `f` | 2 | 1 |
| `sg1000` | ColecoVision | `f` | 0 | 1 |
| `pce` | TurboGrafx16 | `f` | 0 | 1 |
| `sgx` | TurboGrafx16 | `f` | 1 | 1 |
| `atari2600` | Atari2600, then Atari7800 | `f` | 1 | 1 |
| `atari5200` | Atari5200 | `s` | 1 | 1 |
| `atari7800` | Atari7800 | `f` | 1 | 1 |
| `lynx` | AtariLynx | `f` | 1 | 1 |
| `coleco` | ColecoVision | `f` | 1 | 1 |
| `intv` | Intellivision | `f` | 1 | 1 |
| `ws` | WonderSwan | `f` | 1 | 1 |
| `wsc` | WonderSwan | `f` | 1 | 1 |
| `ngp` | jtngp, under `_Arcade` too | `f` | 1 | 2 |
| `vectrex` | Vectrex | `f` | 1 | 1 |
| `pokemini` | PokemonMini | `f` | 1 | 1 |
| `sv` | SuperVision | `f` | 1 | 1 |
| `psx` | PSX | `s` | 1 | 1 |
| `saturn` | Saturn | `s` | 0 | 2 |
| `megacd` | MegaCD | `s` | 0 | 1 |
| `pcecd` | TurboGrafx16 | `s` | 0 | 1 |
| `neocd` | NeoGeo | `s` | 1 | 1 |
| `neogeo` | NeoGeo | `f` | 1 | 1 |

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
