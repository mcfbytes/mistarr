CREATE TABLE "files" (
  id          INTEGER PRIMARY KEY,
  platform    INTEGER NOT NULL REFERENCES platforms(id),
  rel_path    TEXT NOT NULL,
  size        INTEGER NOT NULL,
  mtime       INTEGER NOT NULL,
  crc32       BLOB CHECK (length(crc32) = 4),
  md5         BLOB CHECK (length(md5) = 16),
  sha1        BLOB CHECK (length(sha1) = 20),
  header_rule INTEGER CHECK (header_rule BETWEEN 0 AND 6),
  rom_id      INTEGER REFERENCES roms(id),
  state       INTEGER NOT NULL CHECK (state BETWEEN 0 AND 5),
  scanned_at  INTEGER NOT NULL,
  reason      INTEGER CHECK (reason BETWEEN 0 AND 16),
  crc32_whole BLOB CHECK (length(crc32_whole) = 4),
  md5_whole   BLOB CHECK (length(md5_whole) = 16),
  sha1_whole  BLOB CHECK (length(sha1_whole) = 20),
  UNIQUE (platform, rel_path),
  CHECK (reason IS NULL OR state = 5)
) STRICT;

CREATE INDEX files_rel_lower ON files(platform, lower(rel_path));

CREATE INDEX files_rom ON files(rom_id);

CREATE INDEX files_state ON files(state, platform);

CREATE TRIGGER files_delete_groups AFTER DELETE ON files
WHEN OLD.rom_id IS NOT NULL AND OLD.state = 1 BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT t.group_root FROM roms r JOIN titles t ON t.id = r.title_id
    WHERE r.id = OLD.rom_id AND t.group_root IS NOT NULL
      AND t.group_root NOT IN (SELECT parent_id FROM title_groups_dirty);
END;

CREATE TRIGGER files_insert_groups AFTER INSERT ON files
WHEN NEW.rom_id IS NOT NULL AND NEW.state = 1 BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT t.group_root FROM roms r JOIN titles t ON t.id = r.title_id
    WHERE r.id = NEW.rom_id AND t.group_root IS NOT NULL
      AND t.group_root NOT IN (SELECT parent_id FROM title_groups_dirty);
END;

CREATE TRIGGER files_update_groups AFTER UPDATE OF rom_id, state ON files
WHEN (OLD.state = 1 OR NEW.state = 1)
  AND (OLD.rom_id IS NOT NEW.rom_id OR OLD.state IS NOT NEW.state) BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT DISTINCT t.group_root FROM roms r JOIN titles t ON t.id = r.title_id
    WHERE r.id IN (OLD.rom_id, NEW.rom_id) AND t.group_root IS NOT NULL
      AND t.group_root NOT IN (SELECT parent_id FROM title_groups_dirty);
END;
