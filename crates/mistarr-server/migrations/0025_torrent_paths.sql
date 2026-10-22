-- foreign_keys: off
-- Torrent paths interned as directories and leaves, source and download
-- codes, and integer confidences; see docs/NORMALIZATION.md P5 and P11.

DROP VIEW IF EXISTS torrent_file_view;

DROP INDEX IF EXISTS sources_state;

CREATE TABLE sources_new (
  id                 INTEGER PRIMARY KEY,
  infohash           TEXT NOT NULL UNIQUE,
  display_name       TEXT NOT NULL,
  origin_file        TEXT NOT NULL,
  platform           INTEGER REFERENCES platforms(id),
  bind_score         REAL CHECK (bind_score BETWEEN 0 AND 1),
  state              INTEGER NOT NULL CHECK (state BETWEEN 0 AND 3),
  reason             TEXT,
  seed_policy        INTEGER NOT NULL DEFAULT 0 CHECK (seed_policy BETWEEN 0 AND 2),
  seed_ratio         REAL CHECK ((seed_policy = 1) = (seed_ratio IS NOT NULL)),
  file_count         INTEGER NOT NULL DEFAULT 0 CHECK (file_count >= 0),
  total_size         INTEGER NOT NULL DEFAULT 0 CHECK (total_size >= 0),
  client_id          TEXT,
  added_at           INTEGER NOT NULL,
  suggested_platform INTEGER REFERENCES platforms(id),
  user_binding       INTEGER NOT NULL DEFAULT 0 CHECK (user_binding IN (0, 1)),
  map_stamp          TEXT,
  bind_pending       INTEGER CHECK (bind_pending BETWEEN 0 AND 2),
  bind_platform      INTEGER REFERENCES platforms(id),
  CHECK (seed_policy <> 1 OR seed_ratio > 0),
  CHECK ((bind_pending IS 2) = (bind_platform IS NOT NULL))
) STRICT;

INSERT INTO sources_new (id, infohash, display_name, origin_file, platform, bind_score, state,
                         reason, seed_policy, seed_ratio, file_count, total_size, client_id,
                         added_at, suggested_platform, user_binding, map_stamp, bind_pending,
                         bind_platform)
SELECT id, infohash, display_name, origin_file,
       (SELECT p.id FROM platforms p WHERE p.slug = s.platform_id),
       bind_score,
       CASE state WHEN 'resolving' THEN 0 WHEN 'unbound' THEN 1 WHEN 'bound' THEN 2
                  WHEN 'disabled' THEN 3 ELSE -1 END,
       reason,
       CASE WHEN seed_policy = 'none' THEN 0 WHEN seed_policy = 'client' THEN 2
            WHEN seed_policy LIKE 'ratio:%' THEN 1 ELSE -1 END,
       CASE WHEN seed_policy LIKE 'ratio:%' THEN CAST(substr(seed_policy, 7) AS REAL) END,
       file_count, total_size, client_id, added_at,
       (SELECT p.id FROM platforms p WHERE p.slug = s.suggested_platform_id),
       user_binding, map_stamp,
       CASE WHEN bind_pending IS NULL THEN NULL
            WHEN bind_pending = 'automatic' THEN 0 WHEN bind_pending = 'none' THEN 1
            WHEN bind_pending LIKE 'platform:%' THEN 2 ELSE -1 END,
       CASE WHEN bind_pending LIKE 'platform:%'
            THEN (SELECT p.id FROM platforms p WHERE p.slug = substr(s.bind_pending, 10)) END
FROM sources s ORDER BY id;

DROP TABLE sources;
ALTER TABLE sources_new RENAME TO sources;
CREATE INDEX sources_state ON sources(state);

DROP INDEX IF EXISTS downloads_state;
DROP INDEX IF EXISTS downloads_source;
DROP INDEX IF EXISTS downloads_rom;
DROP INDEX IF EXISTS downloads_recent;

CREATE TABLE downloads_new (
  id          INTEGER PRIMARY KEY,
  title_id    INTEGER NOT NULL REFERENCES titles(id),
  rom_id      INTEGER NOT NULL REFERENCES roms(id),
  source_id   INTEGER REFERENCES sources(id) ON DELETE SET NULL,
  file_index  INTEGER,
  state       INTEGER NOT NULL CHECK (state BETWEEN 0 AND 8),
  progress    REAL NOT NULL DEFAULT 0 CHECK (progress BETWEEN 0 AND 1),
  staged_path TEXT,
  error       TEXT,
  created_at  INTEGER NOT NULL,
  updated_at  INTEGER NOT NULL
) STRICT;

INSERT INTO downloads_new (id, title_id, rom_id, source_id, file_index, state, progress,
                           staged_path, error, created_at, updated_at)
SELECT id, title_id, rom_id, source_id, file_index,
       CASE state WHEN 'wanted' THEN 0 WHEN 'queued' THEN 1 WHEN 'transferring' THEN 2
                  WHEN 'checking' THEN 3 WHEN 'importing' THEN 4 WHEN 'done' THEN 5
                  WHEN 'bad' THEN 6 WHEN 'failed' THEN 7 WHEN 'cancelled' THEN 8
                  ELSE -1 END,
       progress, staged_path, error, created_at, updated_at
FROM downloads ORDER BY id;

DROP TABLE downloads;
ALTER TABLE downloads_new RENAME TO downloads;
CREATE INDEX downloads_state ON downloads(state);
CREATE INDEX downloads_source ON downloads(source_id, file_index);
CREATE INDEX downloads_rom ON downloads(rom_id);
CREATE INDEX downloads_recent ON downloads(updated_at DESC, id DESC);

DROP INDEX IF EXISTS torrent_files_rom;
DROP INDEX IF EXISTS torrent_files_source_rom;

CREATE TABLE torrent_dirs (
  id        INTEGER PRIMARY KEY,
  source_id INTEGER NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
  path      TEXT NOT NULL,
  UNIQUE (source_id, path)
) STRICT;

-- The directory of a path is everything through its last '/', and '' at the top.
INSERT INTO torrent_dirs (source_id, path)
  SELECT DISTINCT source_id, rtrim(path, replace(path, '/', '')) FROM torrent_files;

CREATE TABLE torrent_files_new (
  source_id  INTEGER NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
  file_index INTEGER NOT NULL,
  dir_id     INTEGER NOT NULL REFERENCES torrent_dirs(id),
  leaf       TEXT NOT NULL,
  size       INTEGER NOT NULL,
  rom_id     INTEGER REFERENCES roms(id),
  confidence INTEGER CHECK (confidence BETWEEN 0 AND 2),
  PRIMARY KEY (source_id, file_index)
) STRICT, WITHOUT ROWID;

INSERT INTO torrent_files_new (source_id, file_index, dir_id, leaf, size, rom_id, confidence)
  SELECT f.source_id, f.file_index,
         (SELECT d.id FROM torrent_dirs d
           WHERE d.source_id = f.source_id
             AND d.path = rtrim(f.path, replace(f.path, '/', ''))),
         substr(f.path, length(rtrim(f.path, replace(f.path, '/', ''))) + 1),
         f.size, f.rom_id,
         CASE WHEN f.confidence IS NULL THEN NULL
              WHEN f.confidence = 'hash' THEN 0
              WHEN f.confidence = 'name' THEN 1
              WHEN f.confidence IN ('base', 'size') THEN 2
              ELSE -1 END
  FROM torrent_files f
  ORDER BY f.source_id, f.file_index;

DROP TABLE torrent_files;
ALTER TABLE torrent_files_new RENAME TO torrent_files;
CREATE INDEX torrent_files_rom ON torrent_files(rom_id);
CREATE INDEX torrent_files_source_rom ON torrent_files(source_id, rom_id);

DROP INDEX IF EXISTS torrent_candidates_rom;

CREATE TABLE torrent_candidates_new (
  source_id  INTEGER NOT NULL,
  file_index INTEGER NOT NULL,
  rom_id     INTEGER NOT NULL REFERENCES roms(id),
  confidence INTEGER NOT NULL CHECK (confidence BETWEEN 1 AND 4),
  PRIMARY KEY (source_id, file_index, rom_id),
  FOREIGN KEY (source_id, file_index)
    REFERENCES torrent_files(source_id, file_index) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

INSERT INTO torrent_candidates_new (source_id, file_index, rom_id, confidence)
  SELECT source_id, file_index, rom_id,
         CASE confidence WHEN 'name' THEN 1 WHEN 'base' THEN 2
                         WHEN 'fuzzy' THEN 3 WHEN 'size' THEN 4 ELSE -1 END
  FROM torrent_candidates ORDER BY source_id, file_index, rom_id;

DROP TABLE torrent_candidates;
ALTER TABLE torrent_candidates_new RENAME TO torrent_candidates;
CREATE INDEX torrent_candidates_rom ON torrent_candidates(rom_id);

CREATE VIEW torrent_file_view AS
SELECT f.source_id, f.file_index,
       (SELECT d.path FROM torrent_dirs d WHERE d.id = f.dir_id) || f.leaf AS path,
       f.size, f.rom_id, f.confidence,
       CASE f.confidence WHEN 0 THEN 'hash' WHEN 1 THEN 'name' WHEN 2 THEN 'base' END
         AS confidence_text
FROM torrent_files f;
