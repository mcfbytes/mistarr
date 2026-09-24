ALTER TABLE titles ADD COLUMN mra_file_stamp TEXT;   -- MRA file size and mtime when the title was last stored from it
ALTER TABLE titles ADD COLUMN mra_seen INTEGER;      -- the arcade catalogue run that last found the MRA file
CREATE INDEX titles_mra_path ON titles(platform_id, mra_path) WHERE source = 'mra';
