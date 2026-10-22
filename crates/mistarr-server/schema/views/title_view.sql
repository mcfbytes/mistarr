CREATE VIEW title_view AS
SELECT t.id, t.platform AS platform_key,
       (SELECT p.slug FROM platforms p WHERE p.id = t.platform) AS platform, t.dat_version_id,
       t.name, t.base_name, t.parent_id, t.group_root, t.revision, t.is_1g1r_pick, t.wanted,
       t.retired, t.clone_of, t.group_key, t.inferred, t.source,
       CASE t.source WHEN 0 THEN 'dat' WHEN 1 THEN 'mra' END AS source_text,
       t.setname, t.rbf, t.mra_path, t.mra_check,
       CASE t.mra_check WHEN 0 THEN 'match' WHEN 1 THEN 'refused' WHEN 2 THEN 'missing_part'
                        WHEN 3 THEN 'mismatch' END AS mra_check_text,
       t.mra_detail, t.mra_stamp, t.mra_file_stamp, t.mra_seen
FROM titles t;
