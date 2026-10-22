-- foreign_keys: off
-- DAT versions, titles and group summaries keyed by platform id, with their flags, codes
-- and checks in STRICT tables; see docs/DATA-MODEL.md "Column forms".

DROP VIEW IF EXISTS live_rom_view;
DROP VIEW IF EXISTS rom_view;
DROP VIEW IF EXISTS title_view;
DROP VIEW IF EXISTS title_search_source;

DROP TRIGGER IF EXISTS roms_insert_groups;
DROP TRIGGER IF EXISTS roms_update_groups;
DROP TRIGGER IF EXISTS roms_delete_groups;
DROP TRIGGER IF EXISTS files_insert_groups;
DROP TRIGGER IF EXISTS files_update_groups;
DROP TRIGGER IF EXISTS files_delete_groups;
DROP TRIGGER IF EXISTS title_flags_insert_groups;
DROP TRIGGER IF EXISTS title_flags_update_groups;
DROP TRIGGER IF EXISTS title_flags_delete_groups;
DROP TRIGGER IF EXISTS title_regions_insert_groups;
DROP TRIGGER IF EXISTS title_regions_update_groups;
DROP TRIGGER IF EXISTS title_regions_delete_groups;
DROP TRIGGER IF EXISTS titles_group_root_insert;
DROP TRIGGER IF EXISTS titles_group_root_parent;
DROP TRIGGER IF EXISTS titles_insert_groups;
DROP TRIGGER IF EXISTS titles_update_groups;
DROP TRIGGER IF EXISTS titles_rename_groups;
DROP TRIGGER IF EXISTS titles_rename_search;
DROP TRIGGER IF EXISTS titles_delete_groups;
DROP TRIGGER IF EXISTS titles_name_fixed;

DROP INDEX IF EXISTS dat_versions_family;
DROP INDEX IF EXISTS titles_parent;
DROP INDEX IF EXISTS titles_platform_base;
DROP INDEX IF EXISTS titles_group;
DROP INDEX IF EXISTS titles_source;
DROP INDEX IF EXISTS titles_mra_path;
DROP INDEX IF EXISTS titles_id_dat;
DROP INDEX IF EXISTS titles_group_root;
DROP INDEX IF EXISTS title_groups_name;
DROP INDEX IF EXISTS title_groups_have;
DROP INDEX IF EXISTS title_groups_recent;
DROP INDEX IF EXISTS title_groups_split;
DROP INDEX IF EXISTS title_groups_counts;

-- An unbound version keeps a NULL platform; a source text or code outside its table gives -1,
-- which the CHECK refuses, so the file rolls back.
CREATE TABLE dat_versions_new (
  id            INTEGER PRIMARY KEY,
  platform      INTEGER REFERENCES platforms(id),
  dat_name      TEXT NOT NULL,
  version       TEXT NOT NULL,
  source_file   TEXT NOT NULL,
  loaded_at     INTEGER NOT NULL,
  superseded_by INTEGER REFERENCES dat_versions(id),
  game_count    INTEGER NOT NULL CHECK (game_count >= 0),
  retired       INTEGER NOT NULL DEFAULT 0 CHECK (retired IN (0, 1)),
  source        INTEGER NOT NULL DEFAULT 0 CHECK (source IN (0, 1)),
  family        TEXT NOT NULL,
  UNIQUE (dat_name, version)
) STRICT;

INSERT INTO dat_versions_new (id, platform, dat_name, version, source_file, loaded_at,
                              superseded_by, game_count, retired, source, family)
SELECT d.id,
       CASE WHEN d.platform_id IS NULL THEN NULL
            ELSE COALESCE((SELECT p.id FROM platforms p WHERE p.slug = d.platform_id), -1) END,
       d.dat_name, d.version, d.source_file, d.loaded_at, d.superseded_by, d.game_count,
       d.retired,
       CASE d.source WHEN 'dat' THEN 0 WHEN 'mra' THEN 1 ELSE -1 END,
       d.family
FROM dat_versions d ORDER BY d.id;

DROP TABLE dat_versions;
ALTER TABLE dat_versions_new RENAME TO dat_versions;
CREATE INDEX dat_versions_family ON dat_versions(family, platform);

-- Text columns keep their values as stored; mra_check is NULL until the check has run.
CREATE TABLE titles_new (
  id             INTEGER PRIMARY KEY,
  platform       INTEGER NOT NULL REFERENCES platforms(id),
  dat_version_id INTEGER NOT NULL REFERENCES dat_versions(id),
  name           TEXT NOT NULL,
  base_name      TEXT NOT NULL,
  parent_id      INTEGER REFERENCES titles(id),
  revision       TEXT,
  is_1g1r_pick   INTEGER NOT NULL DEFAULT 0 CHECK (is_1g1r_pick IN (0, 1)),
  wanted         INTEGER NOT NULL DEFAULT 0 CHECK (wanted IN (0, 1)),
  retired        INTEGER NOT NULL DEFAULT 0 CHECK (retired IN (0, 1)),
  clone_of       TEXT,
  group_key      TEXT NOT NULL,
  inferred       INTEGER NOT NULL DEFAULT 0 CHECK (inferred IN (0, 1)),
  source         INTEGER NOT NULL DEFAULT 0 CHECK (source IN (0, 1)),
  setname        TEXT,
  rbf            TEXT,
  mra_path       TEXT,
  mra_check      INTEGER CHECK (mra_check BETWEEN 0 AND 3),
  mra_detail     TEXT,
  mra_stamp      TEXT,
  mra_file_stamp TEXT,
  mra_seen       INTEGER,
  group_root     INTEGER REFERENCES titles(id),
  UNIQUE (dat_version_id, name)
) STRICT;

INSERT INTO titles_new (id, platform, dat_version_id, name, base_name, parent_id, revision,
                        is_1g1r_pick, wanted, retired, clone_of, group_key, inferred, source,
                        setname, rbf, mra_path, mra_check, mra_detail, mra_stamp,
                        mra_file_stamp, mra_seen, group_root)
SELECT t.id, (SELECT p.id FROM platforms p WHERE p.slug = t.platform_id), t.dat_version_id,
       t.name, t.base_name, t.parent_id, t.revision, t.is_1g1r_pick, t.wanted, t.retired,
       t.clone_of, t.group_key, t.inferred,
       CASE t.source WHEN 'dat' THEN 0 WHEN 'mra' THEN 1 ELSE -1 END,
       t.setname, t.rbf, t.mra_path,
       CASE WHEN t.mra_check IS NULL THEN NULL
            WHEN t.mra_check = 'match' THEN 0 WHEN t.mra_check = 'refused' THEN 1
            WHEN t.mra_check = 'missing_part' THEN 2 WHEN t.mra_check = 'mismatch' THEN 3
            ELSE -1 END,
       t.mra_detail, t.mra_stamp, t.mra_file_stamp, t.mra_seen, t.group_root
FROM titles t ORDER BY t.id;

DROP TABLE titles;
ALTER TABLE titles_new RENAME TO titles;
CREATE INDEX titles_group ON titles(platform, inferred, group_key);
CREATE INDEX titles_source ON titles(platform, source);
CREATE INDEX titles_mra_path ON titles(platform, mra_path) WHERE source = 1;
CREATE INDEX titles_id_dat ON titles(id, dat_version_id);
CREATE INDEX titles_group_root ON titles(group_root);

-- The root title's name is read from titles, so the summary row carries none.
CREATE TABLE title_groups_new (
  parent_id         INTEGER NOT NULL,
  platform          INTEGER NOT NULL,
  base_name         TEXT NOT NULL,
  variants          INTEGER NOT NULL CHECK (variants >= 0),
  have_verified     INTEGER NOT NULL CHECK (have_verified >= 0),
  wanted            INTEGER NOT NULL CHECK (wanted >= 0),
  has_pick          INTEGER NOT NULL CHECK (has_pick IN (0, 1)),
  pick_id           INTEGER,
  newest_id         INTEGER NOT NULL,
  source            INTEGER NOT NULL CHECK (source IN (0, 1)),
  lean_flags        INTEGER NOT NULL,
  unflagged_regions INTEGER NOT NULL,
  flag_union        INTEGER NOT NULL,
  region_union      INTEGER NOT NULL,
  split             INTEGER NOT NULL CHECK (split IN (0, 1)),
  PRIMARY KEY (parent_id, platform)
) STRICT, WITHOUT ROWID;

INSERT INTO title_groups_new (parent_id, platform, base_name, variants, have_verified, wanted,
                              has_pick, pick_id, newest_id, source, lean_flags,
                              unflagged_regions, flag_union, region_union, split)
SELECT g.parent_id, (SELECT p.id FROM platforms p WHERE p.slug = g.platform_id), g.base_name,
       g.variants, g.have_verified, g.wanted, g.has_pick, g.pick_id, g.newest_id,
       CASE g.source WHEN 'dat' THEN 0 WHEN 'mra' THEN 1 ELSE -1 END,
       g.lean_flags, g.unflagged_regions, g.flag_union, g.region_union, g.split
FROM title_groups g ORDER BY g.parent_id, 2;

DROP TABLE title_groups;
ALTER TABLE title_groups_new RENAME TO title_groups;
CREATE INDEX title_groups_name ON title_groups(platform, base_name COLLATE NOCASE, parent_id);
CREATE INDEX title_groups_have
  ON title_groups(platform, (have_verified > 0) DESC, base_name COLLATE NOCASE, parent_id);
CREATE INDEX title_groups_recent ON title_groups(platform, newest_id DESC);
CREATE INDEX title_groups_split ON title_groups(platform, parent_id) WHERE split;
CREATE INDEX title_groups_counts
  ON title_groups(platform, lean_flags, source, have_verified, wanted);

CREATE TABLE title_groups_dirty_new (parent_id INTEGER PRIMARY KEY) STRICT;
INSERT INTO title_groups_dirty_new (parent_id) SELECT parent_id FROM title_groups_dirty ORDER BY parent_id;
DROP TABLE title_groups_dirty;
ALTER TABLE title_groups_dirty_new RENAME TO title_groups_dirty;

-- The index content keeps the slug between 0x1F sentinels, so it is not rebuilt.
CREATE VIEW title_search_source AS
  SELECT t.id, t.base_name,
         char(31) || (SELECT p.slug FROM platforms p WHERE p.id = t.platform) || char(31) AS platform
  FROM titles t;

CREATE TRIGGER titles_group_root_insert AFTER INSERT ON titles
BEGIN
  UPDATE titles SET group_root = NEW.parent_id WHERE id = NEW.id;
END;

CREATE TRIGGER titles_group_root_parent AFTER UPDATE OF parent_id ON titles
BEGIN
  UPDATE titles SET group_root = NEW.parent_id WHERE id = NEW.id;
END;

CREATE TRIGGER titles_insert_groups AFTER INSERT ON titles BEGIN
  INSERT INTO title_search (rowid, base_name, platform)
    VALUES (NEW.id, NEW.base_name,
            char(31) || (SELECT slug FROM platforms WHERE id = NEW.platform) || char(31));
  INSERT INTO title_groups_dirty (parent_id)
    SELECT DISTINCT x FROM (SELECT COALESCE(NEW.group_root, NEW.parent_id) AS x UNION SELECT NEW.id)
    WHERE x IS NOT NULL AND x NOT IN (SELECT parent_id FROM title_groups_dirty);
END;

CREATE TRIGGER titles_update_groups
AFTER UPDATE OF platform, group_root, retired, wanted, is_1g1r_pick, mra_check ON titles
WHEN OLD.platform IS NOT NEW.platform OR OLD.group_root IS NOT NEW.group_root
  OR OLD.retired IS NOT NEW.retired OR OLD.wanted IS NOT NEW.wanted
  OR OLD.is_1g1r_pick IS NOT NEW.is_1g1r_pick OR OLD.mra_check IS NOT NEW.mra_check BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT DISTINCT x FROM (SELECT OLD.group_root AS x UNION SELECT NEW.group_root)
    WHERE x IS NOT NULL AND x NOT IN (SELECT parent_id FROM title_groups_dirty);
END;

CREATE TRIGGER titles_rename_groups AFTER UPDATE OF base_name, source, platform ON titles
WHEN OLD.base_name IS NOT NEW.base_name OR OLD.source IS NOT NEW.source
  OR OLD.platform IS NOT NEW.platform BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT NEW.id WHERE NEW.id NOT IN (SELECT parent_id FROM title_groups_dirty);
END;

CREATE TRIGGER titles_rename_search AFTER UPDATE OF base_name, platform ON titles
WHEN OLD.base_name IS NOT NEW.base_name OR OLD.platform IS NOT NEW.platform BEGIN
  INSERT INTO title_search (title_search, rowid, base_name, platform)
    VALUES ('delete', OLD.id, OLD.base_name,
            char(31) || (SELECT slug FROM platforms WHERE id = OLD.platform) || char(31));
  INSERT INTO title_search (rowid, base_name, platform)
    VALUES (NEW.id, NEW.base_name,
            char(31) || (SELECT slug FROM platforms WHERE id = NEW.platform) || char(31));
END;

CREATE TRIGGER titles_delete_groups AFTER DELETE ON titles BEGIN
  INSERT INTO title_search (title_search, rowid, base_name, platform)
    VALUES ('delete', OLD.id, OLD.base_name,
            char(31) || (SELECT slug FROM platforms WHERE id = OLD.platform) || char(31));
  INSERT INTO title_groups_dirty (parent_id)
    SELECT DISTINCT x FROM (SELECT OLD.group_root AS x UNION SELECT OLD.id)
    WHERE x IS NOT NULL AND x NOT IN (SELECT parent_id FROM title_groups_dirty);
END;

CREATE TRIGGER title_flags_insert_groups AFTER INSERT ON title_flags BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT group_root FROM titles WHERE id = NEW.title_id AND group_root IS NOT NULL
      AND group_root NOT IN (SELECT parent_id FROM title_groups_dirty);
END;

CREATE TRIGGER title_flags_delete_groups AFTER DELETE ON title_flags BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT group_root FROM titles WHERE id = OLD.title_id AND group_root IS NOT NULL
      AND group_root NOT IN (SELECT parent_id FROM title_groups_dirty);
END;

CREATE TRIGGER title_flags_update_groups AFTER UPDATE ON title_flags BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT DISTINCT group_root FROM titles WHERE id IN (OLD.title_id, NEW.title_id)
      AND group_root IS NOT NULL AND group_root NOT IN (SELECT parent_id FROM title_groups_dirty);
END;

CREATE TRIGGER title_regions_insert_groups AFTER INSERT ON title_regions BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT group_root FROM titles WHERE id = NEW.title_id AND group_root IS NOT NULL
      AND group_root NOT IN (SELECT parent_id FROM title_groups_dirty);
END;

CREATE TRIGGER title_regions_delete_groups AFTER DELETE ON title_regions BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT group_root FROM titles WHERE id = OLD.title_id AND group_root IS NOT NULL
      AND group_root NOT IN (SELECT parent_id FROM title_groups_dirty);
END;

CREATE TRIGGER title_regions_update_groups AFTER UPDATE ON title_regions BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT DISTINCT group_root FROM titles WHERE id IN (OLD.title_id, NEW.title_id)
      AND group_root IS NOT NULL AND group_root NOT IN (SELECT parent_id FROM title_groups_dirty);
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

CREATE TRIGGER roms_delete_groups AFTER DELETE ON roms WHEN OLD.retired = 0 BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT group_root FROM titles WHERE id = OLD.title_id AND group_root IS NOT NULL
      AND group_root NOT IN (SELECT parent_id FROM title_groups_dirty);
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

CREATE TRIGGER files_delete_groups AFTER DELETE ON files
WHEN OLD.rom_id IS NOT NULL AND OLD.state = 1 BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT t.group_root FROM roms r JOIN titles t ON t.id = r.title_id
    WHERE r.id = OLD.rom_id AND t.group_root IS NOT NULL
      AND t.group_root NOT IN (SELECT parent_id FROM title_groups_dirty);
END;

CREATE TRIGGER titles_name_fixed BEFORE UPDATE OF name ON titles
WHEN NEW.name IS NOT OLD.name BEGIN
  SELECT RAISE(ABORT, 'titles.name never changes');
END;

CREATE VIEW rom_view AS
SELECT r.id, r.title_id,
       CASE WHEN r.own_name THEN r.name ELSE t.name || r.name END AS name, r.size, r.crc32, r.md5, r.sha1,
       NULLIF(lower(hex(r.crc32)), '') AS crc32_hex, NULLIF(lower(hex(r.md5)), '') AS md5_hex,
       NULLIF(lower(hex(r.sha1)), '') AS sha1_hex,
       r.status,
       CASE r.status WHEN 0 THEN 'good' WHEN 1 THEN 'baddump' WHEN 2 THEN 'nodump'
                     WHEN 3 THEN 'verified' END AS status_text,
       r.match_name, r.match_base, r.retired,
       r.header, r.zip_dir, r.present,
       t.name AS title_name, t.dat_version_id, t.source AS title_source,
       t.platform AS platform_key,
       (SELECT p.slug FROM platforms p WHERE p.id = t.platform) AS platform,
       t.group_root, t.parent_id,
       t.retired AS title_retired,
       (r.retired = 0 AND t.retired = 0) AS live,
       EXISTS (SELECT 1 FROM title_flags f WHERE f.title_id = r.title_id AND f.flag = 'bios') AS bios
FROM roms r JOIN titles t ON t.id = r.title_id;

CREATE VIEW live_rom_view AS
SELECT * FROM rom_view WHERE live AND NOT bios;

CREATE VIEW title_view AS
SELECT t.id, t.platform AS platform_key,
       (SELECT p.slug FROM platforms p WHERE p.id = t.platform) AS platform, t.dat_version_id,
       t.name, t.base_name, t.parent_id, t.group_root, t.revision, t.is_1g1r_pick, t.wanted,
       t.retired, t.clone_of, t.group_key, t.inferred, t.source,
       CASE t.source WHEN 0 THEN 'dat' WHEN 1 THEN 'mra' END AS source_text,
       t.setname, t.rbf, t.mra_path, t.mra_check,
       CASE t.mra_check WHEN 0 THEN 'match' WHEN 1 THEN 'refused' WHEN 2 THEN 'missing_part'
                        WHEN 3 THEN 'mismatch' END AS mra_check_text,
       t.mra_detail, t.mra_stamp, t.mra_file_stamp, t.mra_seen
FROM titles t;
