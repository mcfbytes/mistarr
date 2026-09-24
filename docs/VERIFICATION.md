# Verification, matching and 1G1R

## DAT parsing

Two input forms are accepted, told apart by the first element: Logiqx XML,
whose root is `<datafile>`, and a No-Intro database export, which starts
with `<header>` (see "DB export" below). Anything else is rejected with a
reason naming both. Parse with `quick-xml` in streaming mode so a 50 MB DAT
does not need to be held in memory twice.

Logiqx XML is a `<datafile>` with a `<header>` and repeated `<game>`
elements, each with one or more `<rom>` children. Extract per game: `name`,
`cloneof`, `romof`, `description`, `category`, the `region` and `language`
of each `<release>`, and per rom: `name`, `size`, `crc`, `md5`, `sha1`,
`status` (default `good`), `header`. Ignore everything else. Reject files
that have no games. Hash attributes are stored lowercase and
must have their full hex length; a malformed hash, size or status rejects the
file. `dat::DatStream` yields one game at a time for importers that write as
they read.

Regions and languages come from the name (see "Name parsing"); a DAT's own
fields (`<release>` or the export's `archive`) fill them only when the name
has none, which is common for languages.

### Text encoding

DATs, MRAs and `romsets.xml` are read as UTF-8. A leading UTF-8 byte order
mark is skipped and the `encoding` of an XML declaration is ignored; nothing
is transcoded, and UTF-16 is not supported. `mistarr_core::xml::EscapeInvalid`
sits between the file and the XML reader and escapes each byte that is not
part of a valid UTF-8 sequence, so a non-UTF-8 byte fails the file only where
the parser uses its value: a name, element text or an attribute it reads.
A DAT error names the byte offset in the file, counting a byte order mark;
for bad element text it is the offset of the closing tag. The same bytes in
a comment, a processing instruction, an element or attribute the parser
ignores, or a tag name pass without effect, so a Latin-1 comment or unknown
tag never fails a DAT. The escapes are the code points U+10FF80 to U+10FFFF,
so a value that holds one of them, written literally or as a character
reference such as `&#x10FFE9;`, is refused too.

### DB export

A No-Intro database export is two top-level elements, `<header>` and then
`<datafile>`, read as one stream. The header has a `<version>` but no system
name. Each `<game name>` holds one `<archive>` and one or more `<source>`
blocks, each with a `<details>` element and `<file>` elements carrying
`extension`, `size`, `crc32`, `md5`, `sha1`, `format` (`Headered` or
`Headerless`) and, on a headered file, `header`; a file may also carry
`item`, `forcename`, `bad` and `mia`. It maps onto the same game and rom
model as a Logiqx DAT:

| Model | Taken from |
|---|---|
| game name | `game@name` |
| parent | `archive@clone`: `P` or empty on a parent; otherwise the parent's `archive@number`, resolved to its game name by a first pass over the document, since a clone may precede its parent |
| regions, languages | `archive@region` and `archive@languages`, comma-separated |
| flags | `archive@status` (such as `Beta 2`, `Proto 3`, `Possible Proto`, `Demo`, `Sample`) adds `beta`, `proto`, `demo` or `sample` when the name does not already carry that flag, so the hide list applies |
| roms | the game image files of every source, one per `sha1` (size and other hashes without one), of one storage kind chosen per game |
| rom name | the file's `forcename` when present, else `<game name>.<ext>` |
| rom status | `baddump` when the file has `bad="1"`, else `good`; `mia="1"` leaves the status alone, since the file carries known hashes that verify |
| DAT name, version | from the file name, `<System> (DB Export) (<version>).xml` or `.zip`, as `<System> (DB Export)`; the header `<version>` when present |

Only the game image becomes a rom. Extras such as save data or a separate
chip dump are skipped, since a game needs every rom to verify and one of
them as a rom would keep the game from ever verifying. A file with an `item`
attribute is always an extra; it is read leniently, so a malformed size or
hash on it skips the file instead of rejecting the DAT. A file without
`item` is an image when it has no extension, the headerless extension `unh`
(or `lyx` for Lynx), or one of the platform's accepted extensions: every
extension it loads (PLATFORMS.md) plus the one it writes. A headerless file
whose size plus the `header` bytes of a headered image in its source equals
that image's size is its headerless form and also counts, whatever its
extension. For a DAT bound to no platform the headered file's extension is
accepted; with nothing accepted every file without `item` counts. When no
file is an image and a game has exactly one distinct file without `item`,
that file is the image, named with the platform's extension.

The storage kind follows how the board hashes the platform (PLATFORMS.md
"Header rules"). A file is headerless when `format="Headerless"` or its
extension is `unh`, headered when `format="Headered"`. Of the kinds a game
has, the first in this order is taken:

- platforms whose rule strips a header (`ines`, `a78`, `lnx`): headerless,
  then `Default` or no format, then big-endian, other formats, headered;
- `n64`: `BigEndian`, then as below;
- every other platform: `Default` or no format, then `BigEndian`, other
  formats, headered, headerless.

A headerless rom takes the `header` attribute of the headered file in its
source, which placement uses to add the header back. Unless it has a
`forcename`, a headerless, extensionless or lone-file image is named with
the platform's written extension, else the first it loads, else the
headered file's. Candidates are ordered good, then bad dumps, before
duplicates are dropped, so a good dump wins over a bad one of the same
name; of two files that would still get the same rom name the first stays. A DAT name bound
to no platform takes the last rule, and binding it later reads the file
again under the chosen platform's rule.

Both No-Intro and Redump distribute zipped daily packs. Accept `.zip` and
load every `.dat` or `.xml` member as a separate DAT.

Store the header `name` verbatim; platform binding works on it. Store
`version` verbatim; supersession compares it as a string, newest by
`loaded_at` when equal. A DB export named without the `(DB Export)` marker
takes its name from the file stem less a final version group, which becomes
its version.

### DAT families

Several DATs may be live for one platform, such as an official DAT and an
open-licensed add-on list. Only a refreshed or alternate form of the same
list supersedes. That list is the DAT's family, keyed by
`mistarr_core::dat::family_key` on the header name:

1. Drop every bracketed group whose text, lowercased with `-` and `_` read
   as spaces, is one of the format markers in
   `mistarr_core::dat::FORMAT_MARKERS` (`db export`, `headered`,
   `headerless`, `parent clone`, `retool`, `bigendian`, `byteswapped`,
   `littleendian`), optionally followed by a date or version.
2. Drop final `(…)` or `[…]` groups made of a date or version: digits with
   `.`, `-`, `_`, `:` or spaces, optionally after a `v`. Markers are dropped
   first, so a version group followed only by markers goes too.
3. Collapse whitespace and lowercase. A name that is nothing but such groups
   keeps its whole text, lowercased, so it never shares an empty key.

So `Example Vendor - Example System (Headered)` and `Example Vendor - Example
System (DB Export)` are one family, `example vendor - example system`, and
`Example Samples - Example System (Headered)` is another. `X (2)` and `X` are
deliberately one family: a bare number reads as a version. The key is stored
in `dat_versions.family` and refreshed from the names at every start.

Supersession runs within a family among versions bound to the same platform,
or among unbound versions; an unbound version never supersedes or blocks a
bound one, and binding a version applies the rule for its new platform.
Versions are compared by `mistarr_core::dat::version_order`, the numbers of
their digit runs, so `20260101-000000` and `20260102` compare as dates and
`1.9` sorts before `1.10` whatever form each came in. When either side has
no digits, as a DB export named without a version, the one loaded later is
newer. A load not older than the current version supersedes it; an older
version loaded later is stored superseded by the current one, its titles are
not loaded, and `/dats` gives the reason. A new unbound version inherits the
platform its family is bound to only when that is a single platform;
otherwise it stays unbound and `/dats` lists the platforms its family is
current on as `suggested`. At every start, after the keys are refreshed, a
family left with several current versions on one platform keeps the newest
and the others supersede, their titles retire, and the platform's picks are
recomputed.

Removing a version (`DELETE /dats/{id}`) retires its titles and their roms,
clears `wanted` on them and cancels their downloads that have not started,
as unwanting does. It does not bring back an older superseded version: the
file is dropped again for that. A transfer already in progress finishes; it
is placed only if its file matches a live rom of its entry, else it is
quarantined with a reason saying the DAT was removed. The recompute job then
matches the files of retired roms again against live roms, by their stored
hashes and in chunks of 256 per transaction: a cartridge file takes the
state a scan would give it, a disc track is classified again with the other
tracks of its directory under the all-or-nothing rule of "For disc games",
and a file no live rom lists becomes `unverified`. Loads queue the same job.

Two live families on a platform may list the same game. Each title keeps its
own DAT's clone parent in `titles.parent_id`; its effective group is
`titles.group_root`, which browse, `title_groups`, 1G1R and the counts use.
Every recompute rebuilds `group_root` from scratch: each title starts at its
`parent_id`; then a title whose live roms equal, by hash, those of a title in
the largest live version on the platform links to that title's group, and a
title shared only among the other versions links to the group of its
earliest version. Only a single title links, never a group, so two groups of
one DAT never merge through a third-party title, and removing a DAT undoes
its links. Titles with a rom without any hash never link. A clone whose
parent linked away stays in its own DAT's group, which takes its lowest live
id as the new root.

## Name parsing

No-Intro names follow a convention: `Title (Region[, Region]) (Language[,...])
(Rev N) (Beta) (Proto) (Demo) (Sample) (Unl) [b] ...`. Redump adds `(Disc N)`
and `(Track N)` on roms. Parse into:

- `base_name`: everything before the first parenthesised or bracketed tag,
  after any leading bracket tags such as `[BIOS]`
- `regions`: from the first tag whose comma-separated tokens include at least
  one known region name; unknown tokens in that tag are kept as written
- `languages`: from the first tag whose tokens are all two-letter codes
  (`En`), optionally with a subtag (`Zh-Hant`, `Pt-BR`)
- `revision`: `Rev N`, `Rev A`, `v1.1`, `(Alt N)`, `(Beta N)`, `(Proto N)`,
  kept as the tag text with a sortable rank. The rank orders by version
  (`Rev N` counts as `v1.N`, an untagged name as `v1.0`), then stage (proto,
  beta, release), then the beta or proto number, then the alt number
- `flags`: `bios` (also from a `[BIOS]` prefix), `beta`, `proto`, `demo` (also
  `Kiosk`), `sample`, `unl`, `pirate`, `program`, `baddump` from `[b]`, plus
  `disc:N` for Redump

Parsing is table-driven and lives in `mistarr-core::naming` with a large unit
test corpus of synthetic names. Unknown tags are kept in `flags` as `other:...`
(bracket tags keep their brackets, as `other:[!]`) and never cause a parse
failure.

## Clone grouping

Use `cloneof` when the DAT provides it. Otherwise group by
`(platform_id, naming::group_key(parsed))`, which is `base_name` passed
through the pre-download match normalisation below, and elect the parent as
the variant that would win 1G1R under default preferences. This inference is marked `inferred` on the
title so the UI can show it and a later parent/clone DAT can replace it.

## 1G1R selection

Given a clone group and preferences, pick the best variant:

1. Drop anything with a hidden flag (`bios`, `beta`, `proto`, `demo`,
   `sample`, `program` by default; `unl` and `pirate` are shown but rank
   last).
2. Rank by region preference order. A title with several regions ranks by the
   best one it has.
3. Among equal region rank, prefer the one containing a preferred language.
4. Prefer the highest revision if `prefer_latest_revision`, else the lowest.
5. Prefer `status = good` over `baddump`.
6. Tie-break on the shortest name, then lexical, so the choice is
   deterministic.

The pick is recomputed for a platform whenever preferences change or a DAT is
loaded. It is stored on the title so the browse query is a join, not a
computation.

## Hashing

One streaming pass computes CRC32, MD5 and SHA1 together. The reader is
wrapped with the platform's header rule (PLATFORMS.md). Buffer is 256 KiB.
Throughput on the DE10-Nano is roughly 40 to 60 MB/s, which is faster than
the SD card, so hashing is I/O bound; do not add threads.

For zip archives, read the central directory first. For each member compare
the stored CRC32 and uncompressed size against the rom index. Only members
with a candidate match are decompressed and fully hashed. Members with no
candidate are recorded as `unverified` with their CRC only.

For disc games hash every track. The game is `verified` only if every rom in
the DAT entry matched. A cue with tracks missing is `incomplete`, shown as
unverified with a reason.

## Matching order

1. SHA1, exact.
2. MD5, exact, only if the DAT lacks SHA1 for the entry.
3. CRC32 plus size, only if the DAT lacks both.
4. Otherwise `unverified`.

A file may match roms in more than one DAT version (an old and a new one).
Prefer the non-superseded version. A file may match roms in more than one
title (identical dumps under different names); record the first by title id
and note the alternates in the file's detail.

## Pre-download matching

Torrent file lists carry names and sizes only, so a match only decides which
file to offer for a rom; the hashes after the transfer decide what the file
is. Once a source is bound to a platform, each file's leaf name is matched
against that platform's live, non-BIOS roms in tiers, and the confidence of
the tier is recorded:

| Tier | Rule | Confidence |
|---|---|---|
| 1 | Exact name, or name after `naming::normalize_for_match` (NFKC, lowercase, each of ``&*/:`<>?\|"`` replaced by `_`, whitespace runs collapsed) | `name` |
| 2 | Base name (before the first parenthesised tag) plus size | `base` |
| 3 | Fuzzy: size plus a name signal, for files tiers 1 and 2 left unmatched | `fuzzy` |
| 4 | Size only, under the narrow rule below | `size` |

Tiers 1 and 2 store their first rom in `torrent_files` and every further rom
the same tier found for the file in `torrent_candidates`. Tiers 3 and 4 run
only on a platform of cartridge kind (PLATFORMS.md), and only on files whose
extension that platform loads; a zip, a `7z`, and every disc image, cue sheet
or track are never considered, since a size says nothing about them. They
write `torrent_candidates` only.

- **Size.** A candidate rom's size equals the file's, or the file's less the
  header the platform's hashing skips (16 bytes for iNES, 64 for Lynx, 128
  for Atari 7800, 512 for an SMC copier header), the lengths
  `mistarr_core::hash::HeaderRule` defines. Roms are looked up by size
  through the `roms_size` index, one query per distinct file size.
- **Words.** A name's words are the part before its first `(` or `[` tag,
  lowercased and split on anything but letters and digits. Version tags say
  nothing about the title and are dropped: `vN`, `vN.N`, a dotted number such
  as `1.0.6` (also when glued to a word, as in `nova1.0.6`), `rev` with the
  short token after it, `ver` and `version`. Numbers and roman numerals from
  `ii` to `xx` are kept, since they tell sequels apart.
- **Name signal.** A file's words and a rom's base words signal when they are
  equal, or spell the same letters run together (`novathesquirrel` and "nova
  the squirrel"), or when the file's are a leading prefix of the rom's
  (`nova` of "nova the squirrel") and the rom's next word is not a number or
  a roman numeral. So `quest 2` never signals with "quest 3" nor with
  "quest", `quest` never signals with "quest 2", and a longer file name never
  signals with a shorter rom name.
- **Whose words.** A file's words are its stem's when no other considered
  file of the torrent has the same stem words; otherwise, or when the stem
  finds nothing, its parent directory's, when no other considered file has
  the same directory words. A leaf such as `game.nes` shared by every folder
  of a pack therefore signals by its folder or not at all.
- **Fuzzy.** A file names the roms of its size whose words equal its own, or,
  when none do, those its words lead. When those roms span more than 4
  effective clone groups (`group_root`, so versions of one entry count once)
  the name is ambiguous and the file gets none.
- **Size only.** When tier 3 finds nothing for a file, it names every rom of
  its size, provided there are 1 to 4 such roms and the file is the only file
  of the whole torrent with a considered extension, matched or not. A set
  torrent never meets this.

`best_file` offers a file of tiers 1 and 2, or one a hash proved, before any
file of tiers 3 and 4, whatever the sizes; within a tier, a file of the rom's
size, or of that size plus the platform's header, comes first.

A pair a `bad` download already ruled out is not stored again. The mapping is
worked out when the source binds or is rebound, and again for every source
bound to a platform when that platform's DATs, live roms or effective
groups change, including after each recompute; the source's `map_stamp`
records the DAT versions, roms and groups it was worked out against, so an
unchanged platform is skipped. Unbinding or removing a source drops its
candidates and matches, hash proofs included. Post-download hashing is
authoritative: once an import proves a file to be a rom, `torrent_files`
names that rom with confidence `hash`, the file loses its candidates and no
later mapping changes it while the rom stays live, nor does rebinding the
source to the same platform. A proof on a rom that retired, as when its DAT
is removed, is dropped by the next mapping. Only a file itself is proven,
never a zip from one of its members. See ARCHITECTURE.md
"Import" for a file that turns out to be another version.

## Trust

Only DAT hashes decide `verified`. Torrent piece hashes prove the file matches
the torrent, which is necessary but says nothing about provenance. The client's
own "checked" state is used solely as the signal to begin import.
