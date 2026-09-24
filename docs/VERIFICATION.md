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
- `regions`: from the first tag whose comma-separated tokens are all known
  region names
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
