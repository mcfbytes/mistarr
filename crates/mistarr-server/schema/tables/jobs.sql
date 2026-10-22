CREATE TABLE "jobs" (
  id         INTEGER PRIMARY KEY,
  kind       INTEGER NOT NULL CHECK (kind BETWEEN 0 AND 13),
  payload    TEXT NOT NULL,
  state      INTEGER NOT NULL CHECK (state BETWEEN 0 AND 4),
  progress   TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  lane       INTEGER NOT NULL DEFAULT 2 CHECK (lane BETWEEN 0 AND 3),
  subject    TEXT NOT NULL
) STRICT;

CREATE INDEX jobs_state ON jobs(state, id);

CREATE INDEX jobs_subject ON jobs(kind, subject, state);
