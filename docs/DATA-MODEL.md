# Data model

SQLite, WAL mode, one writer. Migrations are numbered SQL files in
`crates/mistarr-server/migrations/` applied at startup; this page shows the
schema after all of them. All timestamps are
Unix seconds. All hashes are stored as lowercase hex text so they can be
compared with DAT values without conversion.

## Tables

```sql
CREATE TABLE platforms (
  id            TEXT PRIMARY KEY,      -- stable slug, e.g. 'nes', 'megadrive', 'psx'
  name          TEXT NOT NULL,         -- display name
  core_dir      TEXT NOT NULL,         -- 'NES', 'Genesis', 'PSX'  (relative to games/)
  kind          TEXT NOT NULL,         -- 'cartridge' | 'disc' | 'romset' | 'arcade'
  core_present  INTEGER NOT NULL DEFAULT 0,
  enabled       INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE dat_versions (
  id            INTEGER PRIMARY KEY,
  platform_id   TEXT REFERENCES platforms(id),   -- NULL while unbound
  dat_name      TEXT NOT NULL,         -- <header><name>
  version       TEXT NOT NULL,         -- <header><version>
  source_file   TEXT NOT NULL,         -- basename as dropped
  loaded_at     INTEGER NOT NULL,
  superseded_by INTEGER REFERENCES dat_versions(id),
  game_count    INTEGER NOT NULL,
  retired       INTEGER NOT NULL DEFAULT 0,   -- set by DELETE /dats/{id}
  source        TEXT NOT NULL DEFAULT 'dat',  -- 'dat' | 'mra': the one row MRA titles belong to
  UNIQUE (dat_name, version)
);

CREATE TABLE dat_stage (          -- the DAT being imported, parsed outside the write lock and applied at once
  seq  INTEGER PRIMARY KEY,
  game TEXT NOT NULL              -- one parsed game with its roms, as JSON
);

CREATE TABLE titles (                   -- one per <game>; the browse unit
  id            INTEGER PRIMARY KEY,
  platform_id   TEXT NOT NULL REFERENCES platforms(id),
  dat_version_id INTEGER NOT NULL REFERENCES dat_versions(id),
  name          TEXT NOT NULL,         -- full DAT game name
  base_name     TEXT NOT NULL,         -- name with region/rev/lang tags stripped
  parent_id     INTEGER REFERENCES titles(id),   -- clone group root, self if parent
  regions       TEXT NOT NULL,         -- json array
  languages     TEXT NOT NULL,         -- json array
  revision      TEXT,
  flags         TEXT NOT NULL,         -- json array: bios, beta, proto, demo, sample, unl, ...
  is_1g1r_pick  INTEGER NOT NULL DEFAULT 0,
  wanted        INTEGER NOT NULL DEFAULT 0,
  retired       INTEGER NOT NULL DEFAULT 0,
  clone_of      TEXT,                  -- the DAT's cloneof, verbatim
  group_key     TEXT NOT NULL DEFAULT '',   -- naming::group_key of the name
  inferred      INTEGER NOT NULL DEFAULT 0, -- parent chosen by group_key, not cloneof
  source        TEXT NOT NULL DEFAULT 'dat', -- 'dat' | 'mra': an MRA title is never retired by a DAT
  setname       TEXT,                  -- MRA <setname>
  rbf           TEXT,                  -- MRA <rbf>
  mra_path      TEXT,                  -- MRA file relative to _Arcade
  mra_check     TEXT,                  -- md5 check: 'match' | 'mismatch' | 'missing_part' | 'refused', NULL when not run
  mra_detail    TEXT,                  -- why the check did not match
  mra_stamp     TEXT,                  -- MRA and zip sizes and mtimes the check ran against
  mra_file_stamp TEXT,                 -- MRA parser version, file size and mtime when the title was last stored from it
  mra_seen      INTEGER,               -- the arcade catalogue run that last found the MRA file
  UNIQUE (dat_version_id, name)
);
CREATE INDEX titles_platform_base ON titles(platform_id, base_name);
CREATE INDEX titles_parent ON titles(parent_id);
CREATE INDEX titles_group ON titles(platform_id, inferred, group_key);
CREATE INDEX titles_source ON titles(platform_id, source);
CREATE INDEX titles_mra_path ON titles(platform_id, mra_path) WHERE source = 'mra';

CREATE TABLE roms (                     -- one per <rom>; the file unit
  id            INTEGER PRIMARY KEY,
  title_id      INTEGER NOT NULL REFERENCES titles(id),
  name          TEXT NOT NULL,         -- filename in the DAT, may include a subdir for discs
  size          INTEGER NOT NULL,
  crc32         TEXT, md5 TEXT, sha1 TEXT,
  status        TEXT NOT NULL DEFAULT 'nodump',   -- 'good' | 'baddump' | 'nodump' | 'verified'
  match_name    TEXT,                  -- normalised leaf name for pre-download matching, filled by binding
  match_base    TEXT,                  -- base name of match_name
  retired       INTEGER NOT NULL DEFAULT 0,       -- no longer listed by the title's DAT entry
  header        TEXT,                  -- the DAT's header attribute, verbatim: hex bytes an adapter may prepend
  zip_dir       TEXT,                  -- MRA roms: directory under games/ holding the zip
  present       INTEGER NOT NULL DEFAULT 0,       -- MRA roms: the zip was on disk at the last catalogue run or import
  UNIQUE (title_id, name)
);
CREATE INDEX roms_sha1 ON roms(sha1);
CREATE INDEX roms_md5  ON roms(md5);
CREATE INDEX roms_crc  ON roms(crc32, size);
CREATE INDEX roms_match_name ON roms(match_name);
CREATE INDEX roms_match_base ON roms(match_base, size);

CREATE TABLE files (                    -- what is on disk under games/
  id            INTEGER PRIMARY KEY,
  platform_id   TEXT NOT NULL REFERENCES platforms(id),
  rel_path      TEXT NOT NULL,         -- relative to games/, e.g. 'NES/a.zip#b.nes' for a zip member, 'mame/a.zip' for an arcade presence row
  size          INTEGER NOT NULL,
  mtime         INTEGER NOT NULL,
  crc32 TEXT, md5 TEXT, sha1 TEXT,
  header_rule   TEXT,                  -- which rule was applied when hashing
  rom_id        INTEGER REFERENCES roms(id),
  state         TEXT NOT NULL,         -- 'verified' | 'unverified' | 'misnamed' | 'bad' | 'pending'
  scanned_at    INTEGER NOT NULL,
  UNIQUE (platform_id, rel_path)
);
CREATE INDEX files_rom ON files(rom_id);
CREATE INDEX files_rel_lower ON files(platform_id, lower(rel_path));   -- arcade rows match zips ignoring case

CREATE TABLE sources (                  -- one per torrent the user dropped in
  id            INTEGER PRIMARY KEY,
  infohash      TEXT NOT NULL UNIQUE,
  display_name  TEXT NOT NULL,         -- torrent name field
  origin_file   TEXT NOT NULL,         -- basename as dropped
  platform_id   TEXT REFERENCES platforms(id),   -- NULL while unbound
  bind_score    REAL,                  -- hit rate that produced the binding
  state         TEXT NOT NULL,         -- 'resolving' | 'unbound' | 'bound' | 'disabled'
  reason        TEXT,                  -- why it is resolving or unbound, shown to the user
  seed_policy   TEXT NOT NULL DEFAULT 'none',   -- 'none' | 'ratio:1.0' | 'client'
  file_count    INTEGER NOT NULL DEFAULT 0,
  total_size    INTEGER NOT NULL DEFAULT 0,
  client_id     TEXT,                  -- id in the download client once added, else NULL
  added_at      INTEGER NOT NULL,
  suggested_platform_id TEXT REFERENCES platforms(id),  -- guessed from the torrent's names, no DAT needed
  user_unbound  INTEGER NOT NULL DEFAULT 0   -- 1 after the user unbound it; never bound automatically again
);
CREATE INDEX sources_state ON sources(state);

CREATE TABLE torrent_files (
  source_id     INTEGER NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
  file_index    INTEGER NOT NULL,
  path          TEXT NOT NULL,         -- inside the torrent, without the torrent's name
  size          INTEGER NOT NULL,
  rom_id        INTEGER REFERENCES roms(id),   -- best pre-download match, may be NULL
  confidence    TEXT,                  -- 'name' | 'size', NULL when unmatched
  PRIMARY KEY (source_id, file_index)
);
CREATE INDEX torrent_files_rom ON torrent_files(rom_id);

CREATE TABLE downloads (
  id            INTEGER PRIMARY KEY,
  title_id      INTEGER NOT NULL REFERENCES titles(id),
  rom_id        INTEGER NOT NULL REFERENCES roms(id),
  source_id     INTEGER REFERENCES sources(id) ON DELETE SET NULL,  -- NULL while 'wanted' or once the source is deleted
  file_index    INTEGER,                         -- NULL while 'wanted'
  state         TEXT NOT NULL,         -- see state machine
  progress      REAL NOT NULL DEFAULT 0,         -- 0 to 1
  staged_path   TEXT,                  -- local path of the finished file, set on 'importing'
  error         TEXT,                  -- why it failed, shown to the user
  created_at    INTEGER NOT NULL,
  updated_at    INTEGER NOT NULL
);
CREATE INDEX downloads_state ON downloads(state);
CREATE INDEX downloads_source ON downloads(source_id, file_index);
CREATE INDEX downloads_rom ON downloads(rom_id);

CREATE TABLE import_log (
  id            INTEGER PRIMARY KEY,
  at            INTEGER NOT NULL,
  download_id   INTEGER REFERENCES downloads(id),
  file_id       INTEGER REFERENCES files(id),
  action        TEXT NOT NULL,         -- 'placed' | 'replaced' | 'quarantined' | 'skipped_existing' | 'renamed'
  detail        TEXT NOT NULL          -- json
);
CREATE INDEX import_log_file ON import_log(file_id);

CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE jobs (
  id            INTEGER PRIMARY KEY,
  kind          TEXT NOT NULL,         -- 'scan' | 'import' | 'poll' | 'detect_client' | 'dat_import' | 'recompute_1g1r' | 'source_import' | 'resolve_magnet' | 'transfer' | 'deselect' | 'arcade_catalog'
  payload       TEXT NOT NULL,         -- json
  state         TEXT NOT NULL,         -- 'queued' | 'running' | 'paused' | 'done' | 'failed'
  progress      TEXT,                  -- json, job specific
  created_at    INTEGER NOT NULL,
  updated_at    INTEGER NOT NULL,
  lane          TEXT NOT NULL DEFAULT 'light'   -- 'heavy' | 'background' | 'light'
);
```

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
- `done`, `bad`, `failed`, `cancelled`: terminal. `failed` may be retried,
  which returns it to `queued` on the same torrent_file; `bad` never retries
  the same torrent_file. Cancelling a `transferring` or `checking` row
  deselects its file in the client and stops the torrent when nothing of it
  is selected any more. `importing` rows cannot be cancelled.
- One rom has at most one row outside the terminal states; wanting a title
  again skips roms that have one or that already have a verified file.

`staged_path` is `<paths.data>/staging/<infohash>/<name>/<path>` for a
multi-file torrent, where `<name>` is `sources.display_name` and `<path>` is
`torrent_files.path`, and `<paths.data>/staging/<infohash>/<path>` for a
single-file one. It is the path mistarr sees, after the remote path map.

### files.state

- `pending`: seen, not yet hashed.
- `verified`: hash matches a rom and the filename is what the adapter expects.
- `misnamed`: hash matches a rom, name differs. The UI offers rename.
- `unverified`: no rom matches in any loaded DAT. A member of an imported MRA
  zip the md5 check did not read, or that no hash source covers, is
  `unverified` with `rom_id` set to the zip's rom, and so is the presence row
  `mame/<zip>` the arcade presence pass writes for a zip a live MRA names.
- `bad`: matches a rom flagged `baddump`.

### sources.state

- `resolving`: a magnet whose file list is not known yet: no client, not yet
  added, or the client is fetching metadata; `reason` says which.
- `unbound`: file list known, no platform reached the binding threshold.
- `bound`: attached to a platform, torrent_files populated.
- `disabled`: user turned it off; existing downloads finish, nothing new is
  chosen from it.

## Titles across DAT versions

A title row belongs to the DAT name, not to one version. Loading a version
reuses the row with the same game name from any version of the same
`dat_name`, moving its `dat_version_id` forward, so `titles.id`, `wanted` and
the `files.rom_id` links survive an update. Titles left on older versions
after a load are the entries the new version dropped; they get `retired = 1`
and are never deleted, as do titles of the same version a reload of it no
longer lists. Roms a kept entry no longer lists get `roms.retired = 1`.
A version whose string sorts below the newest live one of its name is stored
already superseded and does not touch titles. An unbound version stores only
its `dat_versions` row; binding it re-reads the file from `dats/loaded/`.

### MRA titles

Arcade titles read from MRA files have `source = 'mra'` and all belong to
one `dat_versions` row with `source = 'mra'` and `dat_name` `_Arcade`, which
`/dats`, the wizard and DAT loads never see. DAT supersession, reloads and
`DELETE /dats/{id}` only retire `source = 'dat'` titles; an MRA title is
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
regrouped by `(platform_id, group_key)` after each load, electing the parent
that wins 1G1R under default preferences.

## Derived views

The browse screen needs one row per clone group with have/wanted counts. Keep
this as a SQL view so both the API and tests use the same definition. Roms and
files are aggregated per title first, so a title with several roms or several
files per rom counts once:

```sql
CREATE VIEW title_groups AS
SELECT g.parent_id, g.platform_id, p.base_name, p.name,
       g.variants, g.have_verified, g.wanted, g.has_pick, g.pick_id, g.newest_id
FROM (
  SELECT v.platform_id, v.parent_id,
         COUNT(*) AS variants,
         SUM(v.roms > 0 AND v.roms_verified = v.roms) AS have_verified,
         SUM(v.wanted) AS wanted,
         MAX(v.is_1g1r_pick) AS has_pick,
         MAX(CASE WHEN v.is_1g1r_pick = 1 THEN v.id END) AS pick_id,
         MAX(v.id) AS newest_id
  FROM (
    SELECT t.platform_id, t.parent_id, t.id, t.wanted, t.is_1g1r_pick,
           COUNT(DISTINCT r.id) AS roms,
           COUNT(DISTINCT CASE WHEN f.state = 'verified'
                                 OR (r.present = 1
                                     AND COALESCE(t.mra_check, '') NOT IN ('mismatch', 'missing_part'))
                               THEN r.id END) AS roms_verified
    FROM titles t
    LEFT JOIN roms r ON r.title_id = t.id AND r.retired = 0
    LEFT JOIN files f ON f.rom_id = r.id
    WHERE t.retired = 0
    GROUP BY t.platform_id, t.parent_id, t.id
  ) v
  GROUP BY v.platform_id, v.parent_id
) g
JOIN titles p ON p.id = g.parent_id;
```

`variants` counts live titles, `have_verified` the live titles whose every
live rom has a `verified` file, or for an MRA title is present with no failed
md5 check, `wanted` the wanted live titles, and
`newest_id` orders groups by when their newest entry first appeared.
