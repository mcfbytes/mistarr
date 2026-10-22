CREATE TABLE "torrent_files" (
  source_id  INTEGER NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
  file_index INTEGER NOT NULL,
  dir_id     INTEGER NOT NULL REFERENCES torrent_dirs(id),
  leaf       TEXT NOT NULL,
  size       INTEGER NOT NULL,
  rom_id     INTEGER REFERENCES roms(id),
  confidence INTEGER CHECK (confidence BETWEEN 0 AND 2),
  PRIMARY KEY (source_id, file_index)
) STRICT, WITHOUT ROWID;

CREATE INDEX torrent_files_rom ON torrent_files(rom_id);

CREATE INDEX torrent_files_source_rom ON torrent_files(source_id, rom_id);
