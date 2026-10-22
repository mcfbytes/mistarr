CREATE TABLE "chd_whole" (
  chd_sha1 BLOB NOT NULL CHECK (length(chd_sha1) = 20),
  chd_size INTEGER NOT NULL,
  mtime    INTEGER NOT NULL,
  crc32    BLOB NOT NULL CHECK (length(crc32) = 4),
  md5      BLOB NOT NULL CHECK (length(md5) = 16),
  sha1     BLOB NOT NULL CHECK (length(sha1) = 20),
  PRIMARY KEY (chd_sha1, chd_size, mtime)
) STRICT, WITHOUT ROWID;
