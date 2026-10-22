CREATE TABLE "platforms" (
  id           INTEGER PRIMARY KEY,
  slug         TEXT NOT NULL UNIQUE,
  name         TEXT NOT NULL,
  core_dir     TEXT NOT NULL,
  kind         INTEGER NOT NULL CHECK (kind BETWEEN 0 AND 3),
  core_present INTEGER NOT NULL DEFAULT 0 CHECK (core_present IN (0, 1)),
  enabled      INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1))
) STRICT;
