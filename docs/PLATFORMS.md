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
| `nes` | `Nintendo Entertainment System`, `NES` | `NES` | `.nes` | Use the **headered** No-Intro DAT. The core needs an iNES header. A headered file is hashed whole and with its 16-byte header stripped, in one pass, so it matches the headered and the headerless DAT alike ("Header rules"); the header is never stripped on disk. From a DB export the headerless entries are used, and placement adds the header back from the recorded `header` attribute (VERIFICATION.md "DB export"). |
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
| `atari7800` | `Atari 7800` | `Atari7800` | `.a78` | Headered DAT. A headerless DAT matches too: files are hashed with and without the 128-byte header ("Header rules"). |
| `lynx` | `Atari Lynx` | `AtariLynx` | `.lnx` | Headered DAT. A headerless DAT matches too: files are hashed with and without the 64-byte header ("Header rules"). |
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
never zipped. CHD images are accepted on scan, never produced.

A CHD is identified by its tracks: each track's `.bin` is rebuilt from the
image and hashed, and the image is `verified` when every track of one DAT
entry matches (VERIFICATION.md "CHD images"). That means decoding the whole
image, which takes minutes per disc on the DE10-Nano, so it runs only with
`[scan] chd_tracks` on, off by default and switched in System; each image is
decoded once and its track hashes are kept. With it off, or while an image
waits, the scan reads only its header and the Platforms card lists it as not
identified with the reason, not as unmatched. GD-ROM images, images that need
a parent CHD, uncompressed images, which chdman writes without checksums,
and tracks stored without their full 2352-byte sectors are not identified. A DAT that lists `.chd` files as roms still verifies them by
whole-file hash.

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

`romsets.xml` is streamed through a `BufReader`, refused unread above 16 MiB,
and, through core's `CappedReader` like the MRA reader below, capped at one
1 MiB XML event and 64 levels of element nesting; either cap failing refuses
the file with an error. It
also caps what it accumulates: at most `MAX_SETS` (4096) distinct `<romset
name>` values and `MAX_BIOS_NAMES` (4096) distinct BIOS file names from
comments, each kept to `MAX_NAME_BYTES` (256) bytes since a real romset or
BIOS name is a short file or directory name, deduplicated through a
`HashSet` and refused past its cap. For a capped 16 MiB input its worst-case
peak is dominated by `quick_xml`'s own record of currently-open names
(bounded the same way as the MRA reader's below, roughly 16 to 32 MiB) plus
its 1 MiB event buffer (up to about 2 MiB): roughly 17 to 34 MiB; the `sets`
and `bios` lists, each name kept twice over at its cap (once in a `HashSet`,
once in a `Vec`), add under 5 MiB and do not change that order.

### MRA catalogue

The catalogue reads the real MRA files once each. Under `_Arcade` it skips
symlinked folders, any folder named `_Organized` in any letter case, which
the Arcade Organizer fills with thousands of symlinks to the same MRAs, and
a second path to a file already listed (a hard link or a symlink to it, same
device and inode). A symlink to an MRA file elsewhere is followed.
`_alternatives` holds distinct MRAs and is read. An MRA is refused unread
above 16 MiB, a sanity bound: inline part data makes some a few MiB. The
catalogue streams each file, keeping its metadata, zip references and rom
structure; the hex of an inline part is checked as it passes and left in the
file, recorded by its place, so no payload is held across a batch.

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
bytes, and a `<rom>` inside one left open never adds to it. Text that is not
UTF-8 follows VERIFICATION.md "Text encoding": refused in text and
attribute values, passed over in comments.

As VERIFICATION.md "DAT parsing" caps a DAT, and through the same
`mistarr_core::xml::CappedReader`, the reader caps one XML event, a
tag, a text run or a comment, at 1 MiB before it is buffered
(`Error::XmlEventTooLarge`), and element nesting at 64 levels
(`Error::XmlTooDeep`); the open-element stack used to match end tags never
holds more entries than the depth cap allows. Depth is tracked separately
from that stack's own length, as a plain count of `quick_xml`'s start and end
events: MiSTer's tolerant end-tag recovery can close several stack entries at
once, so the plain count is what stays true to the document's real nesting
and is what the 64-level cap checks. Each name kept on the open-element stack
is itself cut to 64 bytes, since matching an end tag never needs more and no
real MRA tag name comes close, so the stack's own memory stays a few KiB
regardless of how long an attacker's tag names run. A `<part>`'s own inline
hex, which can run to several MiB, bypasses the event cap: it is read
straight off the file a buffer at a time into `Part::data` or, from `read`,
left in place as the `Inline` marker below, so neither cap stops a large but
legitimate rom.

The parser also caps what it accumulates: at most `MAX_ROMS` (2048) `<rom>`
elements; at most `MAX_ROM_ITEMS` (1024) parts, patches, interleaved parts
and unsupported entries per `<rom>` (an interleave's own parts count against
the same budget as the rest of its rom's items) and, on top of that, at most
`MAX_TOTAL_ROM_ITEMS` (131,072) of them summed across every `<rom>` in the
document, so total item memory is the same whether they sit in one rom or
many; at most `MAX_ZIPS` (4096) distinct zip names collected from every
`zip` attribute in the document; and, separately, at most
`MAX_ZIPS_PER_LIST` (16) zip names kept from any one `<rom>`'s or `<part>`'s
own `zip` attribute, with duplicates within that one attribute dropped
first, and at most `MAX_TOTAL_ZIP_REFS` (65,536) of those summed across the
document. Every zip name and a `<part>`'s own `name` attribute are kept to
`MAX_NAME_BYTES` (255) bytes, since both are real file names; a longer one
is refused rather than stored, as `romsets.xml`'s own name cap above. Each
cap is refused with `Error::XmlOutputTooLarge`, and zip names dedupe through
a `HashSet` rather than a linear scan. A corpus of real MRAs
has been seen with up to about 160 items in one `<rom>`, well under
`MAX_ROM_ITEMS`. `Mra::md5` is collected from the roms that closed into
`Mra::roms`, never from a `<rom>`-named tag met in passing (nested inside
another rom's unsupported content, for instance), so it cannot grow past
`MAX_ROMS` regardless of what such a tag contains. An unsupported-content
reason quotes at most 32 bytes of the attribute value that triggered it
before it is Debug-escaped, so one malformed value cannot make its reason
far larger than the input needed to write it.

For a capped 16 MiB input read through `read`, the worst case reaches every
cap: up to 2048 `MraRom` entries and up to `MAX_TOTAL_ROM_ITEMS` (131,072)
rom items summed across them, no single `<rom>` holding more than
`MAX_ROM_ITEMS` (1024) of them. Each item stays under roughly 400 bytes on a
32-bit target: a named `<part>` holding a full `MAX_NAME_BYTES` (255) byte
name, or a worst-case unsupported-content reason (32 bytes quoted,
Debug-escaped about 6x, plus its fixed wording), for about 50 MiB of live
data and up to about 100 MiB counting each `Vec`'s own spare capacity once
it has doubled to fit. Zip names are capped the same way but stored three
times over: in `mra.zips` and the `zips_seen` `HashSet` (up to `MAX_ZIPS`
each) and in the per-`<rom>` or per-`<part>` `zips` lists (up to
`MAX_TOTAL_ZIP_REFS` summed); at up to 4096 + 4096 + 65,536 names of
`MAX_NAME_BYTES` (255) bytes each, that adds roughly 18 MiB. `quick_xml`'s
own record of currently-open names adds roughly 16 to 32 MiB, as for
`romsets.xml` above: it keeps the full name of every still-open element up
to the depth cap, and a document of nested opening tags with no closes can
spend most of its 16 MiB on those names before the cap refuses it; MRA's own
open-element stack (a few KiB, per above) truncates each name to 64 bytes
but is a separate copy, so it does not shrink `quick_xml`'s. The 1 MiB event
buffer adds at most a few MiB more. That puts the peak at roughly 86 to 153
MiB: higher than `romsets.xml` above, since an MRA has more kinds of
accumulated output to bound, but still well inside the board's shared
budget.

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

Arcade on MiSTer runs from MRAs, so arcade needs MRAs under `_Arcade`: a
MAME or HBMAME DAT with no MRAs is not a supported setup. A library scan
never walks the arcade platform's directories; hashing every member of every
MAME zip on every scan is needless CPU and SD reads. Arcade state comes from
three places, never a scan:

1. An MRA title's zip presence and md5 check, "MRA assembly" below, run by
   the arcade catalogue job.
2. The presence pass the same job runs after storing titles (ARCHITECTURE.md
   "Arcade catalogue", step 4): it tracks which zips named by live MRAs exist
   under `games/mame` and `games/hbmame` from a stat of each, giving such a
   zip an `unverified` row against the MRA's zip rom that an md5 check reading
   it as a sibling promotes, and it prunes `files` rows whose zip is gone. It
   verifies nothing by itself.
3. The import path, "MRA import" below, for a zip mistarr places. A loaded
   MAME or HBMAME DAT only adds verification here: when an imported zip's MRA
   carries no md5, its members are checked against the DAT entry of the same
   set name with full hashes. A zip the user copies in is never checked
   against a DAT.

While any live MRA title exists, the arcade browse lists MRA titles only,
and wanting an MRA title creates downloads only for its missing zips.

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
| `<part>hex</part>` | inline bytes, digit pairs separated by spaces, commas or newlines; the md5 check, one MRA at a time, decodes them again from the file straight into the digest, for each repeat, holding no payload |
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
rom for `dat` and to the MRA title's zip rom otherwise. Then the presence
and md5 check of every MRA title naming the zip are redone, so a title
shows have once all its zips are present and the check has not failed.
Wanting a title creates one download per zip not on disk, and the zips may
land in any order.

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
  be `verified`. A CHD identified by its tracks hands over the `.chd`, and
  its `#cue` row never counts as a cue sheet;
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

Every adapter implements `CoreAdapter` from ARCHITECTURE.md, whose one
method is `plan_placement`: the final relative path and the list of
transformations. Transformations are limited to: unzip, zip, add header,
strip header, swap byte order, create directory, rename. Nothing else.
Staging paths in a step are relative to the directory holding the staged
item; library paths and the final path are relative to `games/`. Cartridge
files are unzipped and named `<DAT entry name>.<ext written>`. Disc tracks
take the DAT rom names, which are the names a DAT-verified cue already
references, so a cue is never rewritten.

`adapter_for` picks the adapter by the row's kind and, for a cartridge, its
header rule: `ines` adds the DAT's iNES header to a file that lacks one,
`smc` strips a copier header, `n64` swaps a file to big-endian, and any other
cartridge row is only unzipped and renamed.

Every other platform fact is a field or method of the `Platform` row, not of
the adapter: `platform_id`, `games_dir`, `legacy_dirs`, `load_extensions`,
and `bios`, the BIOS filename the core documents for the status screen,
which nothing handles. The library scan keeps its own extension rules: a
file is found by the row's `load_extensions`, and on a cartridge row also as
a `.zip`. It checks no content, so a headerless NES file is still found,
hashed and matched; only placement needs the iNES header the core loads.

## Header rules

Hashing rules keyed by platform, applied in `hash_reader` and `hash_forms`:

| rule | behaviour |
|---|---|
| `none` | hash whole file |
| `ines` | if the file starts with `NES\x1a`, hash the whole file and, in the same pass, the content from byte 16 |
| `smc` | if size mod 1024 is 512, skip the first 512 bytes |
| `a78` | if bytes 1 to 9 are `ATARI7800`, hash the whole file and the content after the 128-byte header |
| `lnx` | if the file starts with `LYNX`, hash the whole file and the content after the 64-byte header |
| `n64` | detect byte order from the first four bytes and normalise to big-endian while hashing |

`ines`, `a78` and `lnx` strip a header: a file with the header's magic has two
forms, and it matches a headered DAT by its whole hashes and size and a
headerless DAT by its content's hashes and the size less the header, the whole
file tried first. Both sets are stored (DATA-MODEL.md `files`), so a DAT loaded
later matches either way without reading the file again. A file without the
magic has one form, the whole file, and matches only by it. `smc` has no
headered form in any DAT, and `n64` DATs list the big-endian form only, so both
keep one set of hashes. Headers are never stripped on disk.

Each rule is a pure function with unit tests in `mistarr-core`.
