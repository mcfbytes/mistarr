-- One definition per read shape, flattened into each query that reads it; see
-- docs/DATA-MODEL.md "Views". Later conversions change these bodies, not the reads.

DROP VIEW IF EXISTS rom_view;
CREATE VIEW rom_view AS
SELECT r.id, r.title_id, r.name, r.size, r.crc32, r.md5, r.sha1,
       r.crc32 AS crc32_hex, r.md5 AS md5_hex, r.sha1 AS sha1_hex,
       r.status, r.status AS status_text, r.match_name, r.match_base, r.retired,
       r.header, r.zip_dir, r.present,
       t.name AS title_name, t.dat_version_id, t.source AS title_source,
       t.platform_id AS platform_key, t.platform_id AS platform, t.group_root, t.parent_id,
       t.retired AS title_retired,
       (r.retired = 0 AND t.retired = 0) AS live,
       EXISTS (SELECT 1 FROM title_flags f WHERE f.title_id = r.title_id AND f.flag = 'bios') AS bios
FROM roms r JOIN titles t ON t.id = r.title_id;

DROP VIEW IF EXISTS live_rom_view;
CREATE VIEW live_rom_view AS
SELECT * FROM rom_view WHERE live AND NOT bios;

DROP VIEW IF EXISTS title_view;
CREATE VIEW title_view AS
SELECT t.id, t.platform_id AS platform_key, t.platform_id AS platform, t.dat_version_id,
       t.name, t.base_name, t.parent_id, t.group_root, t.revision, t.is_1g1r_pick, t.wanted,
       t.retired, t.clone_of, t.group_key, t.inferred, t.source, t.source AS source_text,
       t.setname, t.rbf, t.mra_path, t.mra_check, t.mra_check AS mra_check_text,
       t.mra_detail, t.mra_stamp, t.mra_file_stamp, t.mra_seen
FROM titles t;

DROP VIEW IF EXISTS torrent_file_view;
CREATE VIEW torrent_file_view AS
SELECT f.source_id, f.file_index, f.path, f.size, f.rom_id, f.confidence,
       f.confidence AS confidence_text
FROM torrent_files f;

DROP VIEW IF EXISTS file_view;
CREATE VIEW file_view AS
SELECT f.id, f.platform_id AS platform_key, f.platform_id AS platform, f.rel_path, f.size,
       f.mtime, f.crc32, f.md5, f.sha1, f.crc32 AS crc32_hex, f.md5 AS md5_hex,
       f.sha1 AS sha1_hex, f.header_rule, f.header_rule AS header_rule_text,
       f.rom_id, f.state, f.state AS state_text, f.scanned_at, f.reason,
       f.reason AS reason_text
FROM files f;
