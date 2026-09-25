-- CHD disc images identified by their decoded tracks; see docs/CHD.md and docs/VERIFICATION.md.

ALTER TABLE files ADD COLUMN reason TEXT;     -- why an 'unidentified' row is not identified; NULL otherwise

CREATE TABLE chd_tracks (                     -- rebuilt .bin hashes of a CHD's tracks, keyed by content
  chd_sha1 TEXT NOT NULL,                     -- header combined SHA1, verified by the decode
  chd_size INTEGER NOT NULL,
  track    INTEGER NOT NULL,                  -- 1-based
  size     INTEGER NOT NULL,                  -- rebuilt track length
  crc32 TEXT NOT NULL, md5 TEXT NOT NULL, sha1 TEXT NOT NULL,
  PRIMARY KEY (chd_sha1, chd_size, track)
) WITHOUT ROWID;
CREATE INDEX chd_tracks_sha1 ON chd_tracks(sha1);

CREATE TABLE chd_failures (                   -- CHDs that cannot be identified, so they are not decoded again
  chd_sha1  TEXT NOT NULL,
  chd_size  INTEGER NOT NULL,
  mtime     INTEGER NOT NULL,                 -- of the file that failed; a rewritten file is retried
  reason    TEXT NOT NULL,                    -- a files.reason code
  decoder   INTEGER NOT NULL,                 -- decoder version; an older one is retried
  failed_at INTEGER NOT NULL,
  PRIMARY KEY (chd_sha1, chd_size, mtime)
) WITHOUT ROWID;

CREATE TABLE chd_whole (                      -- whole-file hashes of CHDs a DAT's `.chd` rom size had hashed
  chd_sha1 TEXT NOT NULL,
  chd_size INTEGER NOT NULL,
  mtime    INTEGER NOT NULL,                  -- of the file hashed
  crc32 TEXT NOT NULL, md5 TEXT NOT NULL, sha1 TEXT NOT NULL,
  PRIMARY KEY (chd_sha1, chd_size, mtime)
) WITHOUT ROWID;

-- Whether a scanned .chd has the size of a DAT's .chd rom, whatever other size indexes exist.
CREATE INDEX roms_chd_size ON roms(size) WHERE lower(name) LIKE '%.chd';

-- Unmatched whole-file hashes of disc CHDs; the next scan reads their headers.
UPDATE import_log SET file_id = NULL WHERE file_id IN (
  SELECT id FROM files WHERE lower(rel_path) LIKE '%.chd' AND rom_id IS NULL
    AND platform_id IN (SELECT id FROM platforms WHERE kind = 'disc'));
DELETE FROM files WHERE lower(rel_path) LIKE '%.chd' AND rom_id IS NULL
  AND platform_id IN (SELECT id FROM platforms WHERE kind = 'disc');
