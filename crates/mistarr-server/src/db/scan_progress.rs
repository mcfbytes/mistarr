//! The `scan_progress` table: the directories a platform's interrupted scan already
//! committed. See `docs/DATA-MODEL.md` "`scan_progress`".

use mistarr_core::{PlatformId, PlatformKey};
use rusqlite::{params, Connection, OptionalExtension};

use super::sql;
use crate::error::Result;

/// The directories already committed for a platform's in-progress scan.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure, [`crate::Error::Stored`] on unparseable JSON.
pub fn get(conn: &Connection, platform: PlatformKey) -> Result<Vec<String>> {
    Ok(conn
        .query_row(
            "SELECT done_dirs FROM scan_progress WHERE platform = ?1",
            [platform],
            |r| sql::get_json(r, 0, "scan_progress.done_dirs"),
        )
        .optional()?
        .unwrap_or_default())
}

/// Replaces the committed-directory list for a platform's scan.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure, [`crate::Error::Stored`] when the list cannot
/// be written as JSON.
pub fn save(
    conn: &Connection,
    platform: PlatformKey,
    done_dirs: &[String],
    now: i64,
) -> Result<()> {
    let json = sql::to_json("scan_progress.done_dirs", done_dirs)?;
    conn.prepare_cached(
        "INSERT INTO scan_progress (platform, done_dirs, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(platform) DO UPDATE SET done_dirs = excluded.done_dirs, updated_at = excluded.updated_at",
    )?
    .execute(params![platform, json, now])?;
    Ok(())
}

/// Clears a platform's scan progress once its scan finished.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn clear(conn: &Connection, platform: PlatformKey) -> Result<()> {
    conn.execute("DELETE FROM scan_progress WHERE platform = ?1", [platform])?;
    Ok(())
}

/// Platforms with saved scan progress, for re-enqueuing an interrupted scan
/// at startup.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn platforms_with_progress(conn: &Connection) -> Result<Vec<PlatformId>> {
    let mut stmt = conn.prepare(
        "SELECT p.slug FROM scan_progress s JOIN platforms p ON p.id = s.platform ORDER BY p.slug",
    )?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    Ok(rows
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .map(PlatformId::new)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::fixtures::conn;

    #[test]
    fn progress_round_trips_and_clears() {
        let c = conn();
        let pid = crate::db::platforms::fixed_key("nes").expect("key");
        assert!(get(&c, pid).expect("empty").is_empty());
        save(&c, pid, &["NES".to_owned()], 5).expect("save");
        assert_eq!(get(&c, pid).expect("get"), ["NES".to_owned()]);
        let got = platforms_with_progress(&c).expect("list");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].as_str(), "nes");
        clear(&c, pid).expect("clear");
        assert!(get(&c, pid).expect("empty").is_empty());
        assert!(platforms_with_progress(&c).expect("list").is_empty());
    }
}
