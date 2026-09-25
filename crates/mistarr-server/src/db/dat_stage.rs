//! `dat_stage`: the games of the DAT being imported, parsed and stored in chunks in a
//! TEMP table, then applied in one transaction; see `docs/ARCHITECTURE.md` "DAT import".

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use super::dats::DatVersionId;
use super::titles::{self, RomInput, TitleInput};
use crate::error::Result;

/// One parsed game waiting to be applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagedGame {
    /// Full DAT game name.
    pub name: String,
    /// Name without tags.
    pub base_name: String,
    /// `naming::group_key` of the name.
    pub group_key: String,
    /// The DAT's `cloneof`, if any.
    pub clone_of: Option<String>,
    /// Region names.
    pub regions: Vec<String>,
    /// Language codes.
    pub languages: Vec<String>,
    /// Revision label.
    pub revision: Option<String>,
    /// Flag labels.
    pub flags: Vec<String>,
    /// Its `<rom>` entries.
    pub roms: Vec<StagedRom>,
}

/// One `<rom>` of a staged game; the fields of [`RomInput`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagedRom {
    /// File name in the DAT.
    pub name: String,
    /// Size in bytes.
    pub size: u64,
    /// Lowercase hex CRC32.
    pub crc32: Option<String>,
    /// Lowercase hex MD5.
    pub md5: Option<String>,
    /// Lowercase hex SHA1.
    pub sha1: Option<String>,
    /// `good`, `baddump`, `nodump` or `verified`.
    pub status: String,
    /// The DAT's `header` attribute, verbatim.
    pub header: Option<String>,
}

/// Creates the connection's stage, a TEMP table, so staged rows go to SQLite's
/// temporary directory rather than the database file, and nothing survives a restart.
fn ensure(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS dat_stage (seq INTEGER PRIMARY KEY, game TEXT NOT NULL)",
    )?;
    Ok(())
}

/// Empties the stage. The background lane imports one DAT at a time, so a
/// load starts from an empty stage.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// mistarr_server::db::dat_stage::clear(&conn).unwrap();
/// ```
pub fn clear(conn: &Connection) -> Result<()> {
    ensure(conn)?;
    conn.execute("DELETE FROM temp.dat_stage", [])?;
    Ok(())
}

/// Appends `games` to the stage, in order.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn append(conn: &Connection, games: &[StagedGame]) -> Result<()> {
    ensure(conn)?;
    let mut stmt = conn.prepare_cached("INSERT INTO temp.dat_stage (game) VALUES (?1)")?;
    for game in games {
        let text = serde_json::to_string(game).map_err(|e| crate::Error::Job(e.to_string()))?;
        stmt.execute(params![text])?;
    }
    Ok(())
}

/// Stores every staged game as a title of `version` on `platform`, in the
/// order they were staged, and returns how many.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure, [`crate::Error::Job`] for a row
/// that does not read back.
pub fn apply(conn: &Connection, platform: &str, version: DatVersionId) -> Result<u64> {
    ensure(conn)?;
    let mut stmt = conn.prepare("SELECT game FROM temp.dat_stage ORDER BY seq")?;
    let mut staged = stmt.query([])?;
    let mut n = 0;
    while let Some(row) = staged.next()? {
        let text: String = row.get(0)?;
        let game: StagedGame =
            serde_json::from_str(&text).map_err(|e| crate::Error::Job(e.to_string()))?;
        let title = TitleInput {
            name: &game.name,
            base_name: &game.base_name,
            group_key: &game.group_key,
            clone_of: game.clone_of.as_deref(),
            regions: &game.regions,
            languages: &game.languages,
            revision: game.revision.as_deref(),
            flags: &game.flags,
        };
        let roms: Vec<RomInput<'_>> = game
            .roms
            .iter()
            .map(|r| RomInput {
                name: &r.name,
                size: r.size,
                crc32: r.crc32.as_deref(),
                md5: r.md5.as_deref(),
                sha1: r.sha1.as_deref(),
                status: &r.status,
                header: r.header.as_deref(),
            })
            .collect();
        titles::upsert_title(conn, platform, version, &title, &roms)?;
        n += 1;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::dats::{upsert_version, NewVersion};

    fn game(name: &str) -> StagedGame {
        StagedGame {
            name: name.to_owned(),
            base_name: name.to_owned(),
            group_key: name.to_ascii_lowercase(),
            clone_of: None,
            regions: vec!["USA".into()],
            languages: Vec::new(),
            revision: None,
            flags: Vec::new(),
            roms: vec![StagedRom {
                name: format!("{name}.gb"),
                size: 16,
                crc32: Some("00000001".into()),
                md5: None,
                sha1: None,
                status: "good".into(),
                header: None,
            }],
        }
    }

    #[test]
    fn staged_games_apply_in_order_and_clear() {
        let mut c = rusqlite::Connection::open_in_memory().expect("open");
        crate::db::migrate::apply(&mut c).expect("migrate");
        crate::db::platforms::seed(&mut c, &mistarr_mister::platforms::PLATFORMS).expect("seed");
        let v = NewVersion {
            dat_name: "Maker - Game Boy",
            version: "1",
            source_file: "gb.dat",
            platform: Some("gb"),
            now: 1,
        };
        let id = upsert_version(&c, &v).expect("version").id;
        append(&c, &[game("Example Quest (USA)")]).expect("append");
        append(&c, &[game("Other Tale (USA)")]).expect("append");
        assert_eq!(apply(&c, "gb", id).expect("apply"), 2);
        let names: Vec<String> = c
            .prepare("SELECT name FROM titles ORDER BY id")
            .expect("prepare")
            .query_map([], |r| r.get(0))
            .expect("query")
            .collect::<rusqlite::Result<_>>()
            .expect("rows");
        assert_eq!(names, ["Example Quest (USA)", "Other Tale (USA)"]);
        clear(&c).expect("clear");
        assert_eq!(apply(&c, "gb", id).expect("apply"), 0);
        let in_main: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM main.sqlite_master WHERE name = 'dat_stage'",
                [],
                |r| r.get(0),
            )
            .expect("schema");
        assert_eq!(in_main, 0, "the stage never reaches the database file");
    }
}
