-- Pruning a file clears the import_log entries pointing at it through this index.
CREATE INDEX import_log_file ON import_log(file_id);

-- Arcade rows are matched to zips on disk ignoring ASCII case, as exFAT names them.
CREATE INDEX files_rel_lower ON files(platform_id, lower(rel_path));

-- Arcade member rows read from a central directory carry no md5; import rows always do.
-- Presence is one row per zip, so the md5-less member rows are removed.
UPDATE import_log SET file_id = NULL
WHERE file_id IN (SELECT id FROM files
                  WHERE platform_id = 'arcade' AND rel_path LIKE '%#%' AND md5 IS NULL);
DELETE FROM files WHERE platform_id = 'arcade' AND rel_path LIKE '%#%' AND md5 IS NULL;
