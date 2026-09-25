-- Fewer index pages for a DAT load to write; see docs/DATA-MODEL.md "Indexes".

-- The stage is a TEMP table of the writer connection (src/db/dat_stage.rs).
DROP TABLE dat_stage;

-- The md5 tier matches only roms without a sha1.
DROP INDEX roms_md5;
CREATE INDEX roms_md5 ON roms(md5) WHERE sha1 IS NULL;

-- Binding reads size and base name only of roms it has keyed; a load inserts none.
DROP INDEX roms_match_base;
CREATE INDEX roms_match_base ON roms(match_base, size) WHERE match_base IS NOT NULL;
DROP INDEX roms_size;
CREATE INDEX roms_size ON roms(size) WHERE match_base IS NOT NULL;
