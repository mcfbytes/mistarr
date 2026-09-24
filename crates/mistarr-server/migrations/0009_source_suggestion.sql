ALTER TABLE sources ADD COLUMN suggested_platform_id TEXT REFERENCES platforms(id);   -- guessed from the torrent's names, no DAT needed
ALTER TABLE sources ADD COLUMN user_unbound INTEGER NOT NULL DEFAULT 0;   -- 1 after the user unbound it; never bound automatically again
