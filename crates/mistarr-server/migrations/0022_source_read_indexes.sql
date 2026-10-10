-- Source detail, the sources list and the platform counts read index pages only;
-- dropping first keeps every step re-runnable; see docs/DATA-MODEL.md "Indexes".

DROP INDEX IF EXISTS torrent_files_source_rom;
CREATE INDEX torrent_files_source_rom ON torrent_files(source_id, rom_id);
DROP INDEX IF EXISTS roms_id_title;
CREATE INDEX roms_id_title ON roms(id, title_id);
DROP INDEX IF EXISTS titles_id_dat;
CREATE INDEX titles_id_dat ON titles(id, dat_version_id);

DROP INDEX IF EXISTS roms_match_name;
CREATE INDEX roms_match_name ON roms(match_name, title_id);
DROP INDEX IF EXISTS roms_match_base;
CREATE INDEX roms_match_base ON roms(match_base, size, title_id) WHERE match_base IS NOT NULL;

DROP INDEX IF EXISTS title_groups_counts;
CREATE INDEX title_groups_counts
  ON title_groups(platform_id, lean_flags, source, have_verified, wanted);
