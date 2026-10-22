CREATE TABLE torrent_dirs (
  id        INTEGER PRIMARY KEY,
  source_id INTEGER NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
  path      TEXT NOT NULL,
  UNIQUE (source_id, path)
) STRICT;
