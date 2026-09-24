-- Candidate roms per torrent file from the fuzzy and size-only mapping tiers, and
-- the rom size index those tiers look up; base-name matches get their own label.
CREATE TABLE torrent_candidates (
  source_id  INTEGER NOT NULL,
  file_index INTEGER NOT NULL,
  rom_id     INTEGER NOT NULL REFERENCES roms(id),
  confidence TEXT NOT NULL,
  PRIMARY KEY (source_id, file_index, rom_id),
  FOREIGN KEY (source_id, file_index)
    REFERENCES torrent_files(source_id, file_index) ON DELETE CASCADE
) WITHOUT ROWID;
CREATE INDEX torrent_candidates_rom ON torrent_candidates(rom_id);
CREATE INDEX roms_size ON roms(size);
UPDATE torrent_files SET confidence = 'base' WHERE confidence = 'size';
