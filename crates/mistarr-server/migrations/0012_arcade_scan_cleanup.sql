-- Removes the rows a library scan wrote for arcade zips that matched no rom.
-- Every row the import path or the arcade presence pass writes has a rom_id.
DELETE FROM files
WHERE platform_id = 'arcade'
  AND state = 'unverified'
  AND rom_id IS NULL;

-- Arcade has no library scan, so it has no scan to resume.
DELETE FROM scan_progress WHERE platform_id = 'arcade';
