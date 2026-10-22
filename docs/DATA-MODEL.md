# Data model

SQLite, WAL mode, one writer. Migrations are numbered SQL files in
`crates/mistarr-server/migrations/` applied at startup; this page shows the
schema after all of them. The exact current DDL, one generated file per
table and view, is in `crates/mistarr-server/schema/`; this page explains it.
A binary refuses to open a database whose `schema_version` records a
migration newer than any it embeds, and leaves its contents unchanged; see "Rollback" in [DEPLOYMENT.md](DEPLOYMENT.md). A
migration whose first line is `-- foreign_keys: off` runs with foreign keys
off and must leave `PRAGMA foreign_key_check` clean; see
`crates/mistarr-server/migrations/README.md` for the rebuild rule. All
timestamps are Unix seconds. All hashes are stored as BLOBs of their digest's
bytes (CRC32 4, MD5 16, SHA1 20), checked by `length()` in every table that
holds one, so a hex string written by mistarr's own code is refused; in memory
they are `mistarr_core` digests, which bind and read as that blob with core's
`rusqlite` feature, and the API and the views write them as lowercase hex.
`sources.infohash` stays hex text.

JSON may stage data or carry opaque blobs; nothing filters or joins on JSON.
The JSON columns are `dat_stage.game`, `scan_progress.done_dirs`,
`import_log.detail`, `jobs.payload`, `jobs.progress`, `sources.reason` and
`settings.value`; anything a query compares is a real column or a row of its
own table. A value in one of them that does not parse is an error naming the
column, never a default.

## Module layout

SQL lives in `crates/mistarr-server/src/db/`, one typed function per
statement. A table module owns the writes to its table: `files`,
`scan_progress`, `titles/` (`mod.rs` stores titles and their roms,
`recompute.rs` groups and picks them, `browse.rs` reads them, `detail.rs` reads
a group and holds the want and unwant writes to `titles`), `downloads` and so
on. A statement of one table's lifecycle may touch a neighbour: `dats::retire`
retires roms and titles, `chd` and `sources` update `files` and `roms` in step
with their own rows, and `candidates` updates `torrent_files`; downloads are
cancelled through `downloads`, `arcade::mra_version` writes `dat_versions`, and
`files::delete_ids` clears `import_log`.
Read-only composite views over several tables sit in `db/views/`; reads
that repeat a row shape go through the SQL views of "Views" below. Rom
lookups, the SHA1, MD5 then CRC32 and size tiers among them, are in `roms`;
`titles` and `arcade` write the rom rows.
A platform is named by a `PlatformId` in the `titles`, `arcade`, `roms`, `files`
and `platforms` signatures, read with `platforms::find`.

A statement run once per row of a loop or per file is prepared with
`prepare_cached`; a statement a request or a job runs once is prepared with
`prepare`, so the connection's statement cache holds only the hot ones.

Tests build catalogs with `db::fixtures`, compiled for tests and the
`test-support` feature only: `conn()` is a migrated database with the
platform table, `dat(&platform).title(name).rom(name, &hashes, status)` writes
a DAT version and returns the ids it wrote, and `catalog` writes the
standard `nes` roms. A migration test applies the schema of the version
before the one it checks with `migrate::apply_through` and seeds it with SQL
written for that version.

## Tables

```sql
CREATE TABLE platforms (
  id            INTEGER PRIMARY KEY,   -- fixed per slug, see "Platform ids"; never reused
  slug          TEXT NOT NULL UNIQUE,  -- stable slug, e.g. 'nes', 'megadrive', 'psx'
  name          TEXT NOT NULL,         -- display name
  core_dir      TEXT NOT NULL,         -- 'NES', 'Genesis', 'PSX'  (relative to games/)
  kind          INTEGER NOT NULL CHECK (kind BETWEEN 0 AND 3),   -- "Platform kinds"
  core_present  INTEGER NOT NULL DEFAULT 0 CHECK (core_present IN (0, 1)),
  enabled       INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1))
) STRICT;

CREATE TABLE dat_versions (
  id            INTEGER PRIMARY KEY,
  platform      INTEGER REFERENCES platforms(id),   -- NULL while unbound
  dat_name      TEXT NOT NULL,         -- <header><name>
  version       TEXT NOT NULL,         -- <header><version>
  source_file   TEXT NOT NULL,         -- basename as dropped
  loaded_at     INTEGER NOT NULL,
  superseded_by INTEGER REFERENCES dat_versions(id),
  game_count    INTEGER NOT NULL CHECK (game_count >= 0),
  retired       INTEGER NOT NULL DEFAULT 0 CHECK (retired IN (0, 1)),   -- set by DELETE /dats/{id}
  source        INTEGER NOT NULL DEFAULT 0 CHECK (source IN (0, 1)),    -- "Title sources": the one row MRA titles belong to
  family        TEXT NOT NULL,         -- dat::family_key of dat_name, refreshed at open
  UNIQUE (dat_name, version)
) STRICT;
CREATE INDEX dat_versions_family ON dat_versions(family, platform);
-- family: dat::family_key of dat_name, rewritten from the names at every start.

-- The writer connection's own, created on first use; never in the database file.
CREATE TEMP TABLE load_pending (  -- titles of the DAT being loaded not yet seen in it, cleared per load
  title_id INTEGER PRIMARY KEY
) STRICT;

CREATE TEMP TABLE dat_stage (     -- the DAT being imported, parsed outside the write lock and applied at once
  seq  INTEGER PRIMARY KEY,
  game TEXT NOT NULL              -- one parsed game with its roms, as JSON
) STRICT;

CREATE TABLE titles (                   -- one per <game>; the browse unit
  id            INTEGER PRIMARY KEY,
  platform      INTEGER NOT NULL REFERENCES platforms(id),
  dat_version_id INTEGER NOT NULL REFERENCES dat_versions(id),
  name          TEXT NOT NULL,         -- full DAT game name
  base_name     TEXT NOT NULL,         -- name with region/rev/lang tags stripped
  parent_id     INTEGER REFERENCES titles(id),   -- clone group root in its own DAT, self if parent
  revision      TEXT,
  is_1g1r_pick  INTEGER NOT NULL DEFAULT 0 CHECK (is_1g1r_pick IN (0, 1)),
  wanted        INTEGER NOT NULL DEFAULT 0 CHECK (wanted IN (0, 1)),
  retired       INTEGER NOT NULL DEFAULT 0 CHECK (retired IN (0, 1)),
  clone_of      TEXT,                  -- the DAT's cloneof, verbatim
  group_key     TEXT NOT NULL,         -- naming::group_key of the name
  inferred      INTEGER NOT NULL DEFAULT 0 CHECK (inferred IN (0, 1)),   -- parent chosen by group_key, not cloneof
  source        INTEGER NOT NULL DEFAULT 0 CHECK (source IN (0, 1)),     -- "Title sources": an MRA title is never retired by a DAT
  setname       TEXT,                  -- MRA <setname>
  rbf           TEXT,                  -- MRA <rbf>
  mra_path      TEXT,                  -- MRA file relative to _Arcade
  mra_check     INTEGER CHECK (mra_check BETWEEN 0 AND 3),   -- "MRA checks", NULL when not run
  mra_detail    TEXT,                  -- why the check did not match
  mra_stamp     TEXT,                  -- MRA and zip sizes and mtimes the check ran against
  mra_file_stamp TEXT,                 -- MRA parser version, file size and mtime when the title was last stored from it
  mra_seen      INTEGER,               -- the arcade catalogue run that last found the MRA file
  group_root    INTEGER REFERENCES titles(id),   -- effective clone group; see "Effective groups"
  UNIQUE (dat_version_id, name)
) STRICT;
CREATE INDEX titles_id_dat ON titles(id, dat_version_id);   -- a matched rom's DAT version, without the table row
CREATE INDEX titles_group_root ON titles(group_root);
CREATE TRIGGER titles_group_root_insert AFTER INSERT ON titles
BEGIN UPDATE titles SET group_root = NEW.parent_id WHERE id = NEW.id; END;
CREATE TRIGGER titles_group_root_parent AFTER UPDATE OF parent_id ON titles
BEGIN UPDATE titles SET group_root = NEW.parent_id WHERE id = NEW.id; END;
CREATE INDEX titles_group ON titles(platform, inferred, group_key);
CREATE INDEX titles_source ON titles(platform, source);
CREATE INDEX titles_mra_path ON titles(platform, mra_path) WHERE source = 1;

-- Flags, regions and languages as rows of one table. Known flags take ids 1 to 9 and known
-- regions 10 to 31 in bit order; any other text takes the next id.
CREATE TABLE tags (
  id            INTEGER PRIMARY KEY,
  kind          INTEGER NOT NULL CHECK (kind BETWEEN 0 AND 2),   -- flag 0, region 1, language 2
  name          TEXT NOT NULL,         -- verbatim, case kept
  bit           INTEGER CHECK (bit > 0),   -- the summary bit of a known flag or region
  UNIQUE (kind, name)
) STRICT;
CREATE INDEX tags_kind_nocase ON tags(kind, name COLLATE NOCASE);

-- A title's flags, regions and languages, in name order (`pos`).
CREATE TABLE title_flags (             -- bios, beta, proto, demo, sample, unl, ...
  title_id      INTEGER NOT NULL REFERENCES titles(id) ON DELETE CASCADE,
  pos           INTEGER NOT NULL CHECK (pos >= 0),
  tag_id        INTEGER NOT NULL REFERENCES tags(id),
  PRIMARY KEY (title_id, tag_id)
) STRICT, WITHOUT ROWID;
CREATE INDEX title_flags_tag ON title_flags(tag_id, title_id);
CREATE TABLE title_regions (           -- same columns, regions
  title_id      INTEGER NOT NULL REFERENCES titles(id) ON DELETE CASCADE,
  pos           INTEGER NOT NULL CHECK (pos >= 0),
  tag_id        INTEGER NOT NULL REFERENCES tags(id),
  PRIMARY KEY (title_id, tag_id)
) STRICT, WITHOUT ROWID;
CREATE INDEX title_regions_tag ON title_regions(tag_id, title_id);
CREATE TABLE title_languages (         -- same columns, languages
  title_id      INTEGER NOT NULL REFERENCES titles(id) ON DELETE CASCADE,
  pos           INTEGER NOT NULL CHECK (pos >= 0),
  tag_id        INTEGER NOT NULL REFERENCES tags(id),
  PRIMARY KEY (title_id, tag_id)
) STRICT, WITHOUT ROWID;
CREATE INDEX title_languages_tag ON title_languages(tag_id, title_id);

CREATE TABLE roms (                     -- one per <rom>; the file unit
  id            INTEGER PRIMARY KEY,
  title_id      INTEGER NOT NULL REFERENCES titles(id),
  own_name      INTEGER NOT NULL DEFAULT 1 CHECK (own_name IN (0, 1)),   -- 0: the full name is titles.name || name
  name          TEXT NOT NULL,         -- filename in the DAT (may include a subdir for discs), or its tail after the title's name
  size          INTEGER NOT NULL CHECK (size >= 0),
  crc32         BLOB CHECK (length(crc32) = 4),
  md5           BLOB CHECK (length(md5) = 16),
  sha1          BLOB CHECK (length(sha1) = 20),
  status        INTEGER NOT NULL DEFAULT 2 CHECK (status BETWEEN 0 AND 3),   -- "Rom statuses"
  retired       INTEGER NOT NULL DEFAULT 0 CHECK (retired IN (0, 1)),   -- no longer listed by the title's DAT entry
  match_name    TEXT,                  -- normalised leaf name for pre-download matching, filled by binding
  match_base    TEXT,                  -- base name of match_name
  header        TEXT,                  -- the DAT's header attribute, verbatim: hex bytes an adapter may prepend
  zip_dir       TEXT,                  -- MRA roms: directory under games/ holding the zip
  present       INTEGER NOT NULL DEFAULT 0 CHECK (present IN (0, 1)),   -- MRA roms: the zip was on disk at the last catalogue run or import
  CHECK (own_name = 1 OR name <> '')
) STRICT;
CREATE UNIQUE INDEX roms_title_name ON roms(title_id, own_name, name);
CREATE TRIGGER titles_name_fixed BEFORE UPDATE OF name ON titles
WHEN NEW.name IS NOT OLD.name BEGIN SELECT RAISE(ABORT, 'titles.name never changes'); END;
CREATE INDEX roms_sha1 ON roms(sha1);
CREATE INDEX roms_md5  ON roms(md5) WHERE sha1 IS NULL;
CREATE INDEX roms_crc  ON roms(crc32, size);
CREATE INDEX roms_match_name ON roms(match_name, title_id);
CREATE INDEX roms_match_base ON roms(match_base, size, title_id) WHERE match_base IS NOT NULL;
CREATE INDEX roms_id_title ON roms(id, title_id);   -- a matched file's title, without the table row
CREATE INDEX roms_size ON roms(size) WHERE match_base IS NOT NULL;
CREATE INDEX roms_chd_size ON roms(size) WHERE lower(name) LIKE '%.chd';   -- a scanned .chd hashed whole (the tail keeps the extension)
CREATE INDEX roms_track_size ON roms(size) WHERE size % 2352 = 0;        -- a CHD's rebuilt track sizes

CREATE TABLE files (                    -- what is on disk under games/
  id            INTEGER PRIMARY KEY,
  platform      INTEGER NOT NULL REFERENCES platforms(id),
  rel_path      TEXT NOT NULL,         -- relative to games/, e.g. 'NES/a.zip#b.nes' for a zip member, 'mame/a.zip' for an arcade presence row, 'PSX/G/g.chd#01' or 'PSX/G/g.chd#cue' for a CHD member
  size          INTEGER NOT NULL,
  mtime         INTEGER NOT NULL,
  crc32         BLOB CHECK (length(crc32) = 4),
  md5           BLOB CHECK (length(md5) = 16),
  sha1          BLOB CHECK (length(sha1) = 20),
  header_rule   INTEGER CHECK (header_rule BETWEEN 0 AND 6),   -- "Header rules": the rule it was hashed, or last failed to hash, under; NULL for a zip member known by its CRC32 alone
  rom_id        INTEGER REFERENCES roms(id),
  state         INTEGER NOT NULL CHECK (state BETWEEN 0 AND 5),   -- "File states", see "files.state"
  scanned_at    INTEGER NOT NULL,
  reason        INTEGER CHECK (reason BETWEEN 0 AND 16),   -- "Unidentified reasons": why an unidentified row is not identified; NULL otherwise
  crc32_whole   BLOB CHECK (length(crc32_whole) = 4),
  md5_whole     BLOB CHECK (length(md5_whole) = 16),
  sha1_whole    BLOB CHECK (length(sha1_whole) = 20),   -- rule ines, a78 or lnx: the whole file's hashes, header included; equal to crc32/md5/sha1 when no header was found; NULL under other rules
  UNIQUE (platform, rel_path),
  CHECK (reason IS NULL OR state = 5)
) STRICT;
CREATE INDEX files_rom ON files(rom_id);
CREATE INDEX files_state ON files(state, platform);
CREATE INDEX files_rel_lower ON files(platform, lower(rel_path));   -- arcade rows match zips ignoring case

CREATE TABLE scan_progress (            -- resumable per-platform library scan state
  platform      INTEGER PRIMARY KEY REFERENCES platforms(id),
  done_dirs     TEXT NOT NULL DEFAULT '[]',   -- json array of directories already committed
  updated_at    INTEGER NOT NULL
) STRICT;

CREATE TABLE sources (                  -- one per torrent the user dropped in
  id                 INTEGER PRIMARY KEY,
  infohash           TEXT NOT NULL UNIQUE,
  display_name       TEXT NOT NULL,    -- torrent name field
  origin_file        TEXT NOT NULL,    -- name of the file under sources/loaded/
  platform           INTEGER REFERENCES platforms(id),   -- NULL while unbound
  bind_score         REAL CHECK (bind_score BETWEEN 0 AND 1),   -- hit rate that produced the binding
  state              INTEGER NOT NULL CHECK (state BETWEEN 0 AND 3),   -- "Source states", see "sources.state"
  reason             TEXT,             -- json: why it is resolving or unbound, as a code the API words; see "sources.state"
  seed_policy        INTEGER NOT NULL DEFAULT 0 CHECK (seed_policy BETWEEN 0 AND 2),   -- "Seed policies"
  seed_ratio         REAL CHECK ((seed_policy = 1) = (seed_ratio IS NOT NULL)),   -- the N of ratio:N
  file_count         INTEGER NOT NULL DEFAULT 0 CHECK (file_count >= 0),
  total_size         INTEGER NOT NULL DEFAULT 0 CHECK (total_size >= 0),
  client_id          TEXT,             -- id in the download client once added, else NULL
  added_at           INTEGER NOT NULL,
  suggested_platform INTEGER REFERENCES platforms(id),   -- guessed from the torrent's names, no DAT needed
  user_binding       INTEGER NOT NULL DEFAULT 0 CHECK (user_binding IN (0, 1)),   -- 1 when the user chose the binding, a platform or none; automatic binding never changes it
  map_stamp          TEXT,             -- the platform's live DAT versions and roms the files were last mapped against
  bind_pending       INTEGER CHECK (bind_pending BETWEEN 0 AND 2),   -- "Pending bindings"
  bind_platform      INTEGER REFERENCES platforms(id),   -- the platform of a platform:<id> choice
  CHECK (seed_policy <> 1 OR seed_ratio > 0),
  CHECK ((bind_pending IS 2) = (bind_platform IS NOT NULL))
) STRICT;
CREATE INDEX sources_state ON sources(state);

CREATE TABLE torrent_dirs (             -- a directory each of a source's files shares
  id            INTEGER PRIMARY KEY,
  source_id     INTEGER NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
  path          TEXT NOT NULL,         -- 'dir/' with its slash, '' at the top level
  UNIQUE (source_id, path)
) STRICT;

CREATE TABLE torrent_files (
  source_id     INTEGER NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
  file_index    INTEGER NOT NULL,
  dir_id        INTEGER NOT NULL REFERENCES torrent_dirs(id),
  leaf          TEXT NOT NULL,         -- the text after the path's last '/'
  size          INTEGER NOT NULL,
  rom_id        INTEGER REFERENCES roms(id),   -- best name match, or the rom a hash proved; may be NULL
  confidence    INTEGER CHECK (confidence BETWEEN 0 AND 2),  -- "Match confidences", NULL when unmatched
  PRIMARY KEY (source_id, file_index)
) STRICT, WITHOUT ROWID;
CREATE INDEX torrent_files_rom ON torrent_files(rom_id);
CREATE INDEX torrent_files_source_rom ON torrent_files(source_id, rom_id);   -- a source's matched and unmatched counts

CREATE TABLE torrent_candidates (       -- further roms a file may be; one file, many roms
  source_id     INTEGER NOT NULL,
  file_index    INTEGER NOT NULL,
  rom_id        INTEGER NOT NULL REFERENCES roms(id),
  confidence    INTEGER NOT NULL CHECK (confidence BETWEEN 1 AND 4),   -- "Match confidences"
  PRIMARY KEY (source_id, file_index, rom_id),
  FOREIGN KEY (source_id, file_index)
    REFERENCES torrent_files(source_id, file_index) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;
CREATE INDEX torrent_candidates_rom ON torrent_candidates(rom_id);

CREATE TABLE downloads (
  id            INTEGER PRIMARY KEY,
  title_id      INTEGER NOT NULL REFERENCES titles(id),
  rom_id        INTEGER NOT NULL REFERENCES roms(id),
  source_id     INTEGER REFERENCES sources(id) ON DELETE SET NULL,  -- NULL while 'wanted' or once the source is deleted
  file_index    INTEGER,                         -- NULL while 'wanted'
  state         INTEGER NOT NULL CHECK (state BETWEEN 0 AND 8),   -- "Download states", see state machine
  progress      REAL NOT NULL DEFAULT 0 CHECK (progress BETWEEN 0 AND 1),   -- 0 to 1
  staged_path   TEXT,                  -- local path of the finished file, set on 'importing'
  error         TEXT,                  -- why it failed, shown to the user
  created_at    INTEGER NOT NULL,
  updated_at    INTEGER NOT NULL
) STRICT;
CREATE INDEX downloads_state ON downloads(state);
CREATE INDEX downloads_source ON downloads(source_id, file_index);
CREATE INDEX downloads_rom ON downloads(rom_id);
CREATE INDEX downloads_recent ON downloads(updated_at DESC, id DESC);

CREATE TABLE import_log (
  id            INTEGER PRIMARY KEY,
  at            INTEGER NOT NULL,
  download_id   INTEGER REFERENCES downloads(id),
  file_id       INTEGER REFERENCES files(id),
  action        INTEGER NOT NULL CHECK (action BETWEEN 0 AND 4),   -- "Import actions"
  detail        TEXT NOT NULL          -- json
) STRICT;
CREATE INDEX import_log_file ON import_log(file_id);

CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT, WITHOUT ROWID;
CREATE TABLE chd_tracks (                     -- rebuilt .bin hashes of a CHD's tracks, keyed by content
  chd_sha1 BLOB NOT NULL CHECK (length(chd_sha1) = 20),   -- header combined SHA1, verified by the decode
  chd_size INTEGER NOT NULL,
  track    INTEGER NOT NULL CHECK (track >= 1),           -- 1-based
  size     INTEGER NOT NULL,                              -- rebuilt track length
  crc32    BLOB NOT NULL CHECK (length(crc32) = 4),
  md5      BLOB NOT NULL CHECK (length(md5) = 16),
  sha1     BLOB NOT NULL CHECK (length(sha1) = 20),
  PRIMARY KEY (chd_sha1, chd_size, track)
) STRICT, WITHOUT ROWID;
CREATE INDEX chd_tracks_sha1 ON chd_tracks(sha1);   -- finds the image of member rows whose size is not stored

CREATE TABLE chd_failures (                   -- CHDs that cannot be identified, so they are not decoded again
  chd_sha1  BLOB NOT NULL CHECK (length(chd_sha1) = 20),
  chd_size  INTEGER NOT NULL,
  mtime     INTEGER NOT NULL,                 -- of the file that failed; a rewritten file is retried
  reason    INTEGER NOT NULL CHECK (reason BETWEEN 0 AND 16),   -- a files.reason code
  decoder   INTEGER NOT NULL,                 -- decoder version; an older one is retried
  failed_at INTEGER NOT NULL,
  PRIMARY KEY (chd_sha1, chd_size, mtime)
) STRICT, WITHOUT ROWID;

CREATE TABLE chd_whole (                      -- whole-file hashes of CHDs a DAT's `.chd` rom size had hashed
  chd_sha1 BLOB NOT NULL CHECK (length(chd_sha1) = 20),
  chd_size INTEGER NOT NULL,
  mtime    INTEGER NOT NULL,                  -- of the file hashed
  crc32    BLOB NOT NULL CHECK (length(crc32) = 4),
  md5      BLOB NOT NULL CHECK (length(md5) = 16),
  sha1     BLOB NOT NULL CHECK (length(sha1) = 20),
  PRIMARY KEY (chd_sha1, chd_size, mtime)
) STRICT, WITHOUT ROWID;
-- They outlive the files they describe, so a moved image is identified without a decode.

CREATE TABLE jobs (
  id            INTEGER PRIMARY KEY,
  kind          INTEGER NOT NULL CHECK (kind BETWEEN 0 AND 13),   -- "Job kinds"
  payload       TEXT NOT NULL,         -- json
  state         INTEGER NOT NULL CHECK (state BETWEEN 0 AND 4),   -- "Job states"
  progress      TEXT,                  -- json, job specific
  created_at    INTEGER NOT NULL,
  updated_at    INTEGER NOT NULL,
  lane          INTEGER NOT NULL DEFAULT 2 CHECK (lane BETWEEN 0 AND 3),  -- "Job lanes"
  subject       TEXT NOT NULL          -- dedupe key: payload fields as name 0x1F value, sorted, joined by 0x1E
) STRICT;
CREATE INDEX jobs_subject ON jobs(kind, subject, state);
CREATE INDEX jobs_state ON jobs(state, id);
```

`torrent_files.rom_id` holds one rom, but one file can be a candidate for
several: a short name such as `nova.nes` fits two versions of the same size.
Those further roms, from every mapping tier, live in `torrent_candidates`,
never repeating the pair `torrent_files` holds (VERIFICATION.md
"Pre-download matching"). A file is available for a rom when either table
pairs them, its source is `bound`, and no `bad` download of that rom used the
file; choosing a file for a download, the title's availability and a
source's `matched_count` read the union of both tables. Once an import
proves by hash which rom a file is, its `torrent_files` row names that rom
with confidence `hash` and its candidates are dropped; mapping never changes
such a row again.

## State machines

### downloads.state

```
wanted ──▶ queued ──▶ transferring ◀──▶ checking ──▶ importing ──▶ done
   │        ▲   │           │                │            │
   │        │   ▼           ▼                ▼            ▼
   │        └─ failed ◀─────┴────────────────┘           bad (hash mismatch, quarantined)
   ▼
cancelled   (from wanted, queued, transferring or checking)
```

- `wanted`: title marked, no torrent_file chosen yet (no bound source has it).
  `source_id` and `file_index` are NULL. Every transfer pass, and every
  `source.changed`, re-checks these rows and queues those that now have a file.
- `queued`: torrent_file chosen, not yet started in the client. A row stays
  here while no client is detected, the client does not answer, or a magnet's
  metadata is still pending.
- `transferring`: the client was told to fetch the file, or reports it below
  100 percent. A file the client reports complete and checked in one poll
  goes straight to `importing`.
- `checking`: client reports 100 percent, waiting for the client's own hash
  check to confirm. A failed check returns the row to `transferring`.
- `importing`: the client has the whole file checked; `staged_path` is set and
  the importer owns the row. Under seed policy `none` the poller stops a
  torrent once no download of its source is queued, transferring or checking
  and every file selected in the client is complete; the client never stops
  it on its own.
- `bad` also ends a cartridge download whose file hashed to another live,
  non-BIOS version in the wanted entry's clone group: the file is placed and
  verified as that version (or kept, when the library already holds it
  verified), the error reads "the file in this source is a different
  version: <name>", and the file stops being offered for the wanted rom.
- A `bad` row is the record that its file is not its rom. When it ends so
  after placing another version, or after quarantining a file picked from a
  `fuzzy` or `size` candidate, and the title is still wanted with no verified
  file and no open download of the rom, a new download of the rom opens:
  `queued` on the next best file, else `wanted`, carrying the same error so
  the history shows.
- `done`, `bad`, `failed`, `cancelled`: terminal. `failed` may be retried,
  which returns it to `queued` on the same torrent_file; `bad` never retries
  the same torrent_file. Cancelling a `transferring` or `checking` row
  deselects its file in the client and stops the torrent when nothing of it
  is selected any more. `importing` rows cannot be cancelled.
- One rom has at most one row outside the terminal states; wanting a title
  again skips roms that have one or that already have a verified file.

`staged_path` is `<paths.data>/staging/<infohash>/<name>/<path>` for a
multi-file torrent, where `<name>` is `sources.display_name` and `<path>` is
the file's path inside the torrent, and
`<paths.data>/staging/<infohash>/<path>` for a single-file one. It is the
path mistarr sees, after the remote path map.

### files.state

The column holds the code of "File states"; the words below are what the API,
the events and the views show.

- `pending`: seen, not yet hashed.
- `verified`: hash matches a rom and the filename is what the adapter expects
  (VERIFICATION.md "File names").
- `misnamed`: hash matches a rom, name differs. The UI offers rename. Only
  `verified` files count toward a title's `have`.
- `unverified`: no rom matches in any loaded DAT. A member of an imported MRA
  zip the md5 check did not read, or that no hash source covers, is
  `unverified` with `rom_id` set to the zip's rom, and so is the presence row
  `mame/<zip>` the arcade presence pass writes for a zip a live MRA names.
- `bad`: matches a rom flagged `baddump`.
- `unidentified`: a CHD image on a disc platform whose tracks are not known,
  with `reason` saying why (VERIFICATION.md "CHD images"). Its row is
  `g.chd` itself, with no hashes and no rom. Once its tracks are known the
  row gives way to member rows `g.chd#01`, `g.chd#02`… with the rebuilt
  tracks' sizes and hashes, and `g.chd#cue` (`#cue2`…) for each cue rom
  when every track of one DAT entry matches a distinct rom; members take the
  states above and are never `misnamed`.

### sources.state

- `resolving`: a magnet whose file list is not known yet: no client, not yet
  added, or the client is fetching metadata; `reason` says which.
- `unbound`: file list known, no platform reached the binding threshold.
- `bound`: attached to a platform, torrent_files populated.
- `disabled`: user turned it off; existing downloads finish, nothing new is
  chosen from it.

`reason` is a JSON object whose `code` says why, with the parameters the
sentence the API shows needs: `no_client`, `waiting_metadata`,
`bad_infohash` (the stored infohash cannot be read), `client_refused` (`error`, the client's message), `no_match` (`percent`, the
bind threshold, and `suggested`, the platform its names suggest when that
platform has a DAT), `awaiting_dat` (`platform`, suggested by its names, with
no DAT loaded) and `ignored` (the user marked it as not a game set).
Migration 0021 turns each sentence an earlier version stored into its code.

`user_binding` is the user's override of the classifier. `PUT /sources/{id}`
with a `platform_id`, or `null` for "not a game set", sets it; `binding:
"automatic"` clears it. While it is set, the rebind after a DAT load skips
the source, so no automatic run changes its platform; a `remap_sources` job
still maps its files again against its own platform when that platform's
roms change. Each request also stores the choice in `bind_pending` and queues
a `bind_source` job; the job takes the value when it runs and applies it, and
finds nothing when a later job already took it, so requests that share a
queued job, or run in any order, end on the last choice. Migration 0019 gives
sources the user had unbound the `ignored` reason.

## Titles across DAT versions

A title row belongs to the DAT name, not to one version. Loading a version
reuses the row with the same game name from any version of the same
`dat_name`, moving its `dat_version_id` forward, so `titles.id`, `wanted` and
the `files.rom_id` links survive an update. Titles left on older versions
after a load are the entries the new version dropped; they get `retired = 1`
and are never deleted, as do titles of the same version a reload of it no
longer lists. Roms a kept entry no longer lists get `roms.retired = 1`. A
kept rom the new version gives another size or other hashes loses its files'
links in the same transaction, outside arcade: a fully hashed file becomes
`unverified` with no rom, for the recompute to match again from its stored
hashes, and any other but a CHD's cue row `pending` (VERIFICATION.md
"Matching stored hashes").
A version whose string sorts below the newest live one of its name is stored
already superseded and does not touch titles. An unbound version stores only
its `dat_versions` row; binding it re-reads the file from `dats/loaded/`.

### MRA titles

Arcade titles read from MRA files have `source = 1` (mra) and all belong to
one `dat_versions` row with `source = 1` and `dat_name` `_Arcade`, which
`/dats`, the wizard and DAT loads never see. DAT supersession, reloads and
`DELETE /dats/{id}` only retire `source = 0` (dat) titles; an MRA title is
retired when a catalogue run no longer finds its MRA, and revived, with its
id and `wanted`, when the MRA returns. Each run takes the next `mra_seen`
number and stamps every title whose MRA it finds, so the titles it did not
stamp are the ones to retire; a live title whose `mra_file_stamp` still
matches its file is stamped without reading the MRA again. A run that
stores a title sets the `settings` key `mra.recompute_pending.arcade`, and
the run that recomputes the picks deletes it. Its roms are the zips it names, with
`size` 0, the MRA's `md5` or none, `zip_dir` and `present`. MRA titles are
`inferred` with a `group_key` prefixed `mra:`, so they never group with DAT
entries.

`parent_id` comes from `clone_of` when the DAT has any `cloneof`, else the
titles are `inferred` and every live inferred title of the platform is
regrouped by `(platform, group_key)` after each load, electing the parent
that wins 1G1R under default preferences.

### Effective groups

`titles.group_root` is the effective clone group, the one column every
grouping reads: browse, `title_groups`, group detail, want and unwant, 1G1R
and the platform counts. The triggers keep it equal to `parent_id` whenever
`parent_id` is written. `titles::recompute::recompute_platform` rebuilds it
from scratch for the platform: every title back to its `parent_id`, then each
title that another live DAT on the platform lists with the same roms linked to
that title's group (VERIFICATION.md "DAT families"). Only single titles link,
so `parent_id` always holds each DAT's own parent/clone data. When a group's
root links away, the members it leaves take their lowest live id as their
root, so a group's id is always a title whose `group_root` is itself. A
group's members are the titles whose `group_root` is its id.

## Derived tables

`title_groups` holds one row per effective clone group and platform, the browse
unit; its `parent_id` is the group's `titles.group_root`.
It is a table kept equal to its inputs, not a view, so browse and the
platform counts read one indexed row per group instead of aggregating every
title, rom and file of a platform per request.

```sql
CREATE TABLE title_groups (
  parent_id     INTEGER NOT NULL,
  platform      INTEGER NOT NULL,      -- a platform key; no foreign key, as a derived table
  base_name     TEXT NOT NULL,         -- the root title's; its name is read from the root title
  variants      INTEGER NOT NULL,
  have_verified INTEGER NOT NULL,
  wanted        INTEGER NOT NULL,
  has_pick      INTEGER NOT NULL,
  pick_id       INTEGER,
  newest_id     INTEGER NOT NULL,
  source        INTEGER NOT NULL,      -- the root title's, "Title sources"
  lean_flags    INTEGER NOT NULL,      -- least known-flag bits of a live variant
  unflagged_regions INTEGER NOT NULL,  -- region bits of the variants with no known flag
  flag_union    INTEGER NOT NULL,      -- flag bits of every live variant
  region_union  INTEGER NOT NULL,      -- region bits of every live variant
  split         INTEGER NOT NULL,      -- the root title is on another platform
  PRIMARY KEY (parent_id, platform)
) STRICT, WITHOUT ROWID;
CREATE INDEX title_groups_name ON title_groups(platform, base_name COLLATE NOCASE, parent_id);
CREATE INDEX title_groups_have ON title_groups(platform, (have_verified > 0) DESC, base_name COLLATE NOCASE, parent_id);
CREATE INDEX title_groups_recent ON title_groups(platform, newest_id DESC);
CREATE INDEX title_groups_split ON title_groups(platform, parent_id) WHERE split;
CREATE INDEX title_groups_counts ON title_groups(platform, lean_flags, source, have_verified, wanted);

CREATE TABLE title_groups_dirty (parent_id INTEGER PRIMARY KEY) STRICT;   -- groups a write changed

CREATE VIEW title_search_source AS      -- the platform slug between 0x1F sentinels
  SELECT t.id, t.base_name,
         char(31) || (SELECT p.slug FROM platforms p WHERE p.id = t.platform) || char(31) AS platform
  FROM titles t;
CREATE VIRTUAL TABLE title_search USING fts5(
  base_name, platform, content = 'title_search_source', content_rowid = 'id',
  tokenize = 'trigram');
```

The group columns keep the meaning they have always had. Roms and files are
aggregated per title first, so a title with several roms or several files per
rom counts once. `variants` counts live titles, `have_verified` the live titles whose every
live rom has a `verified` file, or for an MRA title is present with no failed
md5 check, `wanted` the wanted live titles, and
`newest_id` orders groups by when their newest entry first appeared.

The summary columns are bit sets: each known flag and each known region (the
`tags` rows with a `bit`) has a bit, and `1 << 62` stands for any other value. The
flags `prefs.hide` holds by default (bios, beta, proto, demo, sample,
program) take the highest known bits, so `lean_flags`, the smallest
known-flag value of any live variant, is free of them exactly when some
variant is. Browse visibility (hidden flags, a region, required flags) needs
a live variant that passes all three at once. With the default hide list
alone `lean_flags` decides; otherwise the bits prefilter and short-cut, and
the remaining groups check their variants' flag and region rows.

`crates/mistarr-server/src/db/groups.rs` holds the one query that computes a
group from its inputs, grouped by `titles.group_root`; every
refresh, rebuild and check uses it.

### Keeping it current

Triggers on `titles` (its `group_root` among the watched columns), `roms`,
`files`, `title_flags` and `title_regions` insert the affected group roots
into `title_groups_dirty` (a root is marked once per transaction).
`db::commit`, the only way a write transaction commits and the end of every
`Db::write_tx` and `db::transact`, recomputes the dirty groups and empties
the list before `COMMIT`, so readers never see a group out of step with its
titles. It refreshes 4096 roots per statement, so a DAT load that dirties a
whole platform keeps SQLite's temporary tables small. A write made outside a
transaction is settled by the writer connection right after, with a warning
in the log.

`title_search` indexes each title's `base_name` and its platform id, wrapped
in 0x1F so `nes` never matches inside `snes`, with trigrams; the title
triggers keep it in the same transaction, including a title that moves
platform. A search `MATCH`es the platform's phrase and the term together;
since a group's root title can sit on another platform, `split` marks
those rows and the search adds them from `title_groups_split`
(ARCHITECTURE.md "Resource budgets").

`mistarr doctor` compares the table and the search index with a fresh
computation and reports drift; `mistarr doctor --rebuild-groups` recomputes
both.

### Indexes

Every hot read (browse, counts, title detail, launch, want, best file, the
sources, downloads, jobs and import lists) seeks through an index. The
exceptions are inherent or bounded: the platform counts read every group
once, though only the columns `title_groups_counts` holds, MRA titles
through `titles_mra_path`, unfiltered totals count a whole table, and pages
in id order stop at their limit. The test
`db::plans::hot_reads_walk_indexes_not_growing_tables` prints each plan and
fails on any other scan.

The reads behind `GET /sources` and `GET /sources/{id}` walk indexes of their
own. `torrent_files_source_rom` holds a source's files in `rom_id` order, so
the matched count is its `rom_id IS NOT NULL` range, and each `EXISTS` probe
is a `torrent_candidates` primary-key seek. A file without a rom is one seek
of `torrent_files_rom`, whose key carries the table's primary key columns, so
the range stays the source's own unmatched rows. The DAT list then
follows `roms_id_title` to a matched rom's title and `titles_id_dat` to that
title's DAT version, so no `roms` or `titles` row is read. The test
`db::plans::source_detail_seeks_the_source_files_and_downloads` asserts each
seek, and `db::plans::platform_counts_read_the_covering_group_index` asserts
the covering group index.

A read that matches files by `rom_id` and one `state` names `files_rom` with
`INDEXED BY`: with an integer platform in `files_state`, the planner would
otherwise walk every file of that state for each rom.

Each index in this table serves one lookup and holds only the roms that
lookup can return; a DAT load writes into every one whose key its roms
carry, at random places (ARCHITECTURE.md "Writes on a sync mount"):

| Index | Lookup | Rows |
|---|---|---|
| `roms_sha1` | the sha1 tier of matching a file | every rom |
| `roms_md5` | the md5 tier, which matches only roms without a sha1 | roms without a sha1 |
| `roms_crc` | the CRC32 and size tier, and whether a zip member's CRC32 can match anything | every rom |
| `roms_match_name` | binding by normalised name, and the roms still to key | every rom |
| `roms_match_base` | binding by base name and size | roms binding has keyed |
| `roms_size` | the fuzzy and size-only candidates, which read only keyed roms | roms binding has keyed |
| `roms_chd_size` | whether a scanned `.chd` has the size of a DAT's `.chd` rom | roms named `*.chd` |
| `roms_track_size` | the titles a CHD's track sizes can match before it is decoded | roms of whole 2352-byte sectors |

`roms_id_title` and `titles_id_dat` are no lookup index: both are keyed by an
increasing id, so a load appends to them and a read follows a `roms` or
`titles` row by its key.

A load inserts its roms with `match_name` and `match_base` NULL; binding
keys them (`sources::refresh_match_keys`) in one transaction before it reads
them, and both keys carry `title_id`, so a lookup reads a matched rom's
title alongside its id.
The source preview binds a chunk's keys as one JSON list (`json_each`) and
seeks both match indexes from it in one statement grouped by platform
(`sources::platform_hits`), the same rule as binding's per-file lookups;
nothing derived from a torrent file is stored. The base-name expression is
the `match_base!` macro in `db/sources.rs`, which both statements share, and
both name `roms_match_base` with `INDEXED BY`, since repeated sizes would
otherwise draw the planner to `roms_size`.
`db::plans::rom_lookups_and_directory_tracks_seek_their_own_index` asserts
each lookup, the partial ones included, seeks its index, and that a disc
directory's tracks (`files::in_directory`) are one range of the
`(platform, rel_path)` key.


## Column forms

The schema above is the one the migrations build. Wave 13 moves every table to
the forms below; NORMALIZATION.md holds the measurements, the migration files
("Migration grouping"; each table below names its file's number and packages)
and each package's tasks.
Column names here are plain text, since most do not exist yet; the views
under "Views" exist and are named in code font.

Forms:

| Form | Meaning |
|---|---|
| kept | type and meaning unchanged |
| BLOB n | the digest's bytes, declared BLOB with CHECK (length(x) = n); STRICT has no BLOB(n) type |
| code | INTEGER from the value table named, decoded by a code enum in Rust and by CASE in the views |
| 0/1 | INTEGER NOT NULL with CHECK (x IN (0, 1)) |
| key | INTEGER referencing another table's id: platform keys reference platforms(id) |
| tail | roms.name after its title's name when own_name is 0 |
| dropped | the column goes; where its value went is in the row |

Every table is STRICT. A JSON column stays TEXT. A computed key that is
always written (titles.group_key, jobs.subject, dat_versions.family) stays
NOT NULL and loses its `''` default, since `''` is a value it can take, not an
absent one. `schema_version` stays as `migrate.rs` creates it, before any
migration runs; the `title_search` virtual table cannot be STRICT and keeps its
columns.

### Tables

**platforms**: STRICT, rowid; 0024, WP-92.

| Column | Form | CHECK |
|---|---|---|
| id | key: INTEGER PRIMARY KEY, fixed per slug ("Platform ids") | |
| slug | new: TEXT NOT NULL UNIQUE, the 0022 id | |
| name, core_dir | kept TEXT NOT NULL | |
| kind | code, "Platform kinds" | BETWEEN 0 AND 3 |
| core_present, enabled | 0/1 | IN (0, 1) |

**dat_versions**: STRICT, rowid; 0026, WP-92 and WP-93.

| Column | Form | CHECK |
|---|---|---|
| id, dat_name, version, source_file, loaded_at, superseded_by | kept | |
| platform_id | becomes platform: key, NULL while unbound | |
| game_count | kept | >= 0 |
| retired | 0/1 | IN (0, 1) |
| source | code, "Title sources" | IN (0, 1) |
| family | kept TEXT NOT NULL, no default | |

Index dat_versions_family on (family, platform).

**titles**: STRICT, rowid; 0026, WP-89, WP-92 and WP-93.

| Column | Form | CHECK |
|---|---|---|
| id, dat_version_id, base_name, parent_id, group_root | kept | |
| platform_id | becomes platform: key, NOT NULL | |
| name | kept; the trigger titles_name_fixed refuses a change (P3) | |
| revision, clone_of, setname, rbf, mra_path, mra_detail, mra_stamp, mra_file_stamp | kept TEXT, NULL when absent | |
| is_1g1r_pick, wanted, retired, inferred | 0/1 | IN (0, 1) |
| group_key | kept TEXT NOT NULL, no default | |
| source | code, "Title sources" | IN (0, 1) |
| mra_check | code, "MRA checks", NULL when not run | BETWEEN 0 AND 3 |
| mra_seen | kept | |

Indexes: UNIQUE (dat_version_id, name), `titles_id_dat` and `titles_group_root`
kept; `titles_group` on (platform, inferred, group_key); `titles_source` on
(platform, source); `titles_mra_path` on (platform, mra_path) WHERE source = 1;
`titles_parent` and `titles_platform_base` dropped (P6).

**title_flags, title_regions, title_languages**: STRICT, WITHOUT ROWID; 0027, WP-90.

| Column | Form | CHECK |
|---|---|---|
| title_id | kept | |
| pos | kept | >= 0 |
| flag, region, language | become tag_id: key to tags(id) | |

Primary key (title_id, tag_id); index (tag_id, title_id).

**tags** (new): STRICT, rowid; 0027, WP-90.

| Column | Form | CHECK |
|---|---|---|
| id | INTEGER PRIMARY KEY, fixed for known tags ("Tag ids"), the next free id otherwise | |
| kind | code, "Tag kinds" | BETWEEN 0 AND 2 |
| name | TEXT NOT NULL, verbatim | |
| bit | INTEGER, the summary bit of a known flag or region, NULL otherwise; a spelling of a known region that differs in case carries the same bit | bit > 0 |

UNIQUE (kind, name), and an index on (kind, name COLLATE NOCASE) for the browse
region filter, which may match several spellings. `known_flags` and
`known_regions` are dropped: their rows are the tags with a bit.

**roms**: STRICT, rowid; 0024, WP-86 and WP-87.

| Column | Form | CHECK |
|---|---|---|
| id, title_id, match_name, header, zip_dir | kept | |
| own_name | new 0/1: 0 when name is a tail, which holds only when the full name starts with the title's name and is longer; the default 1 means a whole name, and every writer splits through `rom_name_parts` | IN (0, 1) |
| name | tail when own_name is 0, else the full name | own_name = 1 OR name <> '' |
| size | kept | >= 0 |
| crc32, md5, sha1 | BLOB 4, 16, 20; NULL when the DAT gives none | length 4, 16, 20 |
| status | code, "Rom statuses", default 2 | BETWEEN 0 AND 3 |
| match_base | per the P2 outcome: kept, or dropped for an expression index over match_name | |
| retired, present | 0/1 | IN (0, 1) |

Indexes: UNIQUE (title_id, name) becomes roms_title_name UNIQUE (title_id,
own_name, name); `roms_sha1`, `roms_md5`, `roms_crc` keep their columns over
BLOBs; `roms_match_base` and `roms_size` follow the P2 outcome; the others are
kept, the `.chd` predicate still on the stored name, which keeps the extension.
The `.chd` and `.cue` predicates on the stored name assume a title's name never
ends inside a rom's extension.

**files**: STRICT, rowid; 0024, WP-86, WP-92 and WP-93.

| Column | Form | CHECK |
|---|---|---|
| id, rel_path, size, mtime, rom_id, scanned_at | kept | |
| platform_id | becomes platform: key, NOT NULL | |
| crc32, md5, sha1, crc32_whole, md5_whole, sha1_whole | BLOB 4, 16, 20 | length 4, 16, 20 |
| header_rule | code, "Header rules", NULL as in 0022 | BETWEEN 0 AND 6 |
| state | code, "File states" | BETWEEN 0 AND 5 |
| reason | code, "Unidentified reasons" | BETWEEN 0 AND 16, and reason IS NULL OR state = 5 |

Indexes keep their columns, with platform for platform_id.

**sources**: STRICT, rowid; 0025, WP-92 and WP-93.

| Column | Form | CHECK |
|---|---|---|
| id, display_name, origin_file, client_id, added_at, map_stamp | kept | |
| infohash | kept TEXT NOT NULL UNIQUE: not bound through a digest, and a value that does not parse is the `bad_infohash` reason | |
| platform_id, suggested_platform_id | become platform, suggested_platform: keys, NULL when none | |
| bind_score | kept REAL | BETWEEN 0 AND 1 |
| state | code, "Source states" | BETWEEN 0 AND 3 |
| reason | kept JSON; platform slugs inside stay slugs | |
| seed_policy | code, "Seed policies", default 0 | BETWEEN 0 AND 2 |
| seed_ratio | new REAL: the N of `ratio:N` | (seed_policy = 1) = (seed_ratio IS NOT NULL), seed_ratio > 0 |
| file_count, total_size | kept | >= 0 |
| user_binding | 0/1 | IN (0, 1) |
| bind_pending | code, "Pending bindings", NULL when none | BETWEEN 0 AND 2 |
| bind_platform | new key: the platform of `platform:<id>` | (bind_pending IS 2) = (bind_platform IS NOT NULL) |

**torrent_dirs** (new): STRICT, rowid; 0025, WP-88. Columns id, source_id
(cascading from sources), path (`dir/` with its slash, `''` at the top), with
UNIQUE (source_id, path).

**torrent_files**: STRICT, WITHOUT ROWID; 0025, WP-88.

| Column | Form | CHECK |
|---|---|---|
| source_id, file_index, size, rom_id | kept; primary key (source_id, file_index) | |
| path | dropped for dir_id (key to torrent_dirs) and leaf (TEXT); the path is the dir's path then the leaf | |
| confidence | code, "Match confidences", NULL when unmatched | BETWEEN 0 AND 2 |

**torrent_candidates**: STRICT, WITHOUT ROWID; 0025, WP-88. confidence is a
code, "Match confidences", CHECK BETWEEN 1 AND 4; the other columns are kept.

**downloads**: STRICT, rowid; 0025, WP-93. state is a code, "Download states",
CHECK BETWEEN 0 AND 8; progress stays REAL with CHECK BETWEEN 0 AND 1; the other
columns are kept.

**import_log**: STRICT, rowid; 0028, WP-93. action is a code, "Import actions",
CHECK BETWEEN 0 AND 4; the other columns are kept.

**settings**: STRICT, WITHOUT ROWID; 0028, WP-93. Columns kept.

**chd_tracks, chd_whole**: STRICT, WITHOUT ROWID; 0024, WP-86. chd_sha1 and
sha1 are BLOB 20, md5 BLOB 16, crc32 BLOB 4; chd_tracks.track has CHECK
(track >= 1); the other columns are kept.

**chd_failures**: STRICT, WITHOUT ROWID; 0024, WP-86 and WP-93. chd_sha1 is
BLOB 20; reason is a code, "Unidentified reasons", CHECK BETWEEN 0 AND 16.

**jobs**: STRICT, rowid; 0028, WP-93.

| Column | Form | CHECK |
|---|---|---|
| id, payload, progress, created_at, updated_at | kept | |
| kind | code, "Job kinds" | BETWEEN 0 AND 13 |
| state | code, "Job states" | BETWEEN 0 AND 4 |
| lane | code, "Job lanes", default 2 | BETWEEN 0 AND 3 |
| subject | kept TEXT NOT NULL, no default | |

**scan_progress**: STRICT, rowid; 0024, WP-92. platform_id becomes platform,
the INTEGER PRIMARY KEY referencing platforms(id); done_dirs keeps its `'[]'`
default, an empty JSON list.

**title_groups**: STRICT, WITHOUT ROWID; 0026, WP-89, WP-92 and WP-93.

| Column | Form | CHECK |
|---|---|---|
| parent_id, base_name, pick_id, newest_id, lean_flags, unflagged_regions, flag_union, region_union | kept | |
| platform_id | becomes platform: INTEGER, no foreign key, as a derived table | |
| name | dropped (P6): browse reads the root title's | |
| variants, have_verified, wanted | kept counts | >= 0 |
| has_pick, split | 0/1 | IN (0, 1) |
| source | code, "Title sources" | IN (0, 1) |

**title_groups_dirty**: STRICT; 0026, WP-89. Column kept.

**title_search**: the fts5 table keeps the slug between 0x1F sentinels, so its
content does not change; `title_search_source` and the title triggers read the
slug from platforms.

**dat_stage**: the TEMP table becomes STRICT where `dat_stage.rs` creates it; WP-93.

### P2 outcomes

WP-82's board bench gives the base tier's share of the source preview. The
expression index reads a roms row per hit, measured up to 27% slower per
lookup on x86, so it slows the preview by at most 5% when that share is under
18%.

| Share | roms.match_base | `roms_match_base` | `roms_size` |
|---|---|---|---|
| under 18% | dropped | ((the base expression over match_name), size, title_id) WHERE match_name IS NOT NULL | (size) WHERE match_name IS NOT NULL |
| 18% or more | kept TEXT, NULL until keyed | (match_base, size, title_id) WHERE match_base IS NOT NULL | (size) WHERE match_base IS NOT NULL |

The base expression is match_name cut before its first `(` and right-trimmed.
Either way the base-and-size read orders by `+r.id` (P0).

The board bench gives 20 to 25% for every large source, so roms.match_base
stays: the second row.

### Value tables

A code keeps its meaning for good: a new variant takes the next number and a
retired number is never reused. Each table lists the Rust enum's text in
code order.

| Table | Enum | Codes |
|---|---|---|
| Rom statuses | `RomStatus` | good 0, baddump 1, nodump 2, verified 3 |
| File states | `FileState` | pending 0, verified 1, misnamed 2, unverified 3, bad 4, unidentified 5 |
| Header rules | `StoredRule` (`crates/mistarr-server/src/db/files.rs`): the names of `HeaderRule`, plus chd for CHD members | none 0, ines 1, smc 2, a78 3, lnx 4, n64 5, chd 6 |
| Unidentified reasons | `Unidentified`, then `Unidentifiable` | off 0, pending 1, no_layout 2, unreadable 3, not_chd 4, version 5, parent 6, not_cd 7, too_large 8, codec 9, gdrom 10, old_layout 11, cooked 12, pregap 13, corrupt 14, checksum 15, no_checksum 16 |
| Match confidences | `MatchConfidence` | hash 0, name 1, base 2, fuzzy 3, size 4 |
| Title sources | `TitleSource` | dat 0, mra 1 |
| MRA checks | the arcade job's `Check` | match 0, refused 1, missing_part 2, mismatch 3; a failing check is 2 or more |
| Platform kinds | `Kind` of the platform table | cartridge 0, disc 1, romset 2, arcade 3 |
| Source states | `SourceState` | resolving 0, unbound 1, bound 2, disabled 3 |
| Seed policies | `SeedPolicy` | none 0, ratio 1, client 2 |
| Pending bindings | `BindChoice` | automatic 0, none 1, platform 2 |
| Download states | `DownloadState` | wanted 0, queued 1, transferring 2, checking 3, importing 4, done 5, bad 6, failed 7, cancelled 8 |
| Import actions | `ImportAction` | placed 0, replaced 1, quarantined 2, skipped_existing 3, renamed 4 |
| Job kinds | `JobKind` | scan 0, import 1, detect_client 2, dat_import 3, recompute_1g1r 4, source_import 5, resolve_magnet 6, transfer 7, deselect 8, arcade_catalog 9, chd_tracks 10, remap_sources 11, bind_source 12, url_fetch 13 |
| Job states | `JobState` | queued 0, running 1, paused 2, done 3, failed 4 |
| Job lanes | `Lane` | heavy 0, background 1, light 2, fetch 3 |
| Tag kinds | | flag 0, region 1, language 2 |

**Platform ids**, in the order of the binary's platform table; a platform added
later takes the next id:

| Ids | Slugs |
|---|---|
| 1 to 11 | nes, fds, snes, n64, gb, gbc, gba, megadrive, s32x, sms, gg |
| 12 to 22 | sg1000, pce, sgx, atari2600, atari5200, atari7800, lynx, coleco, intv, ws, wsc |
| 23 to 33 | ngp, vectrex, pokemini, sv, psx, saturn, megacd, pcecd, neocd, neogeo, arcade |

A slug in the database that the binary's table does not list keeps its row
with an id from 1000 up, in slug order. A file or scan row that names a slug
with no platform row gets one, so every key resolves. `FIXED_KEYS` in
`db/platforms.rs` holds the ids in Rust, a unit test holds it equal to the
binary's table, and the startup sync inserts a missing platform with its id;
a platform already stored with an id from 1000 up keeps that id when a later
binary lists its slug, since rows elsewhere hold it.
The API, events and `titles` keep the slug (`PlatformId`); a `db` function over
`files` or `scan_progress` takes a `PlatformKey`, which the caller resolves once
with `db::platforms::key` or `PlatformRef`.

**Tag ids**: the known flags take 1 to 9 in bit order (unl, pirate, baddump,
bios, beta, proto, demo, sample, program) and the known regions 10 to 31 in bit
order (World first, Latin America last), so bios is tag 4.

### Views

Each view is a projection: no aggregate, DISTINCT, LIMIT, window
function or compound select, so SQLite flattens it into the query that reads
it. A decode that needs another table's row (a platform slug, a directory
path) is a scalar subquery, not a join, so a view of one table stays one table
and may sit on the right of a LEFT JOIN. Migration `0023_shared_views.sql`
creates them over the columns of the time and `0024_hashes_and_platform_keys.sql`
creates `rom_view`, `live_rom_view` and `file_view` again over the BLOB hashes and
the codes, and `0027_tags.sql` creates the first two again with the bios probe on tag 4;
they store no rows, so the synthetic catalogue's database size is
unchanged by them. `rom_view` and `file_view` read as below, and
the other views read as in the 0023 file and their later files:

```sql
CREATE VIEW rom_view AS
SELECT r.id, r.title_id,
       CASE WHEN r.own_name THEN r.name ELSE t.name || r.name END AS name,
       r.size, r.crc32, r.md5, r.sha1,
       NULLIF(lower(hex(r.crc32)), '') AS crc32_hex, NULLIF(lower(hex(r.md5)), '') AS md5_hex,
       NULLIF(lower(hex(r.sha1)), '') AS sha1_hex,
       r.status,
       CASE r.status WHEN 0 THEN 'good' WHEN 1 THEN 'baddump' WHEN 2 THEN 'nodump'
                     WHEN 3 THEN 'verified' END AS status_text,
       r.match_name, r.match_base, r.retired,
       r.header, r.zip_dir, r.present,
       t.name AS title_name, t.dat_version_id, t.source AS title_source,
       t.platform AS platform_key,
       (SELECT p.slug FROM platforms p WHERE p.id = t.platform) AS platform,
       t.group_root, t.parent_id,
       t.retired AS title_retired,
       (r.retired = 0 AND t.retired = 0) AS live,
       EXISTS (SELECT 1 FROM title_flags f WHERE f.title_id = r.title_id AND f.tag_id = 4) AS bios
FROM roms r JOIN titles t ON t.id = r.title_id;

CREATE VIEW live_rom_view AS
SELECT * FROM rom_view WHERE live AND NOT bios;

CREATE VIEW title_view AS
SELECT t.id, t.platform AS platform_key,
       (SELECT p.slug FROM platforms p WHERE p.id = t.platform) AS platform, t.dat_version_id,
       t.name, t.base_name, t.parent_id, t.group_root, t.revision, t.is_1g1r_pick, t.wanted,
       t.retired, t.clone_of, t.group_key, t.inferred, t.source,
       CASE t.source WHEN 0 THEN 'dat' WHEN 1 THEN 'mra' END AS source_text,
       t.setname, t.rbf, t.mra_path, t.mra_check,
       CASE t.mra_check WHEN 0 THEN 'match' WHEN 1 THEN 'refused' WHEN 2 THEN 'missing_part'
                        WHEN 3 THEN 'mismatch' END AS mra_check_text,
       t.mra_detail, t.mra_stamp, t.mra_file_stamp, t.mra_seen
FROM titles t;

CREATE VIEW torrent_file_view AS
SELECT f.source_id, f.file_index,
       (SELECT d.path FROM torrent_dirs d WHERE d.id = f.dir_id) || f.leaf AS path,
       f.size, f.rom_id, f.confidence,
       CASE f.confidence WHEN 0 THEN 'hash' WHEN 1 THEN 'name' WHEN 2 THEN 'base' END
         AS confidence_text
FROM torrent_files f;

CREATE VIEW file_view AS
SELECT f.id, f.platform AS platform_key,
       (SELECT p.slug FROM platforms p WHERE p.id = f.platform) AS platform, f.rel_path, f.size,
       f.mtime, f.crc32, f.md5, f.sha1,
       NULLIF(lower(hex(f.crc32)), '') AS crc32_hex, NULLIF(lower(hex(f.md5)), '') AS md5_hex,
       NULLIF(lower(hex(f.sha1)), '') AS sha1_hex,
       f.header_rule,
       CASE f.header_rule WHEN 0 THEN 'none' WHEN 1 THEN 'ines' WHEN 2 THEN 'smc'
                          WHEN 3 THEN 'a78' WHEN 4 THEN 'lnx' WHEN 5 THEN 'n64'
                          WHEN 6 THEN 'chd' END AS header_rule_text,
       f.rom_id, f.state,
       CASE f.state WHEN 0 THEN 'pending' WHEN 1 THEN 'verified' WHEN 2 THEN 'misnamed'
                    WHEN 3 THEN 'unverified' WHEN 4 THEN 'bad' WHEN 5 THEN 'unidentified'
                    END AS state_text,
       f.scanned_at, f.reason,
       CASE f.reason WHEN 0 THEN 'off' WHEN 1 THEN 'pending' WHEN 2 THEN 'no_layout'
                     WHEN 3 THEN 'unreadable' WHEN 4 THEN 'not_chd' WHEN 5 THEN 'version'
                     WHEN 6 THEN 'parent' WHEN 7 THEN 'not_cd' WHEN 8 THEN 'too_large'
                     WHEN 9 THEN 'codec' WHEN 10 THEN 'gdrom' WHEN 11 THEN 'old_layout'
                     WHEN 12 THEN 'cooked' WHEN 13 THEN 'pregap' WHEN 14 THEN 'corrupt'
                     WHEN 15 THEN 'checksum' WHEN 16 THEN 'no_checksum' END AS reason_text
FROM files f;
```

Each body drops its view first, so the migration can run again over a schema
stepped back. `file_view` leaves out the whole-file hashes: no read takes them
through it, and a view naming a column makes `ALTER TABLE … DROP COLUMN` fail,
which the migration test of `files.crc32_whole` relies on. For the same reason a
migration that rebuilds, renames or drops a column of a table a view names drops
those views first and creates them again after.

Each conversion changes only the bodies, keeping the column names:

| Column | Body after its conversion |
|---|---|
| rom_view.name | CASE WHEN r.own_name THEN r.name ELSE t.name \|\| r.name END |
| the hex columns | NULLIF(lower(hex(x)), '') of the stored column, since `hex(NULL)` is `''` and an absent hash stays NULL |
| status_text, source_text, mra_check_text, state_text, header_rule_text, reason_text | CASE over the value table |
| platform_key | the platform column, so a read filters by key and seeks the `titles` indexes |
| platform | (SELECT p.slug FROM platforms p WHERE p.id = t.platform) |
| rom_view.match_base | the base expression, when P2 drops the column |
| rom_view.bios | EXISTS on `title_flags` with f.tag_id = 4 |

Rules for reads through them:
- A lookup by hash or by code compares the stored column (sha1, status), never
  hex or text column of a view, which no index serves.
- `rom_view` is a join, so it never sits on the right of a LEFT JOIN: SQLite then
  materializes the whole view. Such a read joins roms and titles and spells the
  full-name expression.
- `INDEXED BY` cannot name a view, and it makes a lost index an error rather than
  a planner choice. A read that needs it stays on the tables: the source DAT
  list's covering `roms_id_title` and `titles_id_dat`, binding's base-and-size
  lookup and the preview's base tier on `roms_match_base`, the CHD size and
  track-layout lookups, and the case-insensitive zip rows.
- Writes go to the tables.
- A read moves onto a view only when its set of predicates stays identical.
  `live_rom_view` (live rom, live title, no bios flag) serves the reads with exactly
  that set, such as the size candidates. Binding tests only the title's retired
  flag and the bios flag, so it keeps binding retired roms of live titles. Its
  lookup by name and the preview's statement stay on the tables with the base
  tier, so binding's predicate has one definition in `db/sources.rs`.
- A read that orders by a column a view does not carry stays on the tables: the
  shared-title signatures order by `titles.id`, which `rom_view` exposes only as
  `roms.title_id`, and sort without the titles index.
- A read whose statement text a plan test selects stays on the tables: the
  source files page and its path search, and the directory tracks of
  `files::in_directory`. Reads of a whole file row stay too, as `file_view` has
  no whole-file hashes.

The plan test in `crates/mistarr-server/src/db/plans/shared_views.rs` runs each
read through a view beside the same read written out over the tables. It
compares the SEARCH, SCAN and temp B-tree steps as a multiset of index and
constraint, with aliases, LEFT-JOIN markers and subquery numbers left out, so a
scalar-subquery seek equals the join seek it replaces; it fails on a step only
one side has and on any MATERIALIZE or CO-ROUTINE step.

### Measured sizes

`mistarr bench-seed` writes the synthetic catalogue (42 000 titles, 82 037
roms, 13 263 files) into a new file:

| Database | `roms` table | `roms_sha1` | `roms_title_name` |
|---|---|---|---|
| 34 820 096 bytes | 6.1 MiB | 2.4 MiB | 2.0 MiB |

79 537 of the 82 037 roms store only the text after their title's name.
