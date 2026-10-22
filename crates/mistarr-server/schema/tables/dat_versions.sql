CREATE TABLE "dat_versions" (
  id            INTEGER PRIMARY KEY,
  platform      INTEGER REFERENCES platforms(id),
  dat_name      TEXT NOT NULL,
  version       TEXT NOT NULL,
  source_file   TEXT NOT NULL,
  loaded_at     INTEGER NOT NULL,
  superseded_by INTEGER REFERENCES dat_versions(id),
  game_count    INTEGER NOT NULL CHECK (game_count >= 0),
  retired       INTEGER NOT NULL DEFAULT 0 CHECK (retired IN (0, 1)),
  source        INTEGER NOT NULL DEFAULT 0 CHECK (source IN (0, 1)),
  family        TEXT NOT NULL,
  UNIQUE (dat_name, version)
) STRICT;

CREATE INDEX dat_versions_family ON dat_versions(family, platform);
