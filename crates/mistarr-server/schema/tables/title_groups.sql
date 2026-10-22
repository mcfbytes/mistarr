CREATE TABLE "title_groups" (
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

CREATE INDEX title_groups_counts
  ON title_groups(platform, lean_flags, source, have_verified, wanted);

CREATE INDEX title_groups_have
  ON title_groups(platform, (have_verified > 0) DESC, base_name COLLATE NOCASE, parent_id);

CREATE INDEX title_groups_name ON title_groups(platform, base_name COLLATE NOCASE, parent_id);

CREATE INDEX title_groups_recent ON title_groups(platform, newest_id DESC);

CREATE INDEX title_groups_split ON title_groups(platform, parent_id) WHERE split;
