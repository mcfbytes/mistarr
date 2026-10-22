CREATE VIEW torrent_file_view AS
SELECT f.source_id, f.file_index,
       (SELECT d.path FROM torrent_dirs d WHERE d.id = f.dir_id) || f.leaf AS path,
       f.size, f.rom_id, f.confidence,
       CASE f.confidence WHEN 0 THEN 'hash' WHEN 1 THEN 'name' WHEN 2 THEN 'base' END
         AS confidence_text
FROM torrent_files f;
