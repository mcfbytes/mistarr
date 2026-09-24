ALTER TABLE sources ADD COLUMN suggested_platform_id TEXT REFERENCES platforms(id);   -- guessed from the torrent's names, no DAT needed
