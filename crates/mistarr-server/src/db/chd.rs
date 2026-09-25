//! The `chd_tracks` cache, `chd_failures`, and the `files` rows of CHD images waiting to
//! be identified. See `docs/VERIFICATION.md` "CHD images" and `docs/DATA-MODEL.md`.

use mistarr_core::chd::{ChdId, Sha1Digest, Unidentifiable};
use mistarr_core::{HashSet, PlatformId};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Serialize, Serializer};

use super::files::{self, FileId, FileRow, NewFile};
use crate::error::Result;

/// Why a CHD's `files` row is `unidentified`: the stable code stored in `files.reason`.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unidentified {
    /// Identifying CHD images by their tracks is off.
    Off,
    /// Waiting for the `chd_tracks` job.
    Pending,
    /// No loaded DAT entry has this number and size of tracks.
    NoLayout,
    /// The file could not be read.
    Unreadable,
    /// The image itself cannot be identified.
    Chd(Unidentifiable),
}

impl Unidentified {
    /// The `files.reason` code.
    ///
    /// ```
    /// use mistarr_core::chd::Unidentifiable;
    /// use mistarr_server::db::chd::Unidentified;
    /// assert_eq!(Unidentified::NoLayout.as_str(), "no_layout");
    /// assert_eq!(Unidentified::Chd(Unidentifiable::Cooked).as_str(), "cooked");
    /// ```
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Pending => "pending",
            Self::NoLayout => "no_layout",
            Self::Unreadable => "unreadable",
            Self::Chd(r) => r.code(),
        }
    }

    /// Parses a `files.reason` code; `None` for an unknown one.
    ///
    /// ```
    /// use mistarr_server::db::chd::Unidentified;
    /// assert_eq!(Unidentified::parse("pending"), Some(Unidentified::Pending));
    /// assert_eq!(Unidentified::parse("nope"), None);
    /// ```
    #[must_use]
    pub fn parse(code: &str) -> Option<Self> {
        [Self::Off, Self::Pending, Self::NoLayout, Self::Unreadable]
            .into_iter()
            .find(|r| r.as_str() == code)
            .or_else(|| Unidentifiable::from_code(code).map(Self::Chd))
    }
}

impl Serialize for Unidentified {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

fn size_i64(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// The cached track hashes of image `id`, `Some` only when tracks 1 to n are all present.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_core::chd::{ChdId, Sha1Digest};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let id = ChdId { sha1: Sha1Digest([7; 20]), size: 10 };
/// assert!(mistarr_server::db::chd::cached_tracks(&conn, &id).unwrap().is_none());
/// ```
pub fn cached_tracks(conn: &Connection, id: &ChdId) -> Result<Option<Vec<HashSet>>> {
    let rows: Vec<(i64, i64, String, String, String)> = conn
        .prepare_cached(
            "SELECT track, size, crc32, md5, sha1 FROM chd_tracks
             WHERE chd_sha1 = ?1 AND chd_size = ?2 ORDER BY track",
        )?
        .query_map(params![id.sha1.to_hex(), size_i64(id.size)], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let numbered = rows
        .iter()
        .enumerate()
        .all(|(i, r)| i64::try_from(i + 1).is_ok_and(|n| n == r.0));
    if rows.is_empty() || !numbered {
        return Ok(None);
    }
    Ok(Some(
        rows.into_iter()
            .map(|(_, size, crc32, md5, sha1)| HashSet {
                size: u64::try_from(size).unwrap_or(0),
                crc32,
                md5,
                sha1,
            })
            .collect(),
    ))
}

/// Caches the track hashes of image `id`, replacing any it had.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn store_tracks(conn: &Connection, id: &ChdId, tracks: &[HashSet]) -> Result<()> {
    let (sha1, size) = (id.sha1.to_hex(), size_i64(id.size));
    conn.prepare_cached("DELETE FROM chd_tracks WHERE chd_sha1 = ?1 AND chd_size = ?2")?
        .execute(params![sha1, size])?;
    let mut insert = conn.prepare_cached(
        "INSERT INTO chd_tracks (chd_sha1, chd_size, track, size, crc32, md5, sha1)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    for (n, t) in (1i64..).zip(tracks) {
        insert.execute(params![
            sha1,
            size,
            n,
            size_i64(t.size),
            t.crc32,
            t.md5,
            t.sha1
        ])?;
    }
    Ok(())
}

/// The cached image whose tracks are exactly `tracks`, for rows that no longer know it.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn find_id(conn: &Connection, tracks: &[HashSet]) -> Result<Option<ChdId>> {
    let Some(first) = tracks.first() else {
        return Ok(None);
    };
    let found: Vec<(String, i64)> = conn
        .prepare_cached(
            "SELECT chd_sha1, chd_size FROM chd_tracks
             WHERE sha1 = ?1 AND track = 1 AND size = ?2 ORDER BY chd_sha1, chd_size",
        )?
        .query_map(params![first.sha1, size_i64(first.size)], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    for (sha1, size) in found {
        let Some(sha1) = Sha1Digest::from_hex(&sha1) else {
            continue;
        };
        let id = ChdId {
            sha1,
            size: u64::try_from(size).unwrap_or(0),
        };
        if cached_tracks(conn, &id)?.as_deref() == Some(tracks) {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

/// Why image `id` with modification time `mtime` could not be identified, and the decoder
/// version that found it. A file rewritten since, as when a copy completes, has another
/// `mtime` and no failure.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn failure(conn: &Connection, id: &ChdId, mtime: i64) -> Result<Option<(Unidentifiable, u32)>> {
    let row: Option<(String, u32)> = conn
        .prepare_cached(
            "SELECT reason, decoder FROM chd_failures
             WHERE chd_sha1 = ?1 AND chd_size = ?2 AND mtime = ?3",
        )?
        .query_row(params![id.sha1.to_hex(), size_i64(id.size), mtime], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?;
    Ok(row.map(|(code, decoder)| {
        let reason = Unidentifiable::from_code(&code).unwrap_or(Unidentifiable::Corrupt);
        (reason, decoder)
    }))
}

/// Records that image `id`, as a file with modification time `mtime`, cannot be identified
/// by this decoder version; an unchanged record keeps its `failed_at`.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn store_failure(
    conn: &Connection,
    id: &ChdId,
    mtime: i64,
    reason: Unidentifiable,
    now: i64,
) -> Result<()> {
    conn.prepare_cached(
        "INSERT INTO chd_failures (chd_sha1, chd_size, mtime, reason, decoder, failed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(chd_sha1, chd_size, mtime) DO UPDATE SET
           reason = excluded.reason, decoder = excluded.decoder, failed_at = excluded.failed_at
         WHERE chd_failures.reason <> excluded.reason OR chd_failures.decoder <> excluded.decoder",
    )?
    .execute(params![
        id.sha1.to_hex(),
        size_i64(id.size),
        mtime,
        reason.code(),
        mistarr_core::chd::DECODER_VERSION,
        now
    ])?;
    Ok(())
}

/// The `unidentified` rows `f` of enabled disc platforms `p`; callers add conditions.
const WAITING_FROM: &str = "FROM files f JOIN platforms p ON p.id = f.platform_id
     WHERE f.state = 'unidentified' AND p.kind = 'disc' AND p.enabled = 1";

/// Up to `limit` `pending` rows of enabled disc platforms with an id above `after`, by id.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn waiting(conn: &Connection, after: FileId, limit: u32) -> Result<Vec<FileRow>> {
    let cols = files::COLUMNS
        .split(", ")
        .map(|c| format!("f.{c}"))
        .collect::<Vec<_>>()
        .join(", ");
    Ok(conn
        .prepare_cached(&format!(
            "SELECT {cols} {WAITING_FROM} AND f.reason = 'pending' AND f.id > ?1
             ORDER BY f.id LIMIT ?2"
        ))?
        .query_map(params![after.0, limit], files::from_row)?
        .collect::<rusqlite::Result<_>>()?)
}

/// How many rows [`waiting`] would return with no limit.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn waiting_count(conn: &Connection) -> Result<u64> {
    let n: i64 = conn
        .prepare_cached(&format!(
            "SELECT COUNT(*) {WAITING_FROM} AND f.reason = 'pending'"
        ))?
        .query_row([], |r| r.get(0))?;
    Ok(u64::try_from(n).unwrap_or(0))
}

/// Whether an enabled disc platform, or `platform` alone, has a row with one of `reasons`.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn has_waiting(
    conn: &Connection,
    platform: Option<&PlatformId>,
    reasons: &[Unidentified],
) -> Result<bool> {
    let codes: Vec<&str> = reasons.iter().map(|r| r.as_str()).collect();
    let codes = serde_json::to_string(&codes).map_err(|e| crate::Error::Job(e.to_string()))?;
    Ok(conn
        .prepare_cached(&format!(
            "SELECT EXISTS(SELECT 1 {WAITING_FROM}
               AND f.reason IN (SELECT value FROM json_each(?1))
               AND (?2 IS NULL OR f.platform_id = ?2))"
        ))?
        .query_row(params![codes, platform.map(|p| p.0.as_str())], |r| r.get(0))?)
}

/// Follows the setting: on moves `off` rows to `pending`; off moves `pending` and
/// `no_layout` rows to `off`. Returns the rows changed.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn set_waiting(conn: &Connection, on: bool) -> Result<usize> {
    let sql = if on {
        "UPDATE files SET reason = 'pending' WHERE state = 'unidentified' AND reason = 'off'"
    } else {
        "UPDATE files SET reason = 'off'
         WHERE state = 'unidentified' AND reason IN ('pending', 'no_layout')"
    };
    Ok(conn.prepare_cached(sql)?.execute([])?)
}

/// Moves the `no_layout` rows of `platform` back to `pending`, after its DATs changed.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn recheck_layouts(conn: &Connection, platform: &PlatformId) -> Result<usize> {
    Ok(conn
        .prepare_cached(
            "UPDATE files SET reason = 'pending'
             WHERE platform_id = ?1 AND state = 'unidentified' AND reason = 'no_layout'",
        )?
        .execute([&platform.0])?)
}

/// Sets row `id`'s reason to `to` only while it is `unidentified` with reason `from`.
/// Returns whether it changed.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn set_reason_if(
    conn: &Connection,
    id: FileId,
    from: Unidentified,
    to: Unidentified,
) -> Result<bool> {
    Ok(conn
        .prepare_cached(
            "UPDATE files SET reason = ?3 WHERE id = ?1 AND state = 'unidentified' AND reason = ?2",
        )?
        .execute(params![id.0, from.as_str(), to.as_str()])?
        > 0)
}

/// Whether a live DAT title on `platform` has exactly one non-cue live rom per size in
/// `sizes`, in any order: the cheap check before a decode.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn layout_known(conn: &Connection, platform: &PlatformId, sizes: &[u64]) -> Result<bool> {
    let Some(&first) = sizes.first() else {
        return Ok(false);
    };
    let titles: Vec<i64> = conn
        .prepare_cached(
            "SELECT DISTINCT r.title_id FROM roms r INDEXED BY roms_size
             JOIN titles t ON t.id = r.title_id
             WHERE r.size = ?2 AND t.platform_id = ?1 AND t.source = 'dat'
               AND r.retired = 0 AND t.retired = 0 AND lower(r.name) NOT LIKE '%.cue'",
        )?
        .query_map(params![platform.0, size_i64(first)], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut want: Vec<i64> = sizes.iter().map(|&s| size_i64(s)).collect();
    want.sort_unstable();
    let mut stmt = conn.prepare_cached(
        "SELECT COALESCE(size, -1) FROM roms
         WHERE title_id = ?1 AND retired = 0 AND lower(name) NOT LIKE '%.cue'",
    )?;
    for title in titles {
        let mut got: Vec<i64> = stmt
            .query_map([title], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        got.sort_unstable();
        if got == want {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Replaces container `container`'s row and member rows with `rows`: the container row and
/// members not in `rows` are deleted through [`files::delete_ids`], and rows that differ
/// from what is stored are written.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn replace_container(
    conn: &Connection,
    platform: &PlatformId,
    container: &str,
    rows: &[NewFile],
    now: i64,
) -> Result<()> {
    let existing = files::zip_member_rows(conn, platform, container)?;
    let mut stale: Vec<i64> = existing
        .iter()
        .filter(|e| !rows.iter().any(|r| r.rel_path == e.rel_path))
        .map(|e| e.id.0)
        .collect();
    if let Some(bare) = files::find_by_path(conn, platform, container)? {
        stale.push(bare.id.0);
    }
    files::delete_ids(conn, &stale)?;
    for row in rows {
        let same = existing.iter().any(|e| {
            e.rel_path == row.rel_path
                && e.size == row.size
                && e.mtime == row.mtime
                && e.crc32 == row.crc32
                && e.md5 == row.md5
                && e.sha1 == row.sha1
                && e.header_rule == row.header_rule
                && e.rom_id == row.rom_id
                && e.state == row.state
                && e.reason == row.reason
        });
        if !same {
            files::upsert_row(conn, platform, row, now)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::files::FileState;

    fn conn() -> Connection {
        let mut c = Connection::open_in_memory().expect("open");
        crate::db::migrate::apply(&mut c).expect("migrate");
        crate::db::platforms::seed(&mut c, &mistarr_mister::platforms::PLATFORMS).expect("seed");
        c
    }

    fn psx() -> PlatformId {
        PlatformId("psx".into())
    }

    fn id(n: u8, size: u64) -> ChdId {
        ChdId {
            sha1: Sha1Digest([n; 20]),
            size,
        }
    }

    fn track(n: u8, size: u64) -> HashSet {
        HashSet {
            size,
            crc32: format!("{n:08x}"),
            md5: format!("{n:032x}"),
            sha1: format!("{n:040x}"),
        }
    }

    fn container(rel: &str, reason: Unidentified) -> NewFile {
        NewFile {
            rel_path: rel.to_owned(),
            size: 100,
            mtime: 1,
            crc32: None,
            md5: None,
            sha1: None,
            header_rule: Some("chd".into()),
            rom_id: None,
            state: FileState::Unidentified,
            reason: Some(reason.as_str().to_owned()),
        }
    }

    fn put(c: &Connection, pid: &PlatformId, row: &NewFile) -> FileId {
        files::upsert_row(c, pid, row, 1).expect("upsert")
    }

    fn reason_of(c: &Connection, rel: &str) -> Option<String> {
        files::find_by_path(c, &psx(), rel)
            .expect("find")
            .and_then(|r| r.reason)
    }

    #[test]
    fn reason_codes_round_trip() {
        for r in [
            Unidentified::Off,
            Unidentified::Pending,
            Unidentified::NoLayout,
            Unidentified::Unreadable,
            Unidentified::Chd(Unidentifiable::PregapMissing),
            Unidentified::Chd(Unidentifiable::Checksum),
        ] {
            assert_eq!(Unidentified::parse(r.as_str()), Some(r));
        }
        assert_eq!(
            serde_json::to_string(&Unidentified::NoLayout).expect("json"),
            "\"no_layout\""
        );
    }

    #[test]
    fn tracks_cache_round_trips_and_needs_every_track() {
        let c = conn();
        let a = id(1, 500);
        let tracks = [track(1, 2352), track(2, 4704)];
        store_tracks(&c, &a, &tracks).expect("store");
        assert_eq!(cached_tracks(&c, &a).expect("read"), Some(tracks.to_vec()));
        assert!(cached_tracks(&c, &id(1, 501)).expect("read").is_none());
        assert_eq!(find_id(&c, &tracks).expect("find"), Some(a));
        assert_eq!(find_id(&c, &tracks[..1]).expect("find"), None);
        assert_eq!(find_id(&c, &[]).expect("find"), None);
        c.execute("DELETE FROM chd_tracks WHERE track = 1", [])
            .expect("delete");
        assert!(cached_tracks(&c, &a).expect("read").is_none(), "a gap");
        store_tracks(&c, &a, &tracks[..1]).expect("replace");
        assert_eq!(
            cached_tracks(&c, &a).expect("read"),
            Some(tracks[..1].to_vec())
        );
    }

    #[test]
    fn a_failure_keeps_its_first_time_until_it_changes() {
        let c = conn();
        let a = id(2, 9);
        assert!(failure(&c, &a, 5).expect("read").is_none());
        store_failure(&c, &a, 5, Unidentifiable::Cooked, 10).expect("store");
        store_failure(&c, &a, 5, Unidentifiable::Cooked, 20).expect("again");
        let at: i64 = c
            .query_row("SELECT failed_at FROM chd_failures", [], |r| r.get(0))
            .expect("at");
        assert_eq!(at, 10);
        assert_eq!(
            failure(&c, &a, 5).expect("read"),
            Some((Unidentifiable::Cooked, mistarr_core::chd::DECODER_VERSION))
        );
        assert!(
            failure(&c, &a, 6).expect("read").is_none(),
            "a rewritten file"
        );
        store_failure(&c, &a, 5, Unidentifiable::Corrupt, 30).expect("changed");
        assert_eq!(
            failure(&c, &a, 5).expect("read").map(|f| f.0),
            Some(Unidentifiable::Corrupt)
        );
    }

    #[test]
    fn waiting_rows_follow_the_setting_and_platform_state() {
        let c = conn();
        let pid = psx();
        let a = put(&c, &pid, &container("PSX/A/a.chd", Unidentified::Pending));
        put(&c, &pid, &container("PSX/B/b.chd", Unidentified::NoLayout));
        put(&c, &pid, &container("PSX/C/c.chd", Unidentified::Off));
        let pending = [Unidentified::Pending];
        assert_eq!(waiting(&c, FileId(0), 10).expect("page").len(), 1);
        assert!(waiting(&c, a, 10).expect("page").is_empty());
        assert_eq!(waiting_count(&c).expect("count"), 1);
        assert!(has_waiting(&c, Some(&pid), &pending).expect("has"));
        assert!(!has_waiting(&c, Some(&PlatformId("saturn".into())), &pending).expect("has"));

        assert_eq!(set_waiting(&c, false).expect("off"), 2);
        assert!(!has_waiting(&c, None, &pending).expect("has"));
        assert_eq!(reason_of(&c, "PSX/B/b.chd").as_deref(), Some("off"));
        assert_eq!(set_waiting(&c, true).expect("on"), 3);
        assert_eq!(waiting_count(&c).expect("count"), 3);

        crate::db::platforms::set_enabled(&c, "psx", false).expect("disable");
        assert_eq!(
            waiting_count(&c).expect("count"),
            0,
            "disabled platforms wait"
        );
        assert!(!has_waiting(&c, None, &pending).expect("has"));
    }

    #[test]
    fn set_reason_if_skips_a_row_that_moved_on() {
        let c = conn();
        let pid = psx();
        let a = put(&c, &pid, &container("PSX/A/a.chd", Unidentified::Pending));
        let (p, n) = (Unidentified::Pending, Unidentified::NoLayout);
        assert!(set_reason_if(&c, a, p, n).expect("set"));
        assert!(!set_reason_if(&c, a, p, Unidentified::Off).expect("skip"));
        assert_eq!(reason_of(&c, "PSX/A/a.chd").as_deref(), Some("no_layout"));
        assert_eq!(recheck_layouts(&c, &pid).expect("recheck"), 1);
        assert_eq!(reason_of(&c, "PSX/A/a.chd").as_deref(), Some("pending"));
    }

    #[test]
    fn layout_known_needs_the_same_track_sizes() {
        let c = conn();
        let pid = psx();
        let title = files::seed_title_fixture(&c, &pid, "Disc").expect("title");
        for (name, t) in [
            ("a.cue", track(9, 70)),
            ("a1.bin", track(1, 4704)),
            ("a2.bin", track(2, 2352)),
        ] {
            files::seed_rom_for_title_fixture(&c, title, name, &t, "good").expect("rom");
        }
        assert!(layout_known(&c, &pid, &[2352, 4704]).expect("known"));
        assert!(layout_known(&c, &pid, &[4704, 2352]).expect("known"));
        assert!(!layout_known(&c, &pid, &[4704]).expect("fewer"));
        assert!(!layout_known(&c, &pid, &[4704, 2352, 2352]).expect("more"));
        assert!(!layout_known(&c, &pid, &[70, 4704, 2352]).expect("cue is not a track"));
        assert!(!layout_known(&c, &pid, &[]).expect("empty"));
        assert!(!layout_known(&c, &PlatformId("saturn".into()), &[2352, 4704]).expect("other"));
    }

    #[test]
    fn replace_container_swaps_rows_and_clears_the_import_log() {
        let c = conn();
        let pid = psx();
        let bare = put(&c, &pid, &container("PSX/G/g.chd", Unidentified::Pending));
        put(
            &c,
            &pid,
            &NewFile {
                rel_path: "PSX/G/g.chd#cue".into(),
                ..container("x", Unidentified::Off)
            },
        );
        c.execute(
            "INSERT INTO import_log (at, file_id, action, detail) VALUES (0, ?1, 'placed', '{}')",
            [bare.0],
        )
        .expect("log");
        let member = NewFile {
            rel_path: "PSX/G/g.chd#01".into(),
            crc32: Some("1".into()),
            md5: Some("2".into()),
            sha1: Some("3".into()),
            state: FileState::Unverified,
            reason: None,
            ..container("x", Unidentified::Off)
        };
        replace_container(&c, &pid, "PSX/G/g.chd", std::slice::from_ref(&member), 5)
            .expect("replace");
        for gone in ["PSX/G/g.chd", "PSX/G/g.chd#cue"] {
            assert!(
                files::find_by_path(&c, &pid, gone).expect("find").is_none(),
                "{gone}"
            );
        }
        let got = files::find_by_path(&c, &pid, "PSX/G/g.chd#01")
            .expect("find")
            .expect("member");
        assert_eq!(
            (got.state, got.reason, got.scanned_at),
            (FileState::Unverified, None, 5)
        );
        let logged: Option<i64> = c
            .query_row("SELECT file_id FROM import_log", [], |r| r.get(0))
            .expect("log");
        assert_eq!(logged, None);
        replace_container(&c, &pid, "PSX/G/g.chd", &[member], 9).expect("again");
        let again = files::find_by_path(&c, &pid, "PSX/G/g.chd#01")
            .expect("find")
            .expect("member");
        assert_eq!(again.scanned_at, 5, "an unchanged row is not written");
    }
}
