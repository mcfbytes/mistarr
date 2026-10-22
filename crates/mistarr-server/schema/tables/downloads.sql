CREATE TABLE "downloads" (
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

CREATE INDEX downloads_recent ON downloads(updated_at DESC, id DESC);

CREATE INDEX downloads_rom ON downloads(rom_id);

CREATE INDEX downloads_source ON downloads(source_id, file_index);

CREATE INDEX downloads_state ON downloads(state);
