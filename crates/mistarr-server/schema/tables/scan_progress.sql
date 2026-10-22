CREATE TABLE "scan_progress" (
  platform   INTEGER PRIMARY KEY REFERENCES platforms(id),
  done_dirs  TEXT NOT NULL DEFAULT '[]',
  updated_at INTEGER NOT NULL
) STRICT;
