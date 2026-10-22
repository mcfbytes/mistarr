CREATE TABLE "import_log" (
  id          INTEGER PRIMARY KEY,
  at          INTEGER NOT NULL,
  download_id INTEGER REFERENCES downloads(id),
  file_id     INTEGER REFERENCES files(id),
  action      INTEGER NOT NULL CHECK (action BETWEEN 0 AND 4),
  detail      TEXT NOT NULL
) STRICT;

CREATE INDEX import_log_file ON import_log(file_id);
