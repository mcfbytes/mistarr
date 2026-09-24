-- A `wanted` download has no torrent_file yet, so source_id and file_index become nullable;
-- deleting a source keeps its finished downloads with source_id NULL.
PRAGMA defer_foreign_keys = ON;

CREATE TABLE downloads_next (
  id            INTEGER PRIMARY KEY,
  title_id      INTEGER NOT NULL REFERENCES titles(id),
  rom_id        INTEGER NOT NULL REFERENCES roms(id),
  source_id     INTEGER REFERENCES sources(id) ON DELETE SET NULL,
  file_index    INTEGER,
  state         TEXT NOT NULL,
  progress      REAL NOT NULL DEFAULT 0,
  staged_path   TEXT,
  error         TEXT,
  created_at    INTEGER NOT NULL,
  updated_at    INTEGER NOT NULL
);
INSERT INTO downloads_next SELECT id, title_id, rom_id, source_id, file_index, state, progress,
  staged_path, error, created_at, updated_at FROM downloads;
DROP TABLE downloads;
ALTER TABLE downloads_next RENAME TO downloads;

CREATE INDEX downloads_state ON downloads(state);
CREATE INDEX downloads_source ON downloads(source_id, file_index);
CREATE INDEX downloads_rom ON downloads(rom_id);
