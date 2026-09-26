-- Whole-file hashes beside the stored ones under a rule that strips a header; see docs/DATA-MODEL.md "files".
ALTER TABLE files ADD COLUMN crc32_whole TEXT;
ALTER TABLE files ADD COLUMN md5_whole TEXT;
ALTER TABLE files ADD COLUMN sha1_whole TEXT;

-- Rows hashed under those rules hold the stripped form alone; a scan at startup hashes them again.
INSERT OR IGNORE INTO scan_progress (platform_id, done_dirs, updated_at)
  SELECT DISTINCT platform_id, '[]', 0 FROM files
  WHERE header_rule IN ('ines', 'a78', 'lnx') AND (sha1 IS NOT NULL OR md5 IS NOT NULL)
    AND platform_id IN (SELECT id FROM platforms WHERE enabled = 1);
