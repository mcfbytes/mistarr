-- Pruning a file clears the import_log entries pointing at it through this index.
CREATE INDEX import_log_file ON import_log(file_id);

-- Arcade rows are matched to zips on disk ignoring ASCII case, as exFAT names them.
CREATE INDEX files_rel_lower ON files(platform_id, lower(rel_path));

-- Presence is one row per zip: drop arcade member rows (`.zip#` in any ASCII case)
-- read from a central directory, which carry no md5, unlike every import row.
UPDATE import_log SET file_id = NULL
WHERE file_id IN (SELECT id FROM files
                  WHERE platform_id = 'arcade' AND rel_path LIKE '%.zip#%' AND md5 IS NULL);
DELETE FROM files WHERE platform_id = 'arcade' AND rel_path LIKE '%.zip#%' AND md5 IS NULL;
