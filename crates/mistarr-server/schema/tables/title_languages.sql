CREATE TABLE "title_languages" (
  title_id INTEGER NOT NULL REFERENCES titles(id) ON DELETE CASCADE,
  pos      INTEGER NOT NULL CHECK (pos >= 0),
  tag_id   INTEGER NOT NULL REFERENCES tags(id),
  PRIMARY KEY (title_id, tag_id)
) STRICT, WITHOUT ROWID;

CREATE INDEX title_languages_tag ON title_languages(tag_id, title_id);
