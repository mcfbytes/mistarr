CREATE TABLE "torrent_candidates" (
  source_id  INTEGER NOT NULL,
  file_index INTEGER NOT NULL,
  rom_id     INTEGER NOT NULL REFERENCES roms(id),
  confidence INTEGER NOT NULL CHECK (confidence BETWEEN 1 AND 4),
  PRIMARY KEY (source_id, file_index, rom_id),
  FOREIGN KEY (source_id, file_index)
    REFERENCES torrent_files(source_id, file_index) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

CREATE INDEX torrent_candidates_rom ON torrent_candidates(rom_id);
