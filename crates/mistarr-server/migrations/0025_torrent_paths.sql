-- foreign_keys: off
-- Torrent paths interned per source as a directory plus a leaf, and match
-- confidences as integer codes; see docs/NORMALIZATION.md "P5 torrent_files".

DROP VIEW IF EXISTS torrent_file_view;

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
