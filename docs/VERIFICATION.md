# Verification, matching and 1G1R

## DAT parsing

Input is Logiqx XML: a `<datafile>` with a `<header>` and repeated `<game>`
elements, each with one or more `<rom>` children. Parse with `quick-xml` in
streaming mode so a 50 MB DAT does not need to be held in memory twice.

Extract per game: `name`, `cloneof`, `romof`, `description`, `category`,
and per rom: `name`, `size`, `crc`, `md5`, `sha1`, `status` (default `good`),
`header`. Ignore everything else. Reject files whose root element is not
`datafile` or that have no games. Hash attributes are stored lowercase and
must have their full hex length; a malformed hash, size or status rejects the
file. `dat::DatStream` yields one game at a time for importers that write as
they read.

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
only on files whose extension the platform loads (PLATFORMS.md), plus `zip`
for a cartridge platform; `7z` is never considered, since the importer cannot
read it. They write `torrent_candidates` only.

- **Size.** A candidate rom's size equals the file's, or the file's less the
  header the platform's hashing skips (16 bytes for iNES, 64 for Lynx, 128
  for Atari 7800, 512 for an SMC copier header). Roms are looked up by size
  through the `roms_size` index, one query per distinct file size.
- **Name signal.** Both names are split into lowercase runs of letters and
  digits; tokens made only of digits, and the version tags `vN`, `rev`, `ver`
  and `version`, are dropped. The file stem's tokens and the rom's
  `match_base` tokens give a signal when one set is a non-empty subset of the
  other (a prefix is one), or when both spell the same letters run together:
  `nova` and `novathesquirrel` both signal with "nova the squirrel", and
  `example_quest_v2` with "example quest". A stem of fillers alone never
  signals.
- **Fuzzy.** A file names every rom of its size with a signal, unless more
  than 8 do; such a name is ambiguous and gets none.
- **Size only.** When tier 3 finds nothing for a file, it names every rom of
  its size, provided there are 1 to 4 such roms and the file is the only
  file of the torrent with a considered extension that tiers 1 and 2 left
  unmatched. A set torrent never meets this.

A pair a `bad` download already ruled out is not stored again. The mapping is
recomputed when the source binds or is rebound, and for every source bound to
a platform when a DAT loads titles for that platform; unbinding or removing a
source drops its candidates. Post-download hashing is authoritative; see
ARCHITECTURE.md "Import" for a file that turns out to be another version.

## Trust

Only DAT hashes decide `verified`. Torrent piece hashes prove the file matches
the torrent, which is necessary but says nothing about provenance. The client's
own "checked" state is used solely as the signal to begin import.
