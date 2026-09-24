-- Pruning a file clears the import_log entries pointing at it through this index.
CREATE INDEX import_log_file ON import_log(file_id);
