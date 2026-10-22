CREATE TABLE "sources" (
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

CREATE INDEX sources_state ON sources(state);
