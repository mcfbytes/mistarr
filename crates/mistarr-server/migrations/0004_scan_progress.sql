CREATE TABLE scan_progress (           -- resumable per-platform library scan state
  platform_id   TEXT PRIMARY KEY REFERENCES platforms(id),
  done_dirs     TEXT NOT NULL DEFAULT '[]',   -- json array of directories already committed
  updated_at    INTEGER NOT NULL
);
