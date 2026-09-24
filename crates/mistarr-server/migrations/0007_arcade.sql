ALTER TABLE dat_versions ADD COLUMN source TEXT NOT NULL DEFAULT 'dat';   -- 'dat' | 'mra'

ALTER TABLE titles ADD COLUMN source TEXT NOT NULL DEFAULT 'dat';   -- 'dat' | 'mra': an MRA title is never retired by a DAT
ALTER TABLE titles ADD COLUMN setname TEXT;       -- MRA <setname>
ALTER TABLE titles ADD COLUMN rbf TEXT;           -- MRA <rbf>
ALTER TABLE titles ADD COLUMN mra_path TEXT;      -- MRA file relative to _Arcade
ALTER TABLE titles ADD COLUMN mra_check TEXT;     -- md5 check: 'match' | 'mismatch' | 'missing_part' | 'refused', NULL when not run
ALTER TABLE titles ADD COLUMN mra_detail TEXT;    -- why the check did not match
ALTER TABLE titles ADD COLUMN mra_stamp TEXT;     -- MRA and zip sizes and mtimes the check ran against
CREATE INDEX titles_source ON titles(platform_id, source);

ALTER TABLE roms ADD COLUMN zip_dir TEXT;                        -- MRA roms: directory under games/ holding the zip
ALTER TABLE roms ADD COLUMN present INTEGER NOT NULL DEFAULT 0;  -- MRA roms: the zip was on disk at the last catalogue run

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
           COUNT(DISTINCT CASE WHEN f.state = 'verified'
                                 OR (r.present = 1
                                     AND COALESCE(t.mra_check, '') NOT IN ('mismatch', 'missing_part'))
                               THEN r.id END) AS roms_verified
    FROM titles t
    LEFT JOIN roms r ON r.title_id = t.id AND r.retired = 0
    LEFT JOIN files f ON f.rom_id = r.id
    WHERE t.retired = 0
    GROUP BY t.platform_id, t.parent_id, t.id
  ) v
  GROUP BY v.platform_id, v.parent_id
) g
JOIN titles p ON p.id = g.parent_id;
