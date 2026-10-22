CREATE VIEW rom_view AS
SELECT r.id, r.title_id,
       CASE WHEN r.own_name THEN r.name ELSE t.name || r.name END AS name, r.size, r.crc32, r.md5, r.sha1,
       NULLIF(lower(hex(r.crc32)), '') AS crc32_hex, NULLIF(lower(hex(r.md5)), '') AS md5_hex,
       NULLIF(lower(hex(r.sha1)), '') AS sha1_hex,
       r.status,
       CASE r.status WHEN 0 THEN 'good' WHEN 1 THEN 'baddump' WHEN 2 THEN 'nodump'
                     WHEN 3 THEN 'verified' END AS status_text,
       r.match_name, r.match_base, r.retired,
       r.header, r.zip_dir, r.present,
       t.name AS title_name, t.dat_version_id, t.source AS title_source,
       t.platform AS platform_key,
       (SELECT p.slug FROM platforms p WHERE p.id = t.platform) AS platform,
       t.group_root, t.parent_id,
       t.retired AS title_retired,
       (r.retired = 0 AND t.retired = 0) AS live,
       EXISTS (SELECT 1 FROM title_flags f WHERE f.title_id = r.title_id AND f.tag_id = 4) AS bios
FROM roms r JOIN titles t ON t.id = r.title_id;
