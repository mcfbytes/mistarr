-- Source import: why a source is waiting or was not bound, match confidence,
-- and precomputed match keys on roms so binding is two indexed lookups.
ALTER TABLE sources ADD COLUMN reason TEXT;
ALTER TABLE torrent_files ADD COLUMN confidence TEXT;
ALTER TABLE roms ADD COLUMN match_name TEXT;
ALTER TABLE roms ADD COLUMN match_base TEXT;
CREATE INDEX roms_match_name ON roms(match_name);
CREATE INDEX roms_match_base ON roms(match_base, size);
CREATE INDEX sources_state ON sources(state);
