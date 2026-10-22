CREATE TABLE "roms" (
  id         INTEGER PRIMARY KEY,
  title_id   INTEGER NOT NULL REFERENCES titles(id),
  own_name   INTEGER NOT NULL DEFAULT 1 CHECK (own_name IN (0, 1)),
  name       TEXT NOT NULL,
  size       INTEGER NOT NULL CHECK (size >= 0),
  crc32      BLOB CHECK (length(crc32) = 4),
  md5        BLOB CHECK (length(md5) = 16),
  sha1       BLOB CHECK (length(sha1) = 20),
  status     INTEGER NOT NULL DEFAULT 2 CHECK (status BETWEEN 0 AND 3),
  retired    INTEGER NOT NULL DEFAULT 0 CHECK (retired IN (0, 1)),
  match_name TEXT,
  match_base TEXT,
  header     TEXT,
  zip_dir    TEXT,
  present    INTEGER NOT NULL DEFAULT 0 CHECK (present IN (0, 1)),
  CHECK (own_name = 1 OR name <> '')
) STRICT;

CREATE INDEX roms_chd_size ON roms(size) WHERE lower(name) LIKE '%.chd';

CREATE INDEX roms_crc ON roms(crc32, size);

CREATE INDEX roms_id_title ON roms(id, title_id);

CREATE INDEX roms_match_base ON roms(match_base, size, title_id) WHERE match_base IS NOT NULL;

CREATE INDEX roms_match_name ON roms(match_name, title_id);

CREATE INDEX roms_md5 ON roms(md5) WHERE sha1 IS NULL;

CREATE INDEX roms_sha1 ON roms(sha1);

CREATE INDEX roms_size ON roms(size) WHERE match_base IS NOT NULL;

CREATE UNIQUE INDEX roms_title_name ON roms(title_id, own_name, name);

CREATE INDEX roms_track_size ON roms(size) WHERE size % 2352 = 0;

CREATE TRIGGER roms_delete_groups AFTER DELETE ON roms WHEN OLD.retired = 0 BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT group_root FROM titles WHERE id = OLD.title_id AND group_root IS NOT NULL
      AND group_root NOT IN (SELECT parent_id FROM title_groups_dirty);
END;

CREATE TRIGGER roms_insert_groups AFTER INSERT ON roms WHEN NEW.retired = 0 BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT group_root FROM titles WHERE id = NEW.title_id AND group_root IS NOT NULL
      AND group_root NOT IN (SELECT parent_id FROM title_groups_dirty);
END;

CREATE TRIGGER roms_update_groups AFTER UPDATE OF title_id, retired, present ON roms
WHEN (OLD.retired = 0 OR NEW.retired = 0) AND (OLD.title_id IS NOT NEW.title_id
  OR OLD.retired IS NOT NEW.retired OR OLD.present IS NOT NEW.present) BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT DISTINCT group_root FROM titles WHERE id IN (OLD.title_id, NEW.title_id)
      AND group_root IS NOT NULL AND group_root NOT IN (SELECT parent_id FROM title_groups_dirty);
END;
