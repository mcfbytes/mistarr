-- Browse reads maintained, indexed tables instead of aggregating titles or parsing JSON
-- per request; see docs/DATA-MODEL.md "Derived tables" and src/db/groups.rs.

CREATE TABLE title_flags (
  title_id INTEGER NOT NULL REFERENCES titles(id) ON DELETE CASCADE,
  pos      INTEGER NOT NULL,   -- order within the title's list
  flag     TEXT NOT NULL,
  PRIMARY KEY (title_id, flag)
) WITHOUT ROWID;
CREATE INDEX title_flags_flag ON title_flags(flag, title_id);

CREATE TABLE title_regions (
  title_id INTEGER NOT NULL REFERENCES titles(id) ON DELETE CASCADE,
  pos      INTEGER NOT NULL,
  region   TEXT NOT NULL,
  PRIMARY KEY (title_id, region)
) WITHOUT ROWID;
CREATE INDEX title_regions_region ON title_regions(region COLLATE NOCASE, title_id);

CREATE TABLE title_languages (
  title_id INTEGER NOT NULL REFERENCES titles(id) ON DELETE CASCADE,
  pos      INTEGER NOT NULL,
  language TEXT NOT NULL,
  PRIMARY KEY (title_id, language)
) WITHOUT ROWID;
CREATE INDEX title_languages_language ON title_languages(language, title_id);

INSERT OR IGNORE INTO title_flags (title_id, pos, flag)
  SELECT t.id, j.key, j.value FROM titles t, json_each(t.flags) j
  WHERE json_valid(t.flags) AND j.type = 'text';
INSERT OR IGNORE INTO title_regions (title_id, pos, region)
  SELECT t.id, j.key, j.value FROM titles t, json_each(t.regions) j
  WHERE json_valid(t.regions) AND j.type = 'text';
INSERT OR IGNORE INTO title_languages (title_id, pos, language)
  SELECT t.id, j.key, j.value FROM titles t, json_each(t.languages) j
  WHERE json_valid(t.languages) AND j.type = 'text';

DROP VIEW title_groups;
ALTER TABLE titles DROP COLUMN flags;
ALTER TABLE titles DROP COLUMN regions;
ALTER TABLE titles DROP COLUMN languages;

-- Bit per known flag and region in the group summary; anything else counts as bit 62.
-- The flags hidden by default take the high bits, so MIN over a group's variants finds one without them.
CREATE TABLE known_flags (name TEXT PRIMARY KEY, bit INTEGER NOT NULL) WITHOUT ROWID;
INSERT INTO known_flags (name, bit) VALUES
  ('unl', 1), ('pirate', 2), ('baddump', 4), ('bios', 8), ('beta', 16),
  ('proto', 32), ('demo', 64), ('sample', 128), ('program', 256);

CREATE TABLE known_regions (name TEXT PRIMARY KEY COLLATE NOCASE, bit INTEGER NOT NULL) WITHOUT ROWID;
INSERT INTO known_regions (name, bit) VALUES
  ('World', 1), ('USA', 2), ('Europe', 4), ('Japan', 8), ('Asia', 16), ('Australia', 32),
  ('Brazil', 64), ('Canada', 128), ('China', 256), ('France', 512), ('Germany', 1024),
  ('Hong Kong', 2048), ('Italy', 4096), ('Korea', 8192), ('Netherlands', 16384),
  ('Russia', 32768), ('Scandinavia', 65536), ('Spain', 131072), ('Sweden', 262144),
  ('Taiwan', 524288), ('UK', 1048576), ('Latin America', 2097152);

CREATE TABLE title_groups (
  parent_id         INTEGER NOT NULL,
  platform_id       TEXT NOT NULL,
  base_name         TEXT NOT NULL,     -- the parent's
  name              TEXT NOT NULL,     -- the parent's
  variants          INTEGER NOT NULL,
  have_verified     INTEGER NOT NULL,
  wanted            INTEGER NOT NULL,
  has_pick          INTEGER NOT NULL,
  pick_id           INTEGER,
  newest_id         INTEGER NOT NULL,
  source            TEXT NOT NULL,     -- the parent's: 'dat' | 'mra'
  lean_flags        INTEGER NOT NULL,  -- least known-flag bits of a live variant of the parent
  unflagged_regions INTEGER NOT NULL,  -- region bits of those variants
  flag_union        INTEGER NOT NULL,  -- flag bits of every live variant of the parent
  region_union      INTEGER NOT NULL,  -- region bits of every live variant of the parent
  split             INTEGER NOT NULL,  -- the parent title is on another platform than the group
  PRIMARY KEY (parent_id, platform_id)
) WITHOUT ROWID;
CREATE INDEX title_groups_name ON title_groups(platform_id, base_name COLLATE NOCASE, parent_id);
CREATE INDEX title_groups_have ON title_groups(platform_id, (have_verified > 0) DESC, base_name COLLATE NOCASE, parent_id);
CREATE INDEX title_groups_recent ON title_groups(platform_id, newest_id DESC);
CREATE INDEX title_groups_split ON title_groups(platform_id, parent_id) WHERE split;

-- Groups whose inputs changed in the open transaction; db::commit refreshes and empties it.
-- The triggers test membership rather than use OR IGNORE, which an upsert overrides.
CREATE TABLE title_groups_dirty (parent_id INTEGER PRIMARY KEY);
INSERT INTO title_groups_dirty (parent_id)
  SELECT DISTINCT parent_id FROM titles WHERE parent_id IS NOT NULL;

-- Substring search over base names; trigram keeps LIKE's "contains" and the triggers keep it in step.
-- `platform` is the id between 0x1F sentinels, so one MATCH filters by platform without substrings.
CREATE VIEW title_search_source AS
  SELECT id, base_name, char(31) || platform_id || char(31) AS platform FROM titles;
CREATE VIRTUAL TABLE title_search USING fts5(
  base_name, platform, content = 'title_search_source', content_rowid = 'id', tokenize = 'trigram'
);
INSERT INTO title_search (title_search) VALUES ('rebuild');

CREATE TRIGGER titles_insert_groups AFTER INSERT ON titles BEGIN
  INSERT INTO title_search (rowid, base_name, platform)
    VALUES (NEW.id, NEW.base_name, char(31) || NEW.platform_id || char(31));
  INSERT INTO title_groups_dirty (parent_id)
    SELECT DISTINCT x FROM (SELECT NEW.parent_id AS x UNION SELECT NEW.id)
    WHERE x IS NOT NULL AND x NOT IN (SELECT parent_id FROM title_groups_dirty);
END;

CREATE TRIGGER titles_update_groups
AFTER UPDATE OF platform_id, parent_id, retired, wanted, is_1g1r_pick, mra_check ON titles
WHEN OLD.platform_id IS NOT NEW.platform_id OR OLD.parent_id IS NOT NEW.parent_id
  OR OLD.retired IS NOT NEW.retired OR OLD.wanted IS NOT NEW.wanted
  OR OLD.is_1g1r_pick IS NOT NEW.is_1g1r_pick OR OLD.mra_check IS NOT NEW.mra_check BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT DISTINCT x FROM (SELECT OLD.parent_id AS x UNION SELECT NEW.parent_id)
    WHERE x IS NOT NULL AND x NOT IN (SELECT parent_id FROM title_groups_dirty);
END;

-- A title's own name, source and platform show in the rows of the groups it roots.
CREATE TRIGGER titles_rename_groups AFTER UPDATE OF name, base_name, source, platform_id ON titles
WHEN OLD.name IS NOT NEW.name OR OLD.base_name IS NOT NEW.base_name OR OLD.source IS NOT NEW.source
  OR OLD.platform_id IS NOT NEW.platform_id BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT NEW.id WHERE NEW.id NOT IN (SELECT parent_id FROM title_groups_dirty);
END;

CREATE TRIGGER titles_rename_search AFTER UPDATE OF base_name, platform_id ON titles
WHEN OLD.base_name IS NOT NEW.base_name OR OLD.platform_id IS NOT NEW.platform_id BEGIN
  INSERT INTO title_search (title_search, rowid, base_name, platform)
    VALUES ('delete', OLD.id, OLD.base_name, char(31) || OLD.platform_id || char(31));
  INSERT INTO title_search (rowid, base_name, platform)
    VALUES (NEW.id, NEW.base_name, char(31) || NEW.platform_id || char(31));
END;

CREATE TRIGGER titles_delete_groups AFTER DELETE ON titles BEGIN
  INSERT INTO title_search (title_search, rowid, base_name, platform)
    VALUES ('delete', OLD.id, OLD.base_name, char(31) || OLD.platform_id || char(31));
  INSERT INTO title_groups_dirty (parent_id)
    SELECT DISTINCT x FROM (SELECT OLD.parent_id AS x UNION SELECT OLD.id)
    WHERE x IS NOT NULL AND x NOT IN (SELECT parent_id FROM title_groups_dirty);
END;

CREATE TRIGGER title_flags_insert_groups AFTER INSERT ON title_flags BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT parent_id FROM titles WHERE id = NEW.title_id AND parent_id IS NOT NULL
      AND parent_id NOT IN (SELECT parent_id FROM title_groups_dirty);
END;

CREATE TRIGGER title_flags_delete_groups AFTER DELETE ON title_flags BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT parent_id FROM titles WHERE id = OLD.title_id AND parent_id IS NOT NULL
      AND parent_id NOT IN (SELECT parent_id FROM title_groups_dirty);
END;

CREATE TRIGGER title_flags_update_groups AFTER UPDATE ON title_flags BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT DISTINCT parent_id FROM titles WHERE id IN (OLD.title_id, NEW.title_id)
      AND parent_id IS NOT NULL AND parent_id NOT IN (SELECT parent_id FROM title_groups_dirty);
END;

CREATE TRIGGER title_regions_insert_groups AFTER INSERT ON title_regions BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT parent_id FROM titles WHERE id = NEW.title_id AND parent_id IS NOT NULL
      AND parent_id NOT IN (SELECT parent_id FROM title_groups_dirty);
END;

CREATE TRIGGER title_regions_delete_groups AFTER DELETE ON title_regions BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT parent_id FROM titles WHERE id = OLD.title_id AND parent_id IS NOT NULL
      AND parent_id NOT IN (SELECT parent_id FROM title_groups_dirty);
END;

CREATE TRIGGER title_regions_update_groups AFTER UPDATE ON title_regions BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT DISTINCT parent_id FROM titles WHERE id IN (OLD.title_id, NEW.title_id)
      AND parent_id IS NOT NULL AND parent_id NOT IN (SELECT parent_id FROM title_groups_dirty);
END;

CREATE TRIGGER roms_insert_groups AFTER INSERT ON roms WHEN NEW.retired = 0 BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT parent_id FROM titles WHERE id = NEW.title_id AND parent_id IS NOT NULL
      AND parent_id NOT IN (SELECT parent_id FROM title_groups_dirty);
END;

CREATE TRIGGER roms_update_groups AFTER UPDATE OF title_id, retired, present ON roms
WHEN (OLD.retired = 0 OR NEW.retired = 0) AND (OLD.title_id IS NOT NEW.title_id
  OR OLD.retired IS NOT NEW.retired OR OLD.present IS NOT NEW.present) BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT DISTINCT parent_id FROM titles WHERE id IN (OLD.title_id, NEW.title_id)
      AND parent_id IS NOT NULL AND parent_id NOT IN (SELECT parent_id FROM title_groups_dirty);
END;

CREATE TRIGGER roms_delete_groups AFTER DELETE ON roms WHEN OLD.retired = 0 BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT parent_id FROM titles WHERE id = OLD.title_id AND parent_id IS NOT NULL
      AND parent_id NOT IN (SELECT parent_id FROM title_groups_dirty);
END;

CREATE TRIGGER files_insert_groups AFTER INSERT ON files
WHEN NEW.rom_id IS NOT NULL AND NEW.state = 'verified' BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT t.parent_id FROM roms r JOIN titles t ON t.id = r.title_id
    WHERE r.id = NEW.rom_id AND t.parent_id IS NOT NULL
      AND t.parent_id NOT IN (SELECT parent_id FROM title_groups_dirty);
END;

CREATE TRIGGER files_update_groups AFTER UPDATE OF rom_id, state ON files
WHEN (OLD.state = 'verified' OR NEW.state = 'verified')
  AND (OLD.rom_id IS NOT NEW.rom_id OR OLD.state IS NOT NEW.state) BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT DISTINCT t.parent_id FROM roms r JOIN titles t ON t.id = r.title_id
    WHERE r.id IN (OLD.rom_id, NEW.rom_id) AND t.parent_id IS NOT NULL
      AND t.parent_id NOT IN (SELECT parent_id FROM title_groups_dirty);
END;

CREATE TRIGGER files_delete_groups AFTER DELETE ON files
WHEN OLD.rom_id IS NOT NULL AND OLD.state = 'verified' BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT t.parent_id FROM roms r JOIN titles t ON t.id = r.title_id
    WHERE r.id = OLD.rom_id AND t.parent_id IS NOT NULL
      AND t.parent_id NOT IN (SELECT parent_id FROM title_groups_dirty);
END;

-- Jobs deduplicate on a normalised key of their payload's fields, never on the JSON text.
ALTER TABLE jobs ADD COLUMN subject TEXT NOT NULL DEFAULT '';
UPDATE jobs SET subject = COALESCE((
  SELECT group_concat(key || char(31) || CASE type
           WHEN 'true' THEN 'true' WHEN 'false' THEN 'false' WHEN 'null' THEN 'null'
           ELSE CAST(atom AS TEXT) END, char(30) ORDER BY key)
  FROM json_each(jobs.payload)), '')
WHERE json_valid(payload) AND json_type(payload) = 'object';
CREATE INDEX jobs_subject ON jobs(kind, subject, state);
CREATE INDEX jobs_state ON jobs(state, id);

-- Hot reads that walked whole tables: per-platform unverified counts and the downloads page.
CREATE INDEX files_state ON files(state, platform_id);
CREATE INDEX downloads_recent ON downloads(updated_at DESC, id DESC);
