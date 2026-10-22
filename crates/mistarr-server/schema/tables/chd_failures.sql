CREATE TABLE "chd_failures" (
  chd_sha1  BLOB NOT NULL CHECK (length(chd_sha1) = 20),
  chd_size  INTEGER NOT NULL,
  mtime     INTEGER NOT NULL,
  reason    INTEGER NOT NULL CHECK (reason BETWEEN 0 AND 16),
  decoder   INTEGER NOT NULL,
  failed_at INTEGER NOT NULL,
  PRIMARY KEY (chd_sha1, chd_size, mtime)
) STRICT, WITHOUT ROWID;
