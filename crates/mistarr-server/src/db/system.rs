//! Counts the system routes derive their state from.

use rusqlite::Connection;

use crate::error::Result;

/// Row counts that decide which wizard steps are complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WizardCounts {
    /// Rows in `dat_versions`, retired or not.
    pub dat_versions: u64,
    /// Rows in `sources`.
    pub sources: u64,
}

/// Counts DAT versions and sources.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let c = mistarr_server::db::system::wizard_counts(&conn).unwrap();
/// assert_eq!(c.dat_versions, 0);
/// ```
pub fn wizard_counts(conn: &Connection) -> Result<WizardCounts> {
    let (dats, sources): (i64, i64) = conn.query_row(
        "SELECT (SELECT COUNT(*) FROM dat_versions), (SELECT COUNT(*) FROM sources)",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok(WizardCounts {
        dat_versions: u64::try_from(dats).unwrap_or(0),
        sources: u64::try_from(sources).unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_follow_rows() {
        let mut c = Connection::open_in_memory().expect("open");
        crate::db::migrate::apply(&mut c).expect("migrate");
        assert_eq!(wizard_counts(&c).expect("counts"), WizardCounts::default());
        c.execute_batch(
            "INSERT INTO dat_versions (dat_name, version, source_file, loaded_at, game_count)
             VALUES ('Test Console', '1', 'a.dat', 0, 0);
             INSERT INTO sources (infohash, display_name, origin_file, state, added_at)
             VALUES ('00', 'n', 'a.torrent', 'unbound', 0);",
        )
        .expect("insert");
        let got = wizard_counts(&c).expect("counts");
        assert_eq!((got.dat_versions, got.sources), (1, 1));
    }
}
