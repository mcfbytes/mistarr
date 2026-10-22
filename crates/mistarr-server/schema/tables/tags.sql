CREATE TABLE tags (
  id   INTEGER PRIMARY KEY,
  kind INTEGER NOT NULL CHECK (kind BETWEEN 0 AND 2),
  name TEXT NOT NULL,
  bit  INTEGER CHECK (bit > 0),
  UNIQUE (kind, name)
) STRICT;

CREATE INDEX tags_kind_nocase ON tags(kind, name COLLATE NOCASE);
