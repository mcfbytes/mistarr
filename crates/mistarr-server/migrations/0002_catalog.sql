ALTER TABLE dat_versions ADD COLUMN retired INTEGER NOT NULL DEFAULT 0;

ALTER TABLE titles ADD COLUMN clone_of TEXT;
ALTER TABLE titles ADD COLUMN group_key TEXT NOT NULL DEFAULT '';
ALTER TABLE titles ADD COLUMN inferred INTEGER NOT NULL DEFAULT 0;
CREATE INDEX titles_parent ON titles(parent_id);
CREATE INDEX titles_group ON titles(platform_id, inferred, group_key);

ALTER TABLE roms ADD COLUMN retired INTEGER NOT NULL DEFAULT 0;

CREATE INDEX IF NOT EXISTS files_rom ON files(rom_id);

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
    SELECT t.platform_id, t.parent_id, t.id, t.wanted, t.is_1g1r_pick,
           COUNT(DISTINCT r.id) AS roms,
           COUNT(DISTINCT CASE WHEN f.state = 'verified' THEN r.id END) AS roms_verified
    FROM titles t
    LEFT JOIN roms r ON r.title_id = t.id AND r.retired = 0
    LEFT JOIN files f ON f.rom_id = r.id
    WHERE t.retired = 0
    GROUP BY t.platform_id, t.parent_id, t.id
  ) v
  GROUP BY v.platform_id, v.parent_id
) g
JOIN titles p ON p.id = g.parent_id;
