CREATE VIEW file_view AS
SELECT f.id, f.platform AS platform_key,
       (SELECT p.slug FROM platforms p WHERE p.id = f.platform) AS platform, f.rel_path, f.size,
       f.mtime, f.crc32, f.md5, f.sha1,
       NULLIF(lower(hex(f.crc32)), '') AS crc32_hex, NULLIF(lower(hex(f.md5)), '') AS md5_hex,
       NULLIF(lower(hex(f.sha1)), '') AS sha1_hex,
       f.header_rule,
       CASE f.header_rule WHEN 0 THEN 'none' WHEN 1 THEN 'ines' WHEN 2 THEN 'smc'
                          WHEN 3 THEN 'a78' WHEN 4 THEN 'lnx' WHEN 5 THEN 'n64'
                          WHEN 6 THEN 'chd' END AS header_rule_text,
       f.rom_id, f.state,
       CASE f.state WHEN 0 THEN 'pending' WHEN 1 THEN 'verified' WHEN 2 THEN 'misnamed'
                    WHEN 3 THEN 'unverified' WHEN 4 THEN 'bad' WHEN 5 THEN 'unidentified'
                    END AS state_text,
       f.scanned_at, f.reason,
       CASE f.reason WHEN 0 THEN 'off' WHEN 1 THEN 'pending' WHEN 2 THEN 'no_layout'
                     WHEN 3 THEN 'unreadable' WHEN 4 THEN 'not_chd' WHEN 5 THEN 'version'
                     WHEN 6 THEN 'parent' WHEN 7 THEN 'not_cd' WHEN 8 THEN 'too_large'
                     WHEN 9 THEN 'codec' WHEN 10 THEN 'gdrom' WHEN 11 THEN 'old_layout'
                     WHEN 12 THEN 'cooked' WHEN 13 THEN 'pregap' WHEN 14 THEN 'corrupt'
                     WHEN 15 THEN 'checksum' WHEN 16 THEN 'no_checksum' END AS reason_text
FROM files f;
