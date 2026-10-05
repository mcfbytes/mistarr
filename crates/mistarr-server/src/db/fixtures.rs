//! Synthetic catalog rows standing in for the DAT import; compiled for tests and the
//! `test-support` feature only.

use mistarr_core::{Hashes, PlatformId};
use rusqlite::{params, Connection};

use super::downloads::DownloadState;
use super::ids::{DatVersionId, DownloadId, SourceId, TitleId};
use super::sql;
use super::titles::RomStatus;
use crate::error::Result;
use mistarr_core::RomId;

/// A migrated in-memory database with the platform table seeded.
///
/// # Panics
///
/// When SQLite cannot open or migrate an in-memory database.
#[must_use]
pub fn conn() -> Connection {
    let mut c = Connection::open_in_memory().expect("open");
    super::migrate::apply(&mut c).expect("migrate");
    super::platforms::seed(&mut c, &mistarr_mister::platforms::PLATFORMS).expect("seed platforms");
    c
}

/// The platform id `id`.
#[must_use]
pub fn pid(id: &str) -> PlatformId {
    PlatformId::new(id.to_owned())
}

/// The three roms of the standard `nes` catalog, in id order: `(rom name, size)`.
pub const CATALOG: [(&str, u64); 3] = [
    ("Example Quest (USA).nes", 40_976),
    ("Second Try (Japan).nes", 24_592),
    ("Third Tale (Europe).nes", 65_552),
];

/// Writes the standard `nes` catalog of [`CATALOG`] and returns its rom ids.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn catalog(conn: &Connection) -> Result<Vec<RomId>> {
    CATALOG
        .iter()
        .map(|(name, size)| seed_rom(conn, &pid("nes"), name, *size, &[]))
        .collect()
}

/// Inserts a DAT version, a title with `flags` named after the rom and one rom without
/// hashes, returning the rom id.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure, e.g. an unknown platform.
pub fn seed_rom(
    conn: &Connection,
    platform: &PlatformId,
    rom_name: &str,
    size: u64,
    flags: &[&str],
) -> Result<RomId> {
    conn.execute(
        "INSERT INTO dat_versions (platform_id, dat_name, version, source_file, loaded_at, game_count)
         VALUES (?1, ?1 || ' test', '1', 'test.dat', 0, 0)
         ON CONFLICT (dat_name, version) DO NOTHING",
        [&platform.as_str()],
    )?;
    let dat: i64 = conn.query_row(
        "SELECT id FROM dat_versions WHERE dat_name = ?1 || ' test'",
        [&platform.as_str()],
        |r| r.get(0),
    )?;
    let title = rom_name.rsplit_once('.').map_or(rom_name, |(t, _)| t);
    conn.execute(
        "INSERT INTO titles (platform_id, dat_version_id, name, base_name)
         VALUES (?1, ?2, ?3, ?3)",
        params![platform.as_str(), dat, title],
    )?;
    let title_id = TitleId::new(conn.last_insert_rowid());
    let flags: Vec<String> = flags.iter().map(|f| (*f).to_owned()).collect();
    super::titles::set_flags(conn, title_id, &flags)?;
    conn.execute(
        "INSERT INTO roms (title_id, name, size, status) VALUES (?1, ?2, ?3, 'good')",
        params![title_id, rom_name, sql::to_i64(size)],
    )?;
    Ok(RomId::new(conn.last_insert_rowid()))
}

/// Inserts a download row in `state`, standing in for the transfer poller.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn download(
    conn: &Connection,
    rom_id: RomId,
    source_id: SourceId,
    file_index: u32,
    state: DownloadState,
    staged_path: Option<&str>,
) -> Result<DownloadId> {
    conn.execute(
        "INSERT INTO downloads (title_id, rom_id, source_id, file_index, state, progress,
                                staged_path, created_at, updated_at)
         SELECT title_id, ?1, ?2, ?3, ?4, 1, ?5, 0, 0 FROM roms WHERE id = ?1",
        params![rom_id, source_id, file_index, state, staged_path],
    )?;
    Ok(DownloadId::new(conn.last_insert_rowid()))
}

/// A rom to write: its name, hashes and status.
type RomSpec = (String, Hashes, RomStatus);

/// A DAT version with titles and roms to write, started by [`dat`].
#[derive(Debug, Clone)]
pub struct Dat {
    platform: PlatformId,
    titles: Vec<(String, Vec<RomSpec>)>,
}

/// The ids a [`Dat`] wrote, in the order it listed them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    /// The DAT version holding the titles.
    pub dat_version: DatVersionId,
    /// Every title.
    pub titles: Vec<TitleId>,
    /// Every rom across all titles, title by title.
    pub roms: Vec<RomId>,
}

impl Written {
    /// The first rom written, for a fixture of one rom.
    ///
    /// # Panics
    ///
    /// When no rom was written.
    #[must_use]
    pub fn first_rom(&self) -> RomId {
        self.roms[0]
    }
}

/// Starts a DAT version on `platform`; add titles with [`Dat::title`] and write with
/// [`Dat::write`].
///
/// ```
/// use mistarr_core::{Hashes, PlatformId};
/// use mistarr_server::db::{fixtures, titles::RomStatus};
/// let conn = fixtures::conn();
/// let h = Hashes { size: 3, crc32: "352441c2".parse().expect("hex"), md5: "0".repeat(32).parse().expect("hex"), sha1: "0".repeat(40).parse().expect("hex") };
/// let w = fixtures::dat(&PlatformId::new("nes"))
///     .title("Example Quest (USA)")
///     .rom("a.nes", &h, RomStatus::Good)
///     .write(&conn)
///     .unwrap();
/// assert_eq!((w.titles.len(), w.roms.len()), (1, 1));
/// ```
#[must_use]
pub fn dat(platform: &PlatformId) -> Dat {
    Dat {
        platform: platform.clone(),
        titles: Vec::new(),
    }
}

impl Dat {
    /// Adds a title and returns it, to take roms.
    #[must_use]
    pub fn title(mut self, name: &str) -> DatTitle {
        self.titles.push((name.to_owned(), Vec::new()));
        DatTitle(self)
    }

    /// Writes the version, its titles and roms.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Db`] on SQLite failure.
    pub fn write(self, conn: &Connection) -> Result<Written> {
        let version: i64 =
            conn.query_row("SELECT COUNT(*) + 1 FROM dat_versions", [], |r| r.get(0))?;
        conn.execute(
            "INSERT INTO dat_versions (platform_id, dat_name, version, source_file, loaded_at, game_count)
             VALUES (?1, 'fixture', ?2, 'fixture.dat', 0, ?3)",
            params![self.platform.as_str(), version.to_string(), i64::try_from(self.titles.len()).unwrap_or(i64::MAX)],
        )?;
        let dat_version = DatVersionId::new(conn.last_insert_rowid());
        let mut written = Written {
            dat_version,
            titles: Vec::new(),
            roms: Vec::new(),
        };
        for (name, roms) in &self.titles {
            conn.execute(
                "INSERT INTO titles (platform_id, dat_version_id, name, base_name)
                 VALUES (?1, ?2, ?3, ?3)",
                params![self.platform.as_str(), dat_version, name],
            )?;
            let title = TitleId::new(conn.last_insert_rowid());
            conn.execute("UPDATE titles SET parent_id = ?1 WHERE id = ?1", [title])?;
            written.titles.push(title);
            for (rom, hashes, status) in roms {
                conn.execute(
                    "INSERT INTO roms (title_id, name, size, crc32, md5, sha1, status)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        title,
                        rom,
                        sql::to_i64(hashes.size),
                        hashes.crc32,
                        hashes.md5,
                        hashes.sha1,
                        status,
                    ],
                )?;
                written.roms.push(RomId::new(conn.last_insert_rowid()));
            }
        }
        Ok(written)
    }
}

/// A [`Dat`] whose last title takes roms.
#[derive(Debug, Clone)]
pub struct DatTitle(Dat);

impl DatTitle {
    /// Adds a rom to the current title.
    #[must_use]
    pub fn rom(mut self, name: &str, hashes: &Hashes, status: RomStatus) -> Self {
        if let Some((_, roms)) = self.0.titles.last_mut() {
            roms.push((name.to_owned(), *hashes, status));
        }
        self
    }

    /// Adds another title.
    #[must_use]
    pub fn title(self, name: &str) -> Self {
        self.0.title(name)
    }

    /// Writes the version; see [`Dat::write`].
    ///
    /// # Errors
    ///
    /// [`crate::Error::Db`] on SQLite failure.
    pub fn write(self, conn: &Connection) -> Result<Written> {
        self.0.write(conn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hashes(size: u64) -> Hashes {
        Hashes {
            size,
            crc32: "352441c2".parse().expect("hex"),
            md5: "900150983cd24fb0d6963f7d28e17f72".parse().expect("hex"),
            sha1: "a9993e364706816aba3e25717850c26c9cd0d89d"
                .parse()
                .expect("hex"),
        }
    }

    #[test]
    fn conn_has_the_platforms_and_catalog_writes_three_roms() {
        let c = conn();
        assert!(super::super::platforms::count(&c).expect("count") > 0);
        let roms = catalog(&c).expect("catalog");
        assert_eq!(roms.len(), 3);
        let size: i64 = c
            .query_row("SELECT size FROM roms WHERE id = ?1", [roms[1]], |r| {
                r.get(0)
            })
            .expect("size");
        assert_eq!(size, 24_592);
    }

    #[test]
    fn a_dat_writes_titles_with_their_roms_in_order() {
        let c = conn();
        let pid = PlatformId::new("nes");
        let w = dat(&pid)
            .title("One")
            .rom("a.nes", &hashes(3), RomStatus::Good)
            .rom("b.nes", &hashes(4), RomStatus::BadDump)
            .title("Two")
            .rom("c.nes", &hashes(5), RomStatus::Good)
            .write(&c)
            .expect("write");
        assert_eq!((w.titles.len(), w.roms.len()), (2, 3));
        let owner: TitleId = c
            .query_row(
                "SELECT title_id FROM roms WHERE id = ?1",
                [w.roms[2]],
                |r| r.get(0),
            )
            .expect("owner");
        assert_eq!(owner, w.titles[1]);
        let again = dat(&pid).title("One").write(&c).expect("second dat");
        assert_ne!(again.dat_version, w.dat_version);
    }

    #[test]
    fn seed_rom_and_download_write_one_row_each() {
        let c = conn();
        let rom = seed_rom(&c, &pid("nes"), "Example Quest (USA).nes", 8, &["bios"]).expect("rom");
        let src = crate::db::sources::insert(
            &c,
            &crate::db::sources::NewSource {
                infohash: &"0a".repeat(20),
                display_name: "Set",
                origin_file: "set.torrent",
                state: crate::db::sources::SourceState::Bound,
                reason: None,
                added_at: 0,
            },
        )
        .expect("source");
        let id = download(&c, rom, src, 0, DownloadState::Queued, None).expect("download");
        assert!(id.get() > 0);
    }
}
