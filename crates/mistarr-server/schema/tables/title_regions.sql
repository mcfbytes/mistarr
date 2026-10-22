CREATE TABLE "title_regions" (
  title_id INTEGER NOT NULL REFERENCES titles(id) ON DELETE CASCADE,
  pos      INTEGER NOT NULL CHECK (pos >= 0),
  tag_id   INTEGER NOT NULL REFERENCES tags(id),
  PRIMARY KEY (title_id, tag_id)
) STRICT, WITHOUT ROWID;

CREATE INDEX title_regions_tag ON title_regions(tag_id, title_id);

CREATE TRIGGER title_regions_delete_groups AFTER DELETE ON title_regions BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT group_root FROM titles WHERE id = OLD.title_id AND group_root IS NOT NULL
      AND group_root NOT IN (SELECT parent_id FROM title_groups_dirty);
END;

CREATE TRIGGER title_regions_insert_groups AFTER INSERT ON title_regions BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT group_root FROM titles WHERE id = NEW.title_id AND group_root IS NOT NULL
      AND group_root NOT IN (SELECT parent_id FROM title_groups_dirty);
END;

CREATE TRIGGER title_regions_update_groups AFTER UPDATE ON title_regions BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT DISTINCT group_root FROM titles WHERE id IN (OLD.title_id, NEW.title_id)
      AND group_root IS NOT NULL AND group_root NOT IN (SELECT parent_id FROM title_groups_dirty);
END;
