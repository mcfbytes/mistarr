-- foreign_keys: off
-- Flags, regions and languages as rows of one tags table; see docs/DATA-MODEL.md "Column forms".

DROP VIEW IF EXISTS live_rom_view;
DROP VIEW IF EXISTS rom_view;

DROP TRIGGER IF EXISTS title_flags_insert_groups;
DROP TRIGGER IF EXISTS title_flags_update_groups;
DROP TRIGGER IF EXISTS title_flags_delete_groups;
DROP TRIGGER IF EXISTS title_regions_insert_groups;
DROP TRIGGER IF EXISTS title_regions_update_groups;
DROP TRIGGER IF EXISTS title_regions_delete_groups;

CREATE TABLE tags (
  id   INTEGER PRIMARY KEY,
  kind INTEGER NOT NULL CHECK (kind BETWEEN 0 AND 2),
  name TEXT NOT NULL,
  bit  INTEGER CHECK (bit > 0),
  UNIQUE (kind, name)
) STRICT;
CREATE INDEX tags_kind_nocase ON tags(kind, name COLLATE NOCASE);

-- The known flags take ids 1 to 9 and the known regions 10 to 31, in bit order.
INSERT INTO tags (id, kind, name, bit)
SELECT row_number() OVER (ORDER BY bit), 0, name, bit FROM known_flags;
INSERT INTO tags (id, kind, name, bit)
SELECT 9 + row_number() OVER (ORDER BY bit), 1, name, bit FROM known_regions;

-- Any other text takes the next ids by kind then name; a spelling of a known region that differs
-- in case is a tag of its own with the known region's bit.
INSERT INTO tags (id, kind, name, bit)
SELECT 31 + row_number() OVER (ORDER BY kind, name), kind, name, bit FROM (
  SELECT 0 AS kind, flag AS name, NULL AS bit FROM title_flags
    WHERE flag COLLATE BINARY NOT IN (SELECT name COLLATE BINARY FROM known_flags)
  UNION
  SELECT 1, region, (SELECT k.bit FROM known_regions k WHERE k.name = region COLLATE NOCASE)
    FROM title_regions
    WHERE region COLLATE BINARY NOT IN (SELECT name COLLATE BINARY FROM known_regions)
  UNION
  SELECT 2, language, NULL FROM title_languages
) ORDER BY kind, name;

-- A title never lists a value twice (the old primary key), so each link keeps its position.
CREATE TABLE title_flags_new (
  title_id INTEGER NOT NULL REFERENCES titles(id) ON DELETE CASCADE,
  pos      INTEGER NOT NULL CHECK (pos >= 0),
  tag_id   INTEGER NOT NULL REFERENCES tags(id),
  PRIMARY KEY (title_id, tag_id)
) STRICT, WITHOUT ROWID;
INSERT INTO title_flags_new (title_id, pos, tag_id)
SELECT f.title_id, f.pos, t.id FROM title_flags f JOIN tags t ON t.kind = 0 AND t.name = f.flag
ORDER BY f.title_id, t.id;
DROP TABLE title_flags;
ALTER TABLE title_flags_new RENAME TO title_flags;
CREATE INDEX title_flags_tag ON title_flags(tag_id, title_id);

CREATE TABLE title_regions_new (
  title_id INTEGER NOT NULL REFERENCES titles(id) ON DELETE CASCADE,
  pos      INTEGER NOT NULL CHECK (pos >= 0),
  tag_id   INTEGER NOT NULL REFERENCES tags(id),
  PRIMARY KEY (title_id, tag_id)
) STRICT, WITHOUT ROWID;
INSERT INTO title_regions_new (title_id, pos, tag_id)
SELECT r.title_id, r.pos, t.id FROM title_regions r JOIN tags t ON t.kind = 1 AND t.name = r.region
ORDER BY r.title_id, t.id;
DROP TABLE title_regions;
ALTER TABLE title_regions_new RENAME TO title_regions;
CREATE INDEX title_regions_tag ON title_regions(tag_id, title_id);

CREATE TABLE title_languages_new (
  title_id INTEGER NOT NULL REFERENCES titles(id) ON DELETE CASCADE,
  pos      INTEGER NOT NULL CHECK (pos >= 0),
  tag_id   INTEGER NOT NULL REFERENCES tags(id),
  PRIMARY KEY (title_id, tag_id)
) STRICT, WITHOUT ROWID;
INSERT INTO title_languages_new (title_id, pos, tag_id)
SELECT l.title_id, l.pos, t.id FROM title_languages l JOIN tags t ON t.kind = 2 AND t.name = l.language
ORDER BY l.title_id, t.id;
DROP TABLE title_languages;
ALTER TABLE title_languages_new RENAME TO title_languages;
CREATE INDEX title_languages_tag ON title_languages(tag_id, title_id);

DROP TABLE known_flags;
DROP TABLE known_regions;

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
       EXISTS (SELECT 1 FROM title_flags f WHERE f.title_id = r.title_id AND f.tag_id = 4) AS bios
FROM roms r JOIN titles t ON t.id = r.title_id;

CREATE VIEW live_rom_view AS
SELECT * FROM rom_view WHERE live AND NOT bios;
