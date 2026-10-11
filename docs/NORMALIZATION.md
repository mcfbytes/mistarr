# Normalization

Wave 13 in WORKPLAN.md moves the schema to compact, checked column forms: integer
keys and codes instead of repeated text in the long tables, binary hashes, shared
views for the common read shapes, and a compacted write-back. This file holds the
measurements behind each change and each package's task list. The orchestrator
ticks a task when the pull request that does it merges, and a package when every
task in it is ticked.

Rules for these packages, on top of the ones in WORKPLAN.md:

- The API, the events and every user-facing string stay the same. A migration test
  seeds the previous schema and holds every row equal through a reconstruction
  (full names, `hex()` hashes, code text) before and after.
- No caches and no stored derived results stand in for a query: reads use the
  B-tree indexes, held by the plan tests in `crates/mistarr-server/src/db/plans.rs`.
- A data-rewriting migration drops a table's old indexes before copying it, so the
  file never grows past its starting size, and runs through the copy in RAM when
  memory allows (ARCHITECTURE.md "DAT import in RAM").
- A package records the synthetic catalogue's database size before and after in
  DATA-MODEL.md.
- Unreleased migrations may be merged into one file, so the board rebuilds each
  table once per release.

## Progress

- [ ] **Wave 13**: after wave 12
  - [ ] WP-83 Column forms: design · Opus
  - [ ] WP-85 Rebuild-safe migrations · Flash
  - [ ] WP-86 Hash blobs and the roms rebuild · Opus
  - [ ] WP-87 Rom names as title tails · Opus
  - [ ] WP-88 Torrent paths and confidence codes · Flash
  - [ ] WP-89 Group names from titles, unused title indexes · Flash
  - [ ] WP-90 Tag tables · Opus
  - [ ] WP-91 Compact write-back · Opus
  - [ ] WP-92 Integer platform ids · Opus, then Flash
  - [ ] WP-93 Minimal types · Flash
  - [ ] WP-94 Shared views · Opus

## Measurements

The numbers below come from a copy of the board's database after the second board
test: 120.8 MB at schema 0021, with 145,915 roms, 47,043 titles, 52,541
torrent_files, 14,036 torrent_candidates, 13,983 files, 29,168 title_groups, 55
sources and 67 DAT versions. Each was measured on an in-memory copy with SQLite
3.46.1 on x86-64. "Baseline" is that copy with migration 0022 applied and
vacuumed, 108.23 MiB; "board start" is 0022 applied in place, 118.5 MiB. Values
that only Rust derives were emulated and checked against the stored ones:
`match_name` and `match_base` match on every row.

Sizes are on the baseline, in MiB; `auto:` is a UNIQUE or PRIMARY KEY autoindex.

| Table | Rows | Total | Table and each index |
|---|---|---|---|
| roms | 145,915 | 67.68 | table 31.87, auto:(title_id,name) 8.18, match_name 7.61, sha1 6.68, match_base 5.49, crc 2.90, id_title 2.09, size 1.68, track_size 1.12, md5 0.06, chd_size 0.00 |
| titles | 47,043 | 14.35 | table 6.25, auto:(dat_version_id,name) 2.22, group 1.74, platform_base 1.68, source 0.73, id_dat 0.57, group_root 0.48, parent 0.48, mra_path 0.18 |
| torrent_files | 52,541 | 7.35 | table 5.55, source_rom 0.63, auto:PK 0.62, rom 0.54 |
| title_groups | 29,168 | 6.09 | table 2.88, have 1.13, name 1.10, counts 0.54, recent 0.44, split 0.00 |
| title_search (fts5) | | 4.76 | data 4.26, docsize 0.48 |
| files | 13,983 | 4.58 | table 2.11, rel_lower 1.00, auto:(platform_id,rel_path) 1.00, state 0.32, rom 0.14 |
| title_regions | 48,486 | 1.54 | region index 0.88, table 0.66 |
| title_flags | 21,120 | 0.80 | table 0.41, flag index 0.39 |
| torrent_candidates | 14,036 | 0.42 | table 0.24, rom 0.18 |
| title_languages | 16,934 | 0.37 | table 0.20, language index 0.17 |
| jobs, chd_*, sources, dat_versions, downloads, import_log, settings | ≤ 253 | 0.20 together | |

**roms** (63% disc tracks: psx 61,361, saturn 30,575, megacd 8,426, neocd 3,357; 94,079 sizes divisible by 2352)
- Hex hashes:
  - crc32 is 8 B, 140,800 non-null, 114,528 distinct; 71,017 values are ≥ 0x80000000, so an INTEGER would need 6 bytes.
  - md5 is 32 B; sha1 is 40 B.
  - sha1 has 114,530 distinct values over 140,801 rows, so 26,271 (18%) are duplicates.
  - 5,114 roms have no sha1, all of them MRA zips.
- Text enum: `status` uses 3 values (good 133,258, verified 12,540, baddump 117), average 4.3 B.
- Repeated text: `name` averages 45.8 B.
  - 139,871 names (95.9%) start with their title's `name`.
  - What follows has only 182 distinct values: `.cue` 14,297, `.bin` 6,780, ` (Track 1).bin` 4,838, …
  - Only one name equals its title's name exactly.
  - Names that do not start with the title's name: 5,114 MRA zips and 930 DAT disc members such as `USA\FILE.BIN`.
- Derived:
  - `match_name` (41.8 B, 123,541 distinct) is `normalize_for_match(strip_ext(leaf(name)))`. It needs Rust (NFKC and Unicode lowercase).
  - `match_base` (22.7 B, 27,408 distinct) is `match_name` cut before its first `(` and right-trimmed, which SQL can reproduce exactly.
  - All 145,915 roms are keyed on the board, so the partial indexes `WHERE match_base IS NOT NULL` hold every rom.
- Sparse: `header` has 4,576 non-null (1,003 distinct, verbatim DAT text); `zip_dir` has 5,114 non-null with 2 values (mame, hbmame).

**titles**
- `platform_id` is TEXT with 31 values, average 4.0 B.
- `source` is an enum: dat 44,345, mra 2,698.
- Derived from `name` in Rust: `base_name` (23.8 B, 25,526 distinct) and `group_key` (24.0 B, 25,832 distinct).
- Nine MRA-only columns are non-null on at most 2,698 rows.
- `clone_of` is non-null on 1 row; `parent_id = group_root` on every row.
- Unused indexes:
  - Nothing reads `titles` by `parent_id`, and titles are never deleted, so the foreign-key child lookup never runs.
  - Nothing filters or sorts by `(platform_id, base_name)` beyond the `platform_id` prefix, which `titles_source` and `titles_group` already serve.

**torrent_files**
- `path` averages 83.6 B, made of a directory prefix (38.4 B) and a leaf (44.2 B).
- There are only 55 distinct (source, directory) pairs: one prefix per torrent here.
- `confidence` is an enum: name 41,913, NULL 10,628.
- It is a rowid table with a (source_id, file_index) PK autoindex, and nothing reads its rowid.

**title_groups**
- `name`, `base_name`, `source` and `platform_id` equal the root title's on 29,168 of 29,168 rows.
- `name` (37.7 B) is only displayed. `base_name` is the sort key of two indexes, and `source` is in `title_groups_counts`.

**files**
- Hex hashes: crc32 12,957, md5 and sha1 6,332, `*_whole` about 1,000.
- `state` uses 5 values (unverified 8,553, verified 5,156, misnamed 257, bad 14, unidentified 3); `header_rule` 5; `platform_id` 20.
- `rel_path` averages 58.4 B across 120 directories.

**Title tags:** `title_flags.flag` has 3,551 distinct values (beta, unl, `disc:N`, many `other:…`), average 10.8 B. `title_regions.region` has 204, `title_languages.language` 30.

**Small tables:**
- `jobs`: kind 9 values, lane 3, state 1.
- `sources`: `infohash` is hex text read as a `String`; `bind_pending` mixes an enum with an FK (`platform:<id>`).
- `dat_versions.family` is derived from `dat_name` and rewritten at every start, which is fine.

**Planner finding on real data:** `SqlDatIndex::by_base_name_and_size` (`WHERE r.match_base = ?1 AND r.size = ?2 … ORDER BY r.id`) is planned as `SEARCH r USING INDEX roms_size (size=?)`. The index returns rows in id order, so the planner skips the sort, but real sizes repeat: 5,114 roms of size 0, 4,554 of 1 MiB, 3,186 of 256 KiB. For 3,000 sampled lookups:

| Query shape | Time | Index used |
|---|---|---|
| As written | 0.218 s | `roms_size` |
| `ORDER BY +r.id` | 0.011 s | covering `roms_match_base` |
| No ORDER BY | 0.008 s | covering `roms_match_base` |
| As written, after ANALYZE | 0.015 s | covering `roms_match_base` |

The synthetic catalogue's random sizes hide this from the plan tests.

## Proposals

Savings are measured against the baseline: each change is rebuilt alone, then vacuumed. Ranked by value over effort.

| # | Change | Saved (MiB) | Effort | Verdict |
|---|---|---|---|---|
| P0 | Base+size lookup keeps `roms_match_base` on repeated sizes | 30x faster lookup | tiny | **Done**: WP-81's `INDEXED BY` and a plan test with repeated sizes |
| P1 | Hashes as BLOB(4/16/20) in roms, files, chd_tracks, chd_whole, chd_failures | 9.02 roms + 0.48 files/chd | medium | **Recommend** |
| P2 | Drop `roms.match_base`; index the SQL expression over `match_name` | 3.53 | small | **Recommend if** WP-82's board bench allows (loses covering) |
| P3 | `roms.name` as the tail after the title's name, with `own_name` flag | 9.93 | medium-high | **Recommend** |
| P4 | `roms.status` integer code; STRICT and CHECK on every rebuilt table | 0.66 | small, inside P1 | **Recommend** |
| P5 | torrent_files WITHOUT ROWID, `torrent_dirs` interning, integer confidence (candidates too) | 2.86 + 0.05 | small-medium | **Recommend** |
| P6 | Drop `title_groups.name`, `titles_parent`, `titles_platform_base` | 0.92 + 0.48 + 1.68 | small | **Recommend** |
| P7 | `tags` table for flags, regions and languages, absorbing `known_flags` and `known_regions` | 0.69 | medium | **Recommend**, later |
| P8 | Write-back via `VACUUM INTO` when free pages > 25% and memory allows | realizes the rest: file 118.5 → 79.0 | medium | **Recommend** |
| P9 | migrate.rs: a migration may run with foreign keys off, then `foreign_key_check` | enables P1, P3, P5 | small | **Recommend**, first |
| P10 | Integer platform ids: `platforms.id` INTEGER, slug kept once | ~0.95 | high (about 250 SQL lines) | **Recommend**, folded into the rebuilds |
| P11 | Minimal types: integer codes for every enumeration, 0/1 booleans, NULL for absent values, STRICT everywhere | ~0.6 plus faster compares | medium, mechanical | **Recommend** |
| P12 | Shared views for the common read shapes | none (readability, one definition) | medium | **Recommend**, before the conversions |

Measured together (P1 to P6, no P7): **108.23 → 78.93 MiB (-29.30, -27%)**. The board start, migrated with peak-free ordering and vacuumed: **118.5 → 79.0 MiB**, integrity ok. P7 adds about 0.7 MiB more.

Combinations measured on the roms family (67.68 MiB):

| Combination | Total (MiB) |
|---|---|
| P1 + P2 + P4 | 95.09 (-13.14) |
| that + P3 | 85.23 (-23.00) |
| P1 + P2 + P4 with match keys from a Rust function index (no `match_*` columns) | 88.80 (-19.43) |

### P1 Hashes as BLOBs
```sql
crc32 BLOB CHECK (length(crc32) = 4), md5 BLOB CHECK (length(md5) = 16), sha1 BLOB CHECK (length(sha1) = 20)
-- copied with unhex(); same in files (and *_whole), chd_tracks, chd_whole, chd_failures.chd_sha1
```
- **Why:** halves every hash key: `roms_sha1` 6.68 → 3.94 MiB, `roms_crc` 2.90 → 2.36 MiB, and the roms table about -9 MiB. In a STRICT table the CHECK rejects a hex string written by mistake, and a wrong width can no longer be stored.
- **Plans:** sha1, md5 and crc32+size tiers seek the same indexes. 20k sha1 lookups: 0.057 s as text, 0.058 s as BLOB (x86).
- **Code:**
  - `mistarr-core/src/digest.rs`: `Digest<N>`'s ToSql becomes `Blob(&self.0)`, and FromSql reads a BLOB of exactly N bytes. It is one conversion for every hash column, so all tables switch in the same release.
  - `db/titles/recompute.rs:388`: `r.crc32 || ':' || r.size` becomes `r.crc32 || CAST(r.size AS BLOB)`, read as bytes.
  - `synth.rs` (hex strings), test SQL with hex literals (59 `repeat()` sites plus literals), and the "stored as lowercase hex text" paragraph of DATA-MODEL.md.
  - The API is unchanged: serde still writes hex.
- **Risks:** the CLI shows binary (use `hex()`). A text value bound by mistake matches nothing; STRICT and CHECK catch the writes, and tests catch the reads.
- **Out of scope:** `sources.infohash` is not bound through `Digest`. It stays TEXT; converting it is optional (55 rows).

### P2 `match_base` as an expression index
```sql
CREATE INDEX roms_match_base ON roms((CASE WHEN instr(match_name,'(') > 0
  THEN rtrim(substr(match_name, 1, instr(match_name,'(') - 1)) ELSE match_name END), size, title_id)
  WHERE match_name IS NOT NULL;
CREATE INDEX roms_size ON roms(size) WHERE match_name IS NOT NULL;
```
- **Why:** removes a derived column that could drift, keeps one source of truth (`match_name`), and saves 3.53 MiB.
- **Plans:** `SEARCH r USING INDEX roms_match_base (<expr>=? AND size=?)`.
  - On 3.46 the expression index is **not covering**, with the predicate on the column or on the expression. Each hit reads its roms row (about 6 hits per lookup).
  - x86 time is equal (0.012 to 0.014 s per 3,000 lookups, against 0.011 s covering).
  - Decide with WP-82's board bench; if the base tier is hot there, keep the column.
- **Code:**
  - One `const MATCH_BASE: &str` holding the expression, used by `SqlDatIndex` and WP-82's statement.
  - `key_batch` writes only `match_name`.
  - The `candidates` size-index predicate and the plan tests change.

### P3 Rom names as title-name tails
```sql
own_name INTEGER NOT NULL CHECK (own_name IN (0, 1)),  -- 0: full name is titles.name || name
name     TEXT NOT NULL,
CREATE UNIQUE INDEX roms_title_name ON roms(title_id, own_name, name);   -- replaces UNIQUE(title_id, name)
CREATE TRIGGER titles_name_fixed BEFORE UPDATE OF name ON titles WHEN NEW.name IS NOT OLD.name
BEGIN SELECT RAISE(ABORT, 'titles.name never changes'); END;
```
- **Rule:** `own_name = 0` only when the full name starts with the title's name and is longer, so the stored text always keeps the extension. `lower(name) LIKE '%.chd'` (the `roms_chd_size` index) and `'%.cue'` stay valid on the stored text.
- **Why:** removes the title name repeated in 96% of roms. The table drops about 5.5 MiB and the unique index 8.18 → 3.35 MiB, 9.93 MiB in all.
  - It is safe because `titles.name` is never updated in production: `put_title` finds rows by name, and only `groups/tests.rs:646` renames a title.
  - The trigger makes that invariant a constraint.
- **Plans:**
  - Title detail is unchanged (`sqlite_autoindex`/`roms_title_name (title_id=?)`).
  - Lookups by exact name (`files::unmatch_changed_rom`, arcade `lower(name) = lower(?)`) become a title-range seek plus a title PK seek, one title's roms at most.
  - Source files page: one more PK seek per page row.
  - 5k title rom-name reads take 0.009 s stored and 0.013 s rebuilt from the tail (x86).
- **Code:**
  - `const ROM_NAME: &str = "CASE WHEN r.own_name THEN r.name ELSE t.name || r.name END"`.
  - One Rust `rom_name_parts(title, name)` used by both upserts: `titles::upsert_title` `ON CONFLICT(title_id, own_name, name)`, and `arcade.rs:263`.
  - About 26 SQL reads of `r.name` across 17 files (roms.rs 6, titles 3, chd, launch, candidates, downloads, files, source_detail, dat_stage, jobs/arcade, jobs/chd, dat_import/stream 2, import/mra, import/support).
- **Incompatible with** match keys indexed through a Rust function (see Rejected).

### P4 `status` codes, STRICT and CHECK
- **Change:** `status INTEGER NOT NULL CHECK (status BETWEEN 0 AND 3)` (good 0, baddump 1, nodump 2, verified 3), plus 0/1 CHECKs on the flag columns, in the P1 rebuild at no extra rewrite.
- **Code:**
  - A `code_enum!` beside `text_enum!` in `db/sql.rs`: ToSql/FromSql use the integer code; `as_str`, `Display` and serde keep the text, so the API is unchanged.
  - A `const fn sql(self) -> &'static str` gives literals for `format!`.
  - `RomStatus` has 4 SQL literal sites.
- **Enums on other tables:** not converted (see Rejected). Their Rust enums already refuse unknown text on read.

### P5 torrent_files
```sql
CREATE TABLE torrent_dirs (id INTEGER PRIMARY KEY, source_id INTEGER NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
  path TEXT NOT NULL, UNIQUE (source_id, path)) STRICT;          -- 'dir/' with its slash, '' for top level
CREATE TABLE torrent_files (source_id INTEGER NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
  file_index INTEGER NOT NULL, dir_id INTEGER NOT NULL REFERENCES torrent_dirs(id), leaf TEXT NOT NULL,
  size INTEGER NOT NULL, rom_id INTEGER REFERENCES roms(id),
  confidence INTEGER CHECK (confidence BETWEEN 0 AND 2),         -- hash 0, name 1, base 2
  PRIMARY KEY (source_id, file_index)) STRICT, WITHOUT ROWID;
-- torrent_candidates.confidence INTEGER CHECK (BETWEEN 1 AND 4): name 1, base 2, fuzzy 3, size 4
```
- **Why:** each directory prefix is stored once per source (it repeated on every row), there is no rowid or autoindex, and a source's files are clustered by key.
  - Saved: WITHOUT ROWID alone 0.73 MiB; with directories and codes 2.86 MiB (7.77 → 4.91 including candidates).
  - Paths round-trip exactly.
- **Plans:**
  - Files page: `SEARCH f USING PRIMARY KEY (source_id=?)`, then a `torrent_dirs` PK seek per row.
  - The DAT list stays `COVERING INDEX torrent_files_source_rom`.
  - Search becomes `instr(lower(d.path || f.leaf), ?)` over the same per-source range.
- **Code:** about 13 path sites (sources.rs insert and reads, source_detail 3, candidates 2, downloads 2, downloads_import 2, jobs/source_import), the staged path, and `MatchConfidence` as a `code_enum!`. The API returns `d.path || f.leaf`.

### P6 Group names from titles; unused title indexes
- **Change:** `ALTER TABLE title_groups DROP COLUMN name; DROP INDEX titles_parent; DROP INDEX titles_platform_base;`. Recreate `titles_rename_groups` without `name` in its column list and WHEN.
- **Why:** one source of truth for the display name, and two indexes nothing reads. Saved 0.92 + 2.16 MiB.
- **Plans:** browse adds `SEARCH p USING INTEGER PRIMARY KEY` for the page's 50 rows; recompute keeps `titles_group` and `titles_source`.
- **Code:**
  - `groups.rs` COLUMNS and refresh drop `name`.
  - `browse.rs:310` reads `p.name` via `JOIN titles p ON p.id = g.parent_id`.
  - Tests that select `g.name` change.
  - The plan test is the gate for the index drops.

### P7 Tag tables
```sql
CREATE TABLE tags (id INTEGER PRIMARY KEY, kind INTEGER NOT NULL CHECK (kind BETWEEN 0 AND 2),   -- flag, region, language
  name TEXT NOT NULL, bit INTEGER, UNIQUE (kind, name)) STRICT;   -- bit: the known_flags/known_regions bit, NULL otherwise
-- title_flags/regions/languages(title_id, pos, tag_id) WITHOUT ROWID, PK (title_id, tag_id), index (tag_id, title_id)
```
- **Why:** 3,551 flag strings and 204 regions are stored once; one table replaces `known_flags` and `known_regions`. Saved 0.69 MiB (2.71 → 2.02).
- **Code:**
  - `groups.rs` bit computation and `visible()`.
  - Browse region filter (NOCASE becomes `UNIQUE (kind, name COLLATE NOCASE)` for regions).
  - `detail.rs` lists, `store_lists`, and the bios checks.
  - Known tags take fixed ids, so `f.flag = 'bios'` becomes `tag_id = <bios id>`.

### P8 Compact write-back (`db::ram`)
- **Change:** after an import or migration in RAM, when free pages exceed 25% of the copy and `MemAvailable` covers floor + copy + live pages, run `VACUUM INTO <dir>/compact.db` and write that back instead of the copy.
- **Why:** dropped tables leave free pages; without this the card keeps 118.5 MiB holding 28 to 53 MiB of free pages.
- **Measured:** VACUUM takes 0.18 to 0.32 s on x86, ending at 79.0 MiB.

### P9 Rebuild-safe migrations
- **Problem:** migrations run inside a transaction with `foreign_keys` ON, where the pragma cannot be changed. `DROP TABLE roms` then fails on its children.
- **Change:** a migration whose first line is `-- foreign_keys: off` runs with them off. `migrate.rs` must also run `PRAGMA foreign_key_check` before commit and fail the migration on any row. The pragma goes back on after commit or rollback.
- **Triggers:** a renamed table fails `ALTER TABLE … RENAME` while triggers on other tables name the missing one (seen in the prototype for `files_*_groups`). Each rebuild migration drops and recreates those triggers explicitly; no `legacy_alter_table`.

### P10 Integer platform ids
```sql
CREATE TABLE platforms (id INTEGER PRIMARY KEY, slug TEXT NOT NULL UNIQUE, ...) STRICT;
-- every platform column (titles, title_groups, files, dat_versions, sources, downloads, ...)
-- becomes `platform INTEGER NOT NULL REFERENCES platforms(id)`
```
- **Why:** the slug is stored once, so a rename is one row and every reference is a checked integer. The biggest indexes (`titles_group`, `title_groups_*`, the `files` autoindex) lead with a 1-byte integer instead of a text compare. Measured about 0.95 MiB on titles, title_groups and files.
- **Ids:** the platform table seeds fixed ids that are never reused, so migrations, fixtures and tests agree across installs.
- **Code:** about 250 SQL lines name a platform column. `PlatformId` stays the slug newtype at the API and crate boundaries; a `PlatformKey` integer newtype crosses `db` functions, resolved once per request. Reads that only display the slug go through P12's views and keep their SQL.
- **Order:** folded into the rebuilds that already rewrite the same tables (titles and title_groups in WP-89, files in WP-86), so no table is rebuilt twice. WP-83 fixes the grouping.

### P11 Minimal types
SQLite stores each record as a header of serial types followed by the values. An integer takes 1, 2, 3, 4, 6 or 8 bytes by its value, the integers 0 and 1 and NULL take no bytes beyond their header byte, and text and blobs take their length. Nothing compresses rows further, so the type choice is the lever:
- every enumeration column is an integer code through a `code_enum!` beside `text_enum!`, on small tables too (jobs kind, lane and state; downloads, sources, import_log, dat_versions, `files.state`, `files.header_rule`, `titles.source`, `title_groups.source`), about 0.6 MiB in all, and integer compares in every filter;
- booleans are 0/1 with a CHECK, never text;
- an absent value is NULL, not `''` or a sentinel (`titles.group_key`, `jobs.subject`, `dat_versions.family` default to `''` today);
- times are integer Unix seconds and sizes integers, never REAL or text;
- every table is STRICT, so a value of the wrong type is refused on write.
P12's views decode codes to their text with `CASE`, so the CLI and the API keep reading words.

### P12 Shared views for read shapes
SQLite flattens a view whose body has no aggregate, DISTINCT, LIMIT, window function or compound select into the outer query, so a read through such a view plans exactly like the written-out join. Views:
- `rom_view`: a rom with its full name (P3's tail rule), hex hashes (`lower(hex(sha1))`), status text, the title's platform slug, and its live and BIOS state;
- `live_rom_view`: roms of live, non-BIOS titles, the predicate binding, candidates and the size index repeat;
- `title_view`: a title with its platform slug and source text;
- `torrent_file_view`: a torrent file with its full path (`d.path || f.leaf`) and confidence text.
Rules: lookups by hash compare the BLOB column of the table, never a view's hex expression, which no index serves; writes go to the tables; every view lives in a migration and in DATA-MODEL.md; a plan test runs each hot read through its view and fails on a `MATERIALIZE` or `CO-ROUTINE` step or a lost index.

### Hot reads before and after (EXPLAIN QUERY PLAN, composite prototype)

| Read | Before | After |
|---|---|---|
| Browse page | `title_groups_name (platform_id=?)`, pick PK | same + root title PK seek |
| Search | fts5 MATCH + `title_groups_split` | unchanged |
| Title detail | `titles_group_root` (covering), roms `(title_id=?)` | same, not covering on titles (t.name read for the tail) |
| Source files page | autoindex `(source_id=?)`, roms PK | `PRIMARY KEY (source_id=?)`, dirs PK, roms PK, titles PK |
| Source DAT list | covering `torrent_files_source_rom`, PK seeks | unchanged |
| Bind by name | covering `roms_match_name` | unchanged |
| Bind base+size | covering `roms_match_base` (`INDEXED BY`) | `roms_match_base (<expr>=? AND size=?)` with `+r.id` |
| sha1 / md5 / crc tiers | `roms_sha1`, `roms_md5`, `roms_crc (crc32=? AND size=?)` | same, BLOB keys |
| Rom by title and name | covering `(title_id=? AND name=?)` | titles PK, `roms_title_name (title_id=?)` |
| Track sizes, chd sized | `roms_track_size`, `roms_chd_size` | unchanged |

### Rejected

| Proposal | Measured | Why rejected |
|---|---|---|
| `match_name` as an expression index over a deterministic Rust function `match_key(name)` | -6.3 MiB beyond P1+P2+P4; would end key batches and give WP-82 keys without storage | Cannot combine with P3, which saves more. Stock `sqlite3` cannot write roms or check that index. Index contents would depend on the binary's Unicode tables (a REINDEX on every normalizer change). Revisit if P3 is dropped. |
| Lookup tables for enums | none | The Rust enum is the source of truth; CHECK on the code range suffices |
| MRA columns in a `title_mra` side table | 0.16 MiB | Too little for the code churn |
| fts5 without the platform column | 0.99 MiB | `FtsPlatform` won the board search bench (ARCHITECTURE.md) |
| Hash interning table | ~1 MiB estimate (18% duplicate sha1) | Every tier lookup becomes two seeks |
| roms WITHOUT ROWID | none | Rom ids are FK targets of four tables, and rows are wide |
| Drop `roms_id_title` / `titles_id_dat` | none | Still about 8x denser than the narrowed rows; WP-81's reads need them |
| `auto_vacuum=INCREMENTAL` | none | Pointer-map writes on every commit on a sync card; P8 compacts on write-back instead |
| ANALYZE / `sqlite_stat1` | fixes P0 | Plans would follow statistics rather than the plan tests; fix the query instead |
| page_size 8 or 16 KiB | not measured (an in-memory copy cannot change page size) | ARCHITECTURE.md fixes 4 KiB for per-write cost and cache size |
| A stored match key on torrent_files (WP-82) | ~4.5 MiB estimate (table + index) | A derived copy that drifts; WP-82 binds keys computed in Rust as a list |
| Merging `roms_track_size` into `roms_size` | 1.12 MiB | Different predicates: tracks must be found before binding keys roms |
| Not changed, by design | | `match_name`, `base_name`, `group_key`: Rust-only derivations, written once and indexed. `title_groups` is the existing derived table. `sources.file_count` and `total_size` are written once from the torrent. |

## Packages

Order: WP-83, then WP-85 and WP-94, then WP-86 with WP-87 and the files part of
WP-92, then WP-88, WP-89 with the titles part of WP-92, and WP-93, then WP-90;
WP-91 follows WP-77. WP-82 lands before WP-86.

### WP-83 Column forms: design
- [ ] DATA-MODEL.md "Column forms": every column of every table with its target form (kept TEXT, BLOB(n), integer code with its value table, title tail, interned key, dropped, expression index), its CHECK, and STRICT or WITHOUT ROWID per table.
- [ ] The P2 decision, from WP-82's board bench of the base tier.
- [ ] The migration grouping: which packages share one migration file per release, so each table rebuilds once (P10 folded into WP-86 and WP-89).
- [ ] The views of P12 with their columns, and the plan-test rule for them.
- [ ] Docs only; no code.

### WP-85 Rebuild-safe migrations
- [ ] A migration whose first line is `-- foreign_keys: off` runs with foreign keys off and is rolled back when `PRAGMA foreign_key_check` returns a row; keys are on again after commit and after failure.
- [ ] Tests: a parent table rebuilt under its children passes; an orphan fails and rolls back; the pragma state after each.
- [ ] Each rebuild drops and recreates the triggers of other tables that name the rebuilt one.
- [ ] `crates/mistarr-server/migrations/README.md` documents the marker.

### WP-86 Hash blobs and the roms rebuild
- [ ] One migration rebuilds roms (P1, P2 as WP-83 decides, P4, STRICT and CHECKs, peak-free order) and files, chd_tracks, chd_whole and chd_failures with BLOB hashes, with P10's platform key on files.
- [ ] `Digest<N>` binds and reads BLOB(N); `RomStatus` is a `code_enum!`.
- [ ] A migration test seeds 0022 rows (NULL hashes, MRA roms, baddump, unkeyed roms) and holds every row equal through `hex()` and the status text.
- [ ] Plan tests green; ARCHITECTURE.md sync-write and memory tables measured again.

### WP-87 Rom names as title tails
- [ ] P3 in WP-86's migration while it is unreleased: `own_name`, the unique index on title, flag and name, and the trigger that refuses a title rename.
- [ ] One full-name expression for every read and one split function for both upserts.
- [ ] Tests: every rom's full name equal across the migration, a rom equal to its title name kept whole, arcade zip and disc member names, upserts idempotent, API output byte-identical in the HTTP tests.

### WP-88 Torrent paths and confidence codes
- [ ] P5: interned directories per source, torrent_files WITHOUT ROWID, integer confidence on torrent_files and torrent_candidates.
- [ ] Paths round-trip in a migration test; source detail search, staged paths and downloads unchanged in their tests.
- [ ] A plan test asserts the primary-key range and the directory seek.

### WP-89 Group names from titles, unused title indexes
- [ ] P6 in one migration with P10's platform key on titles and title_groups.
- [ ] Browse reads the root title's name; the rename trigger is recreated.
- [ ] No plan names the dropped indexes; group rows equal across the migration, less the name.

### WP-90 Tag tables
- [ ] P7: one tag table with fixed ids for the known flags and regions, link tables by tag id, the known-flag and known-region tables folded in.
- [ ] Groups' bit columns equal for every group across the migration; browse visibility tests unchanged; `mistarr doctor` finds no drift.

### WP-91 Compact write-back
- [ ] P8 in the copy-in-RAM path for imports and migrations, with its own memory check.
- [ ] A test drops a large table in the copy and gets a smaller card file with identical rows; card writes stay about one per MiB.
- [ ] ARCHITECTURE.md "DAT import in RAM" describes it.

### WP-92 Integer platform ids
- [ ] P10: the platform table with fixed integer ids and the slug, and the `PlatformKey` newtype.
- [ ] Every platform column becomes the integer key, in the rebuild WP-83 assigns it to; the slug crosses the API unchanged.
- [ ] Split into one package per module after the schema part lands, each small enough for Flash.

### WP-93 Minimal types
- [ ] P11 on every table not rebuilt by another package: integer codes through `code_enum!`, 0/1 booleans with CHECK, NULL for absent values, STRICT.
- [ ] One package per table group, each holding every row equal across its migration.

### WP-94 Shared views
- [ ] P12's views in a migration, defined over the current columns, with the reads in `crates/mistarr-server/src/db/` moved onto them where they repeat a shape.
- [ ] A plan test per hot read through a view: same index seeks as the written-out join, no `MATERIALIZE` or `CO-ROUTINE` step.
- [ ] Later packages change a view's body rather than every read.

## Conflicts with wave 12

- **WP-82:**
  - It must not add a match key column to torrent_files; it binds a key list.
  - It edits `SqlDatIndex` and adds a statement over `roms_match_name`/`roms_match_base`, which P2 and P3 later touch. Land WP-82 first, and have WP-86 change both statements through the `MATCH_BASE` constant.
  - Its bench decides P2.
  - P5 changes its sample read (`file_index, path, size` becomes a join with `torrent_dirs`), so WP-88 depends on WP-82.
- **WP-77:**
  - WP-91 rewrites the `db::ram` write-back that WP-77 restructures, so it follows WP-77.
  - WP-86 and WP-87 change the apply SQL (`upsert_title`, `arcade`, `dat_stage`) that WP-77's row-for-row comparison test reads; land WP-77 first or rebase its test onto reconstructed values (`ROM_NAME`, `hex()`).
  - Only WP-91 conflicts with WP-77's code itself.

## Migration safety on the board

Measured on x86 in memory (Python sqlite3 3.46.1) from the board start (118.5 MiB, 0022 applied in place). Board figures are estimates: SQLite CPU on the ARMv7 board taken as 15 to 25 times slower, the card at 10 to 20 MB/s, and 25 ms per synced write (ARCHITECTURE.md).

**Peak-free rebuild order** (measured): drop the table's old indexes → create the new table → `INSERT … SELECT … ORDER BY id` → drop the old table → rename → create the indexes, including the explicit UNIQUE.
- Freed index pages take the new rows, so **the file never grows past 118.5 MiB**.
- Building indexes before dropping peaks at 163.0 MiB; dropping first but building indexes after the copy peaks at 138.7 MiB.

| Migration | x86 time | File: start → peak → end (free) | Board time in RAM | Card writes in RAM / in place |
|---|---|---|---|---|
| WP-86 + WP-87 roms rebuild | 0.52 s | 118.5 → 118.5 → 118.5 (28.1) | ~10 to 15 s CPU | in RAM: part of the one write-back; in place: ~11.4k pages, ~34k writes, **~14 min** |
| WP-86 alone (P1 + P2 + P4) | ~0.5 s | 118.5 → ≤ 118.5 | ~10 s | ~14k pages, ~42k writes, ~17 min |
| WP-87 as a second rebuild | ~0.5 s | ≤ 118.5 | ~10 s | another ~14 min, hence fold it into WP-86 |
| files + chd rebuild (in WP-86) | 0.03 s | reuses free pages | < 1 s | ~1k writes |
| WP-88 torrent_files | 0.30 s | reuses free pages | ~5 s | ~1.3k pages, ~4k writes, ~2 min |
| WP-89 drop column and indexes | 0.01 s | DROP COLUMN rewrites title_groups (2.9 MiB) | < 1 s | ~2.2k writes, ~1 min |
| WP-90 tags | 0.05 s | reuses free pages | ~1 s | ~1.5k writes |
| Compaction (WP-91, VACUUM INTO) | 0.18 to 0.32 s | + 79.0 MiB second file | ~5 s | none extra |

**In RAM** (the normal path, `db::ram::migrate_in_ram`):
- Copy-in reads 118.5 MiB (6 to 12 s).
- Memory peak: the copy (118.5) + rollback journal + CREATE INDEX sorter in `/tmp/mistarr`.
  - Journal: estimated under 10 MiB, since freed pages are reused as freelist leaves; in-place rewrites are files 2.1 and title_groups 2.9.
  - Sorter: estimated under 10 MiB, the largest key set being about 7 MiB.
  - Total about **140 MiB**, inside `need(118.5) = 210 MiB`, so `MemAvailable` ≥ 338 MiB with the 128 MiB floor.
- Compaction needs another 79 MiB, so WP-91 checks memory separately and skips it when short.
- Write-back: 79 MiB compacted, or 118.5 MiB not, at 10 to 20 MB/s, which is about one card write per MiB plus 8.
- Total roughly 0.5 to 1 minute.
- After compaction, later DAT imports need about 59 MiB less (`need(79.0) = 150.5 MiB`) and write back 39.5 MiB less each.

**In place on the card** (fallback when memory is short):
- Every page goes through the WAL: one transaction per migration file, two writes per frame plus one per checkpointed page.
- The WAL grows to about 50 MiB before commit, and the file does not shrink.
- Before starting, check free room ≥ 2 × the rebuilt table's new size and fail with a message naming the card otherwise.
- The startup `mistarr.migrating` progress already shows the work.

**Interruption:**
- Each migration file is one transaction, and startup reruns pending ones before any server code runs, so no intermediate schema is ever served.
- In RAM: a cut before the swap leaves the card file byte-identical (ARCHITECTURE.md swap table); a cut during the renames is recovered by the existing start-up rules.
- In place: uncommitted WAL frames are ignored at the next open, and the migration reruns from the start.
- The hash switch is safe across files because the binary only serves after all migrations apply.

**Rollback:**
- `install.sh` saves `mistarr.db.prev` (with `-wal`/`-shm`) and `mistarr.prev` before the upgrade, and its rollback restores both.
- An older binary refuses the newer schema (`SchemaTooNew`) and leaves the file unchanged, so a binary-only downgrade is safe but needs the DB restored.

**Verification for each data migration:**
- Seed the 0022 schema with representative rows and compare a reconstruction view (full name, `hex()` hashes, enum text, `d.path || f.leaf`) before and after.
- `PRAGMA integrity_check` and `foreign_key_check` must both be clean, and `db::plans` green.
- Record the synth catalogue's size, `sync_writes_on_the_bench_catalogue` and `tests/memory.rs` peaks in the docs.
- A board run reports copy-in, work and write-back times from the migration report.
