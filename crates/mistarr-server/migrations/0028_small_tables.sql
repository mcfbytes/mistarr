-- Small tables: every enumeration becomes its integer code and every table is STRICT,
-- so a wrong type is refused on write. The API still reads the words.
-- Nothing references these three tables, so the rebuilds need no `foreign_keys: off`.

-- Jobs are transient, so rows whose kind, state or lane is outside the value table go first.
DROP INDEX jobs_subject;
DROP INDEX jobs_state;
DELETE FROM jobs
 WHERE kind NOT IN ('scan', 'import', 'detect_client', 'dat_import', 'recompute_1g1r',
                    'source_import', 'resolve_magnet', 'transfer', 'deselect', 'arcade_catalog',
                    'chd_tracks', 'remap_sources', 'bind_source', 'url_fetch')
    OR state NOT IN ('queued', 'running', 'paused', 'done', 'failed')
    OR lane NOT IN ('heavy', 'background', 'light', 'fetch');
CREATE TABLE jobs_new (
  id         INTEGER PRIMARY KEY,
  kind       INTEGER NOT NULL CHECK (kind BETWEEN 0 AND 13),
  payload    TEXT NOT NULL,
  state      INTEGER NOT NULL CHECK (state BETWEEN 0 AND 4),
  progress   TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  lane       INTEGER NOT NULL DEFAULT 2 CHECK (lane BETWEEN 0 AND 3),
  subject    TEXT NOT NULL
) STRICT;
INSERT INTO jobs_new (id, kind, payload, state, progress, created_at, updated_at, lane, subject)
SELECT id,
       CASE kind WHEN 'scan' THEN 0 WHEN 'import' THEN 1 WHEN 'detect_client' THEN 2
                 WHEN 'dat_import' THEN 3 WHEN 'recompute_1g1r' THEN 4
                 WHEN 'source_import' THEN 5 WHEN 'resolve_magnet' THEN 6
                 WHEN 'transfer' THEN 7 WHEN 'deselect' THEN 8 WHEN 'arcade_catalog' THEN 9
                 WHEN 'chd_tracks' THEN 10 WHEN 'remap_sources' THEN 11
                 WHEN 'bind_source' THEN 12 WHEN 'url_fetch' THEN 13 END,
       payload,
       CASE state WHEN 'queued' THEN 0 WHEN 'running' THEN 1 WHEN 'paused' THEN 2
                  WHEN 'done' THEN 3 WHEN 'failed' THEN 4 END,
       progress, created_at, updated_at,
       CASE lane WHEN 'heavy' THEN 0 WHEN 'background' THEN 1 WHEN 'light' THEN 2
                 WHEN 'fetch' THEN 3 END,
       subject
FROM jobs ORDER BY id;
DROP TABLE jobs;
ALTER TABLE jobs_new RENAME TO jobs;
CREATE INDEX jobs_subject ON jobs(kind, subject, state);
CREATE INDEX jobs_state ON jobs(state, id);

-- An action outside the value table fails the NOT NULL and rolls the file back.
DROP INDEX import_log_file;
CREATE TABLE import_log_new (
  id          INTEGER PRIMARY KEY,
  at          INTEGER NOT NULL,
  download_id INTEGER REFERENCES downloads(id),
  file_id     INTEGER REFERENCES files(id),
  action      INTEGER NOT NULL CHECK (action BETWEEN 0 AND 4),
  detail      TEXT NOT NULL
) STRICT;
INSERT INTO import_log_new (id, at, download_id, file_id, action, detail)
SELECT id, at, download_id, file_id,
       CASE action WHEN 'placed' THEN 0 WHEN 'replaced' THEN 1 WHEN 'quarantined' THEN 2
                   WHEN 'skipped_existing' THEN 3 WHEN 'renamed' THEN 4 END,
       detail
FROM import_log ORDER BY id;
DROP TABLE import_log;
ALTER TABLE import_log_new RENAME TO import_log;
CREATE INDEX import_log_file ON import_log(file_id);

CREATE TABLE settings_new (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
) STRICT, WITHOUT ROWID;
INSERT INTO settings_new (key, value) SELECT key, value FROM settings ORDER BY key;
DROP TABLE settings;
ALTER TABLE settings_new RENAME TO settings;
