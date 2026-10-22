CREATE TABLE "title_flags" (
  title_id INTEGER NOT NULL REFERENCES titles(id) ON DELETE CASCADE,
  pos      INTEGER NOT NULL CHECK (pos >= 0),
  tag_id   INTEGER NOT NULL REFERENCES tags(id),
  PRIMARY KEY (title_id, tag_id)
) STRICT, WITHOUT ROWID;

CREATE INDEX title_flags_tag ON title_flags(tag_id, title_id);

CREATE TRIGGER title_flags_delete_groups AFTER DELETE ON title_flags BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT group_root FROM titles WHERE id = OLD.title_id AND group_root IS NOT NULL
      AND group_root NOT IN (SELECT parent_id FROM title_groups_dirty);
END;

CREATE TRIGGER title_flags_insert_groups AFTER INSERT ON title_flags BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT group_root FROM titles WHERE id = NEW.title_id AND group_root IS NOT NULL
      AND group_root NOT IN (SELECT parent_id FROM title_groups_dirty);
END;

CREATE TRIGGER title_flags_update_groups AFTER UPDATE ON title_flags BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT DISTINCT group_root FROM titles WHERE id IN (OLD.title_id, NEW.title_id)
      AND group_root IS NOT NULL AND group_root NOT IN (SELECT parent_id FROM title_groups_dirty);
END;
