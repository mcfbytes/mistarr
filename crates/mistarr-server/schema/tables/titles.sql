CREATE TABLE "titles" (
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

CREATE INDEX titles_group ON titles(platform, inferred, group_key);

CREATE INDEX titles_group_root ON titles(group_root);

CREATE INDEX titles_id_dat ON titles(id, dat_version_id);

CREATE INDEX titles_mra_path ON titles(platform, mra_path) WHERE source = 1;

CREATE INDEX titles_source ON titles(platform, source);

CREATE TRIGGER titles_delete_groups AFTER DELETE ON titles BEGIN
  INSERT INTO title_search (title_search, rowid, base_name, platform)
    VALUES ('delete', OLD.id, OLD.base_name,
            char(31) || (SELECT slug FROM platforms WHERE id = OLD.platform) || char(31));
  INSERT INTO title_groups_dirty (parent_id)
    SELECT DISTINCT x FROM (SELECT OLD.group_root AS x UNION SELECT OLD.id)
    WHERE x IS NOT NULL AND x NOT IN (SELECT parent_id FROM title_groups_dirty);
END;

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

CREATE TRIGGER titles_name_fixed BEFORE UPDATE OF name ON titles
WHEN NEW.name IS NOT OLD.name BEGIN
  SELECT RAISE(ABORT, 'titles.name never changes');
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

CREATE TRIGGER titles_update_groups
AFTER UPDATE OF platform, group_root, retired, wanted, is_1g1r_pick, mra_check ON titles
WHEN OLD.platform IS NOT NEW.platform OR OLD.group_root IS NOT NEW.group_root
  OR OLD.retired IS NOT NEW.retired OR OLD.wanted IS NOT NEW.wanted
  OR OLD.is_1g1r_pick IS NOT NEW.is_1g1r_pick OR OLD.mra_check IS NOT NEW.mra_check BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT DISTINCT x FROM (SELECT OLD.group_root AS x UNION SELECT NEW.group_root)
    WHERE x IS NOT NULL AND x NOT IN (SELECT parent_id FROM title_groups_dirty);
END;
