-- Removes member rows the generic scan wrote for arcade zips before scan
-- started skipping arcade; the import path always sets rom_id, so this cannot touch it.
DELETE FROM files
WHERE platform_id = 'arcade'
  AND state = 'unverified'
  AND rom_id IS NULL
  AND instr(rel_path, '#') > 0;
