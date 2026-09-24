ALTER TABLE dat_versions ADD COLUMN family TEXT NOT NULL DEFAULT '';   -- dat::family_key of dat_name, refreshed at startup
CREATE INDEX dat_versions_family ON dat_versions(family, platform_id);

ALTER TABLE titles ADD COLUMN group_root INTEGER REFERENCES titles(id);   -- effective clone group: parent_id, or a title of another DAT listing the same roms
UPDATE titles SET group_root = parent_id;
CREATE INDEX titles_group_root ON titles(group_root);
CREATE TRIGGER titles_group_root_insert AFTER INSERT ON titles
BEGIN
  UPDATE titles SET group_root = NEW.parent_id WHERE id = NEW.id;
END;
CREATE TRIGGER titles_group_root_parent AFTER UPDATE OF parent_id ON titles
BEGIN
  UPDATE titles SET group_root = NEW.parent_id WHERE id = NEW.id;
END;

DROP VIEW title_groups;
CREATE VIEW title_groups AS
SELECT g.parent_id, g.platform_id, p.base_name, p.name,
       g.variants, g.have_verified, g.wanted, g.has_pick, g.pick_id, g.newest_id
FROM (
  SELECT v.platform_id, v.parent_id,
         COUNT(*) AS variants,
         SUM(v.roms > 0 AND v.roms_verified = v.roms) AS have_verified,
         SUM(v.wanted) AS wanted,
         MAX(v.is_1g1r_pick) AS has_pick,
         MAX(CASE WHEN v.is_1g1r_pick = 1 THEN v.id END) AS pick_id,
         MAX(v.id) AS newest_id
  FROM (
    SELECT t.platform_id, t.group_root AS parent_id, t.id, t.wanted, t.is_1g1r_pick,
           COUNT(DISTINCT r.id) AS roms,
           COUNT(DISTINCT CASE WHEN f.state = 'verified'
                                 OR (r.present = 1
                                     AND COALESCE(t.mra_check, '') NOT IN ('mismatch', 'missing_part'))
                               THEN r.id END) AS roms_verified
    FROM titles t
    LEFT JOIN roms r ON r.title_id = t.id AND r.retired = 0
    LEFT JOIN files f ON f.rom_id = r.id
    WHERE t.retired = 0
    GROUP BY t.platform_id, t.group_root, t.id
  ) v
  GROUP BY v.platform_id, v.parent_id
) g
JOIN titles p ON p.id = g.parent_id;
