-- Removes generic-scan noise rows for arcade zips: the import path and the
-- arcade presence pass always set rom_id, so this cannot touch either.
DELETE FROM files
WHERE platform_id = 'arcade'
  AND state = 'unverified'
  AND rom_id IS NULL;

-- Arcade is never scanned, so it never resumes one.
DELETE FROM scan_progress WHERE platform_id = 'arcade';
