ALTER TABLE dat_versions ADD COLUMN family TEXT NOT NULL DEFAULT '';   -- dat::family_key of dat_name, refreshed at open
CREATE INDEX dat_versions_family ON dat_versions(family, platform_id);
