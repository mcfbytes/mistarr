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
  UNIQUE (dat_name, version)
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
  UNIQUE (dat_version_id, name)
);
CREATE INDEX titles_platform_base ON titles(platform_id, base_name);

CREATE TABLE roms (                     -- one per <rom>; the file unit
  id            INTEGER PRIMARY KEY,
  title_id      INTEGER NOT NULL REFERENCES titles(id),
  name          TEXT NOT NULL,         -- filename in the DAT, may include a subdir for discs
  size          INTEGER NOT NULL,
  crc32         TEXT, md5 TEXT, sha1 TEXT,
  status        TEXT NOT NULL DEFAULT 'nodump',   -- 'good' | 'baddump' | 'nodump' | 'verified'
  UNIQUE (title_id, name)
);
CREATE INDEX roms_sha1 ON roms(sha1);
CREATE INDEX roms_md5  ON roms(md5);
CREATE INDEX roms_crc  ON roms(crc32, size);

CREATE TABLE files (                    -- what is on disk under games/
  id            INTEGER PRIMARY KEY,
  platform_id   TEXT NOT NULL REFERENCES platforms(id),
  rel_path      TEXT NOT NULL,         -- relative to games/<core_dir>/, includes zip member as 'a.zip#b.nes'
  size          INTEGER NOT NULL,
  mtime         INTEGER NOT NULL,
  crc32 TEXT, md5 TEXT, sha1 TEXT,
  header_rule   TEXT,                  -- which rule was applied when hashing
  rom_id        INTEGER REFERENCES roms(id),
  state         TEXT NOT NULL,         -- 'verified' | 'unverified' | 'misnamed' | 'bad' | 'pending'
  scanned_at    INTEGER NOT NULL,
  UNIQUE (platform_id, rel_path)
);

CREATE TABLE sources (                  -- one per torrent the user dropped in
  id            INTEGER PRIMARY KEY,
  infohash      TEXT NOT NULL UNIQUE,
  display_name  TEXT NOT NULL,         -- torrent name field
  origin_file   TEXT NOT NULL,         -- basename as dropped
  platform_id   TEXT REFERENCES platforms(id),   -- NULL while unbound
  bind_score    REAL,                  -- hit rate that produced the binding
  state         TEXT NOT NULL,         -- 'resolving' | 'unbound' | 'bound' | 'disabled'
  seed_policy   TEXT NOT NULL DEFAULT 'none',   -- 'none' | 'ratio:1.0' | 'client'
  file_count    INTEGER NOT NULL DEFAULT 0,
  total_size    INTEGER NOT NULL DEFAULT 0,
  client_id     TEXT,                  -- id in the download client once added, else NULL
  added_at      INTEGER NOT NULL
);

CREATE TABLE torrent_files (
  source_id     INTEGER NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
  file_index    INTEGER NOT NULL,
  path          TEXT NOT NULL,
  size          INTEGER NOT NULL,
  rom_id        INTEGER REFERENCES roms(id),   -- best pre-download match, may be NULL
  PRIMARY KEY (source_id, file_index)
);
CREATE INDEX torrent_files_rom ON torrent_files(rom_id);

CREATE TABLE downloads (
  id            INTEGER PRIMARY KEY,
  title_id      INTEGER NOT NULL REFERENCES titles(id),
  rom_id        INTEGER NOT NULL REFERENCES roms(id),
  source_id     INTEGER NOT NULL REFERENCES sources(id),
  file_index    INTEGER NOT NULL,
  state         TEXT NOT NULL,         -- see state machine
  progress      REAL NOT NULL DEFAULT 0,
  staged_path   TEXT,
  error         TEXT,
  created_at    INTEGER NOT NULL,
  updated_at    INTEGER NOT NULL
);

CREATE TABLE import_log (
  id            INTEGER PRIMARY KEY,
  at            INTEGER NOT NULL,
  download_id   INTEGER REFERENCES downloads(id),
  file_id       INTEGER REFERENCES files(id),
  action        TEXT NOT NULL,         -- 'placed' | 'replaced' | 'quarantined' | 'skipped_existing'
  detail        TEXT NOT NULL          -- json
);

CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE jobs (
  id            INTEGER PRIMARY KEY,
  kind          TEXT NOT NULL,         -- 'scan' | 'import' | 'poll' | 'bind' | 'detect_client'
  payload       TEXT NOT NULL,         -- json
  state         TEXT NOT NULL,         -- 'queued' | 'running' | 'paused' | 'done' | 'failed'
  progress      TEXT,                  -- json, job specific
  created_at    INTEGER NOT NULL,
  updated_at    INTEGER NOT NULL
);

CREATE VIEW title_groups AS
SELECT p.id AS parent_id, p.platform_id, p.base_name,
       COUNT(t.id) AS variants,
       SUM(CASE WHEN f.state = 'verified' THEN 1 ELSE 0 END) AS have_verified,
       SUM(t.wanted) AS wanted,
       MAX(t.is_1g1r_pick) AS has_pick
FROM titles p
JOIN titles t ON t.parent_id = p.id AND t.retired = 0
LEFT JOIN roms r ON r.title_id = t.id
LEFT JOIN files f ON f.rom_id = r.id
WHERE p.parent_id = p.id
GROUP BY p.id;

