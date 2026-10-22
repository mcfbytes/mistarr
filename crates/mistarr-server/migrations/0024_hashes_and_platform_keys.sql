-- foreign_keys: off
-- Hashes as BLOBs, and the status, state, header rule and reason columns as integer
-- codes, in STRICT tables with their CHECKs; see docs/DATA-MODEL.md "Column forms".

DROP VIEW IF EXISTS live_rom_view;
DROP VIEW IF EXISTS rom_view;
DROP VIEW IF EXISTS file_view;

DROP TRIGGER IF EXISTS roms_insert_groups;
DROP TRIGGER IF EXISTS roms_update_groups;
DROP TRIGGER IF EXISTS roms_delete_groups;
DROP TRIGGER IF EXISTS files_insert_groups;
DROP TRIGGER IF EXISTS files_update_groups;
DROP TRIGGER IF EXISTS files_delete_groups;

DROP INDEX IF EXISTS roms_sha1;
DROP INDEX IF EXISTS roms_md5;
DROP INDEX IF EXISTS roms_crc;
DROP INDEX IF EXISTS roms_match_name;
DROP INDEX IF EXISTS roms_match_base;
DROP INDEX IF EXISTS roms_id_title;
DROP INDEX IF EXISTS roms_size;
DROP INDEX IF EXISTS roms_chd_size;
DROP INDEX IF EXISTS roms_track_size;

-- A hash text that is not hex gives an empty blob, which the length CHECK refuses,
-- so the file rolls back instead of storing NULL; a wrong width fails the same way.
CREATE TABLE roms_new (
  id         INTEGER PRIMARY KEY,
  title_id   INTEGER NOT NULL REFERENCES titles(id),
  name       TEXT NOT NULL,
  size       INTEGER NOT NULL CHECK (size >= 0),
  crc32      BLOB CHECK (length(crc32) = 4),
  md5        BLOB CHECK (length(md5) = 16),
  sha1       BLOB CHECK (length(sha1) = 20),
  status     INTEGER NOT NULL DEFAULT 2 CHECK (status BETWEEN 0 AND 3),
  retired    INTEGER NOT NULL DEFAULT 0 CHECK (retired IN (0, 1)),
  match_name TEXT,
  match_base TEXT,
  header     TEXT,
  zip_dir    TEXT,
  present    INTEGER NOT NULL DEFAULT 0 CHECK (present IN (0, 1)),
  UNIQUE (title_id, name)
) STRICT;

INSERT INTO roms_new (id, title_id, name, size, crc32, md5, sha1, status, retired,
                      match_name, match_base, header, zip_dir, present)
SELECT id, title_id, name, size,
       CASE WHEN crc32 IS NULL THEN NULL ELSE COALESCE(unhex(crc32), x'') END,
       CASE WHEN md5 IS NULL THEN NULL ELSE COALESCE(unhex(md5), x'') END,
       CASE WHEN sha1 IS NULL THEN NULL ELSE COALESCE(unhex(sha1), x'') END,
       CASE status WHEN 'good' THEN 0 WHEN 'baddump' THEN 1 WHEN 'nodump' THEN 2
                   WHEN 'verified' THEN 3 ELSE -1 END,
       retired, match_name, match_base, header, zip_dir, present
FROM roms ORDER BY id;

DROP TABLE roms;
ALTER TABLE roms_new RENAME TO roms;
CREATE INDEX roms_sha1 ON roms(sha1);
CREATE INDEX roms_md5 ON roms(md5) WHERE sha1 IS NULL;
CREATE INDEX roms_crc ON roms(crc32, size);
CREATE INDEX roms_match_name ON roms(match_name, title_id);
CREATE INDEX roms_match_base ON roms(match_base, size, title_id) WHERE match_base IS NOT NULL;
CREATE INDEX roms_id_title ON roms(id, title_id);
CREATE INDEX roms_size ON roms(size) WHERE match_base IS NOT NULL;
CREATE INDEX roms_chd_size ON roms(size) WHERE lower(name) LIKE '%.chd';
CREATE INDEX roms_track_size ON roms(size) WHERE size % 2352 = 0;

DROP INDEX IF EXISTS files_rom;
DROP INDEX IF EXISTS files_state;
DROP INDEX IF EXISTS files_rel_lower;

CREATE TABLE files_new (
  id          INTEGER PRIMARY KEY,
  platform_id TEXT NOT NULL REFERENCES platforms(id),
  rel_path    TEXT NOT NULL,
  size        INTEGER NOT NULL,
  mtime       INTEGER NOT NULL,
  crc32       BLOB CHECK (length(crc32) = 4),
  md5         BLOB CHECK (length(md5) = 16),
  sha1        BLOB CHECK (length(sha1) = 20),
  header_rule INTEGER CHECK (header_rule BETWEEN 0 AND 6),
  rom_id      INTEGER REFERENCES roms(id),
  state       INTEGER NOT NULL CHECK (state BETWEEN 0 AND 5),
  scanned_at  INTEGER NOT NULL,
  reason      INTEGER CHECK (reason BETWEEN 0 AND 16),
  crc32_whole BLOB CHECK (length(crc32_whole) = 4),
  md5_whole   BLOB CHECK (length(md5_whole) = 16),
  sha1_whole  BLOB CHECK (length(sha1_whole) = 20),
  UNIQUE (platform_id, rel_path),
  CHECK (reason IS NULL OR state = 5)
) STRICT;

INSERT INTO files_new (id, platform_id, rel_path, size, mtime, crc32, md5, sha1, header_rule,
                       rom_id, state, scanned_at, reason, crc32_whole, md5_whole, sha1_whole)
SELECT id, platform_id, rel_path, size, mtime,
       CASE WHEN crc32 IS NULL THEN NULL ELSE COALESCE(unhex(crc32), x'') END,
       CASE WHEN md5 IS NULL THEN NULL ELSE COALESCE(unhex(md5), x'') END,
       CASE WHEN sha1 IS NULL THEN NULL ELSE COALESCE(unhex(sha1), x'') END,
       CASE WHEN header_rule IS NULL THEN NULL
            WHEN header_rule = 'none' THEN 0 WHEN header_rule = 'ines' THEN 1
            WHEN header_rule = 'smc' THEN 2 WHEN header_rule = 'a78' THEN 3
            WHEN header_rule = 'lnx' THEN 4 WHEN header_rule = 'n64' THEN 5
            WHEN header_rule = 'chd' THEN 6 ELSE -1 END,
       rom_id,
       CASE state WHEN 'pending' THEN 0 WHEN 'verified' THEN 1 WHEN 'misnamed' THEN 2
                  WHEN 'unverified' THEN 3 WHEN 'bad' THEN 4 WHEN 'unidentified' THEN 5
                  ELSE -1 END,
       scanned_at,
       CASE WHEN reason IS NULL THEN NULL
            WHEN reason = 'off' THEN 0 WHEN reason = 'pending' THEN 1
            WHEN reason = 'no_layout' THEN 2 WHEN reason = 'unreadable' THEN 3
            WHEN reason = 'not_chd' THEN 4 WHEN reason = 'version' THEN 5
            WHEN reason = 'parent' THEN 6 WHEN reason = 'not_cd' THEN 7
            WHEN reason = 'too_large' THEN 8 WHEN reason = 'codec' THEN 9
            WHEN reason = 'gdrom' THEN 10 WHEN reason = 'old_layout' THEN 11
            WHEN reason = 'cooked' THEN 12 WHEN reason = 'pregap' THEN 13
            WHEN reason = 'corrupt' THEN 14 WHEN reason = 'checksum' THEN 15
            WHEN reason = 'no_checksum' THEN 16 ELSE -1 END,
       CASE WHEN crc32_whole IS NULL THEN NULL ELSE COALESCE(unhex(crc32_whole), x'') END,
       CASE WHEN md5_whole IS NULL THEN NULL ELSE COALESCE(unhex(md5_whole), x'') END,
       CASE WHEN sha1_whole IS NULL THEN NULL ELSE COALESCE(unhex(sha1_whole), x'') END
FROM files ORDER BY id;

DROP TABLE files;
ALTER TABLE files_new RENAME TO files;
CREATE INDEX files_rom ON files(rom_id);
CREATE INDEX files_state ON files(state, platform_id);
CREATE INDEX files_rel_lower ON files(platform_id, lower(rel_path));

DROP INDEX IF EXISTS chd_tracks_sha1;

CREATE TABLE chd_tracks_new (
  chd_sha1 BLOB NOT NULL CHECK (length(chd_sha1) = 20),
  chd_size INTEGER NOT NULL,
  track    INTEGER NOT NULL CHECK (track >= 1),
  size     INTEGER NOT NULL,
  crc32    BLOB NOT NULL CHECK (length(crc32) = 4),
  md5      BLOB NOT NULL CHECK (length(md5) = 16),
  sha1     BLOB NOT NULL CHECK (length(sha1) = 20),
  PRIMARY KEY (chd_sha1, chd_size, track)
) STRICT, WITHOUT ROWID;

INSERT INTO chd_tracks_new (chd_sha1, chd_size, track, size, crc32, md5, sha1)
SELECT COALESCE(unhex(chd_sha1), x''), chd_size, track, size,
       COALESCE(unhex(crc32), x''), COALESCE(unhex(md5), x''), COALESCE(unhex(sha1), x'')
FROM chd_tracks ORDER BY chd_sha1, chd_size, track;

DROP TABLE chd_tracks;
ALTER TABLE chd_tracks_new RENAME TO chd_tracks;
CREATE INDEX chd_tracks_sha1 ON chd_tracks(sha1);

CREATE TABLE chd_whole_new (
  chd_sha1 BLOB NOT NULL CHECK (length(chd_sha1) = 20),
  chd_size INTEGER NOT NULL,
  mtime    INTEGER NOT NULL,
  crc32    BLOB NOT NULL CHECK (length(crc32) = 4),
  md5      BLOB NOT NULL CHECK (length(md5) = 16),
  sha1     BLOB NOT NULL CHECK (length(sha1) = 20),
  PRIMARY KEY (chd_sha1, chd_size, mtime)
) STRICT, WITHOUT ROWID;

INSERT INTO chd_whole_new (chd_sha1, chd_size, mtime, crc32, md5, sha1)
SELECT COALESCE(unhex(chd_sha1), x''), chd_size, mtime,
       COALESCE(unhex(crc32), x''), COALESCE(unhex(md5), x''), COALESCE(unhex(sha1), x'')
FROM chd_whole ORDER BY chd_sha1, chd_size, mtime;

DROP TABLE chd_whole;
ALTER TABLE chd_whole_new RENAME TO chd_whole;

CREATE TABLE chd_failures_new (
  chd_sha1  BLOB NOT NULL CHECK (length(chd_sha1) = 20),
  chd_size  INTEGER NOT NULL,
  mtime     INTEGER NOT NULL,
  reason    INTEGER NOT NULL CHECK (reason BETWEEN 0 AND 16),
  decoder   INTEGER NOT NULL,
  failed_at INTEGER NOT NULL,
  PRIMARY KEY (chd_sha1, chd_size, mtime)
) STRICT, WITHOUT ROWID;

INSERT INTO chd_failures_new (chd_sha1, chd_size, mtime, reason, decoder, failed_at)
SELECT COALESCE(unhex(chd_sha1), x''), chd_size, mtime,
       CASE reason
         WHEN 'off' THEN 0 WHEN 'pending' THEN 1 WHEN 'no_layout' THEN 2
         WHEN 'unreadable' THEN 3 WHEN 'not_chd' THEN 4 WHEN 'version' THEN 5
         WHEN 'parent' THEN 6 WHEN 'not_cd' THEN 7 WHEN 'too_large' THEN 8
         WHEN 'codec' THEN 9 WHEN 'gdrom' THEN 10 WHEN 'old_layout' THEN 11
         WHEN 'cooked' THEN 12 WHEN 'pregap' THEN 13 WHEN 'corrupt' THEN 14
         WHEN 'checksum' THEN 15 WHEN 'no_checksum' THEN 16 ELSE -1 END,
       decoder, failed_at
FROM chd_failures ORDER BY chd_sha1, chd_size, mtime;

DROP TABLE chd_failures;
ALTER TABLE chd_failures_new RENAME TO chd_failures;

CREATE TRIGGER roms_insert_groups AFTER INSERT ON roms WHEN NEW.retired = 0 BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT group_root FROM titles WHERE id = NEW.title_id AND group_root IS NOT NULL
      AND group_root NOT IN (SELECT parent_id FROM title_groups_dirty);
END;

CREATE TRIGGER roms_update_groups AFTER UPDATE OF title_id, retired, present ON roms
WHEN (OLD.retired = 0 OR NEW.retired = 0) AND (OLD.title_id IS NOT NEW.title_id
  OR OLD.retired IS NOT NEW.retired OR OLD.present IS NOT NEW.present) BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT DISTINCT group_root FROM titles WHERE id IN (OLD.title_id, NEW.title_id)
      AND group_root IS NOT NULL AND group_root NOT IN (SELECT parent_id FROM title_groups_dirty);
END;

CREATE TRIGGER roms_delete_groups AFTER DELETE ON roms WHEN OLD.retired = 0 BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT group_root FROM titles WHERE id = OLD.title_id AND group_root IS NOT NULL
      AND group_root NOT IN (SELECT parent_id FROM title_groups_dirty);
END;

CREATE TRIGGER files_insert_groups AFTER INSERT ON files
WHEN NEW.rom_id IS NOT NULL AND NEW.state = 1 BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT t.group_root FROM roms r JOIN titles t ON t.id = r.title_id
    WHERE r.id = NEW.rom_id AND t.group_root IS NOT NULL
      AND t.group_root NOT IN (SELECT parent_id FROM title_groups_dirty);
END;

CREATE TRIGGER files_update_groups AFTER UPDATE OF rom_id, state ON files
WHEN (OLD.state = 1 OR NEW.state = 1)
  AND (OLD.rom_id IS NOT NEW.rom_id OR OLD.state IS NOT NEW.state) BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT DISTINCT t.group_root FROM roms r JOIN titles t ON t.id = r.title_id
    WHERE r.id IN (OLD.rom_id, NEW.rom_id) AND t.group_root IS NOT NULL
      AND t.group_root NOT IN (SELECT parent_id FROM title_groups_dirty);
END;

CREATE TRIGGER files_delete_groups AFTER DELETE ON files
WHEN OLD.rom_id IS NOT NULL AND OLD.state = 1 BEGIN
  INSERT INTO title_groups_dirty (parent_id)
    SELECT t.group_root FROM roms r JOIN titles t ON t.id = r.title_id
    WHERE r.id = OLD.rom_id AND t.group_root IS NOT NULL
      AND t.group_root NOT IN (SELECT parent_id FROM title_groups_dirty);
END;

CREATE VIEW rom_view AS
SELECT r.id, r.title_id, r.name, r.size, r.crc32, r.md5, r.sha1,
       NULLIF(lower(hex(r.crc32)), '') AS crc32_hex, NULLIF(lower(hex(r.md5)), '') AS md5_hex,
       NULLIF(lower(hex(r.sha1)), '') AS sha1_hex,
       r.status,
       CASE r.status WHEN 0 THEN 'good' WHEN 1 THEN 'baddump' WHEN 2 THEN 'nodump'
                     WHEN 3 THEN 'verified' END AS status_text,
       r.match_name, r.match_base, r.retired,
       r.header, r.zip_dir, r.present,
       t.name AS title_name, t.dat_version_id, t.source AS title_source,
       t.platform_id AS platform_key, t.platform_id AS platform, t.group_root, t.parent_id,
       t.retired AS title_retired,
       (r.retired = 0 AND t.retired = 0) AS live,
       EXISTS (SELECT 1 FROM title_flags f WHERE f.title_id = r.title_id AND f.flag = 'bios') AS bios
FROM roms r JOIN titles t ON t.id = r.title_id;

CREATE VIEW live_rom_view AS
SELECT * FROM rom_view WHERE live AND NOT bios;

CREATE VIEW file_view AS
SELECT f.id, f.platform_id AS platform_key, f.platform_id AS platform, f.rel_path, f.size,
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
