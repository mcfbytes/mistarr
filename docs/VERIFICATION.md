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
| rom status | `nodump` when the file has `mia="1"`, `baddump` when it has `bad="1"`, else `good` |
| DAT name, version | from the file name, `<System> (DB Export) (<version>).xml` or `.zip`, as `<System> (DB Export)`; the header `<version>` when present |

Only the game image becomes a rom: a file with an `item` attribute, and a
file whose extension is neither the image extension nor `unh`, is skipped.
Those are extras such as save data or a separate chip dump, and a game
needs every rom to verify, so one of them as a rom would keep the game
from ever verifying. The image extension is the platform's written
extension, else the extension of the game's headered file; without either,
every file without `item` counts.

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
`forcename`, its extension is the platform's written extension instead of
`.unh`, else the headered file's.
Two files that would get the same rom name keep the first. A DAT name bound
to no platform takes the last rule, and binding it later reads the file
again under the chosen platform's rule.

Both No-Intro and Redump distribute zipped daily packs. Accept `.zip` and
load every `.dat` or `.xml` member as a separate DAT.

Store the header `name` verbatim; platform binding works on it. Store
`version` verbatim; supersession compares it as a string, newest by
`loaded_at` when equal.

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

Torrent file lists carry names and sizes only. Match a torrent file to a rom
by: exact name match, then name match after `naming::normalize_for_match`
(NFKC, lowercase, each of ``&*/:`<>?\|"`` replaced by `_`, whitespace runs
collapsed), then base_name plus size. Record the confidence. Post-download hashing is authoritative and can reassign the
file to a different rom; when that happens the UI shows the original
expectation and the actual match.

## Trust

Only DAT hashes decide `verified`. Torrent piece hashes prove the file matches
the torrent, which is necessary but says nothing about provenance. The client's
own "checked" state is used solely as the signal to begin import.
