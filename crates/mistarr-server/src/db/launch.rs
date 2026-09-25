//! What the launch route needs to know about a title; see `docs/API.md` "Launching".

use rusqlite::{Connection, OptionalExtension};

use super::titles::TitleId;
use crate::error::Result;

/// File states that mean a file holds the entry's content.
const LOADABLE: &str = "('verified', 'misnamed', 'bad')";

/// A title as the launch route sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // Each flag is a separate refusal reason.
pub struct LaunchTitle {
    /// Platform id.
    pub platform_id: String,
    /// The DAT entry is flagged `bios`.
    pub bios: bool,
    /// `mra` for an arcade title read from an MRA file, else `dat`.
    pub source: String,
    /// The MRA file relative to `_Arcade`, for an MRA title.
    pub mra_path: Option<String>,
    /// Every live rom has a file on disk: a loadable file for a DAT entry, a
    /// present zip with no failed md5 check for an MRA title.
    pub complete: bool,
    /// Every live rom has a `verified` file; a disc launches only then.
    pub all_verified: bool,
    /// `rel_path` of the loadable files of its live roms, verified first.
    pub files: Vec<String>,
}

/// The launch facts of title `id`, or `None` when there is no such title.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::{launch, titles::TitleId};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert!(launch::title(&conn, TitleId(1)).unwrap().is_none());
/// ```
pub fn title(conn: &Connection, id: TitleId) -> Result<Option<LaunchTitle>> {
    let row = conn
        .query_row(
            &format!(
                "SELECT t.platform_id,
                        EXISTS (SELECT 1 FROM title_flags f WHERE f.title_id = t.id AND f.flag = 'bios'),
                        t.source, t.mra_path,
                        (SELECT COUNT(*) FROM roms r WHERE r.title_id = t.id AND r.retired = 0),
                        (SELECT COUNT(*) FROM roms r WHERE r.title_id = t.id AND r.retired = 0
                           AND EXISTS (SELECT 1 FROM files f
                                       WHERE f.rom_id = r.id AND f.state IN {LOADABLE})),
                        (SELECT COUNT(*) FROM roms r
                         WHERE r.title_id = t.id AND r.retired = 0 AND r.present = 1),
                        COALESCE(t.mra_check, '') IN ('mismatch', 'missing_part'),
                        (SELECT COUNT(*) FROM roms r WHERE r.title_id = t.id AND r.retired = 0
                           AND EXISTS (SELECT 1 FROM files f
                                       WHERE f.rom_id = r.id AND f.state = 'verified'))
                 FROM titles t WHERE t.id = ?1"
            ),
            [id.0],
            |r| {
                Ok((
                    LaunchTitle {
                        platform_id: r.get(0)?,
                        bios: r.get(1)?,
                        source: r.get(2)?,
                        mra_path: r.get(3)?,
                        complete: false,
                        all_verified: false,
                        files: Vec::new(),
                    },
                    r.get::<_, i64>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, bool>(7)?,
                    r.get::<_, i64>(8)?,
                ))
            },
        )
        .optional()?;
    let Some((mut title, roms, with_file, present, check_failed, verified)) = row else {
        return Ok(None);
    };
    title.all_verified = roms > 0 && verified == roms;
    title.complete = roms > 0
        && if title.source == "mra" {
            present == roms && !check_failed
        } else {
            with_file == roms
        };
    let mut stmt = conn.prepare(&format!(
        "SELECT f.rel_path FROM roms r JOIN files f ON f.rom_id = r.id
         WHERE r.title_id = ?1 AND r.retired = 0 AND f.state IN {LOADABLE}
         ORDER BY CASE f.state WHEN 'verified' THEN 0 WHEN 'misnamed' THEN 1 ELSE 2 END,
                  r.name, f.id"
    ))?;
    title.files = stmt
        .query_map([id.0], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(Some(title))
}

#[cfg(test)]
mod tests {
    use mistarr_core::{HashSet, PlatformId};

    use super::*;
    use crate::db::files::{self, FileState, Hashed};

    fn conn() -> Connection {
        let mut c = Connection::open_in_memory().expect("open");
        crate::db::migrate::apply(&mut c).expect("migrate");
        crate::db::platforms::seed(&mut c, &mistarr_mister::platforms::PLATFORMS).expect("seed");
        c
    }

    fn hashes() -> HashSet {
        HashSet {
            size: 4,
            crc32: "00000001".into(),
            md5: "0".repeat(32),
            sha1: "1".repeat(40),
        }
    }

    fn file(c: &Connection, pid: &PlatformId, rel: &str, rom: i64, state: FileState) {
        files::upsert(c, pid, rel, 4, 0, &Hashed::default(), Some(rom), state, 0).expect("file");
    }

    #[test]
    fn disc_title_is_complete_only_with_every_track() {
        let c = conn();
        let pid = PlatformId("psx".into());
        let t = files::seed_title_fixture(&c, &pid, "Example Disc (USA)").expect("title");
        let cue =
            files::seed_rom_for_title_fixture(&c, t, "g.cue", &hashes(), "good").expect("rom");
        let bin =
            files::seed_rom_for_title_fixture(&c, t, "g.bin", &hashes(), "good").expect("rom");
        file(&c, &pid, "PSX/G/g.cue", cue, FileState::Verified);
        let got = title(&c, TitleId(t)).expect("read").expect("title");
        assert!(!got.complete && !got.all_verified);
        assert_eq!(got.files, ["PSX/G/g.cue"]);
        file(&c, &pid, "PSX/G/g.bin", bin, FileState::Misnamed);
        let got = title(&c, TitleId(t)).expect("read").expect("title");
        assert!(got.complete && !got.all_verified && !got.bios);
        assert_eq!(got.files, ["PSX/G/g.cue", "PSX/G/g.bin"]);
        assert_eq!(
            (got.platform_id.as_str(), got.source.as_str()),
            ("psx", "dat")
        );
        file(&c, &pid, "PSX/G/g.bin", bin, FileState::Verified);
        assert!(
            title(&c, TitleId(t))
                .expect("read")
                .expect("title")
                .all_verified
        );
    }

    #[test]
    fn unverified_and_pending_files_do_not_count() {
        let c = conn();
        let pid = PlatformId("nes".into());
        let rom = files::seed_rom_fixture(&c, &pid, "Example Quest", "a.nes", &hashes(), "good")
            .expect("rom");
        let t: i64 = c
            .query_row("SELECT title_id FROM roms WHERE id = ?1", [rom], |r| {
                r.get(0)
            })
            .expect("title");
        file(&c, &pid, "NES/a.nes", rom, FileState::Pending);
        let got = title(&c, TitleId(t)).expect("read").expect("title");
        assert!(!got.complete && got.files.is_empty());
        crate::db::titles::set_flags(&c, TitleId(t), &["bios".to_owned()]).expect("flag");
        assert!(title(&c, TitleId(t)).expect("read").expect("title").bios);
    }

    #[test]
    fn mra_title_needs_every_zip_and_no_failed_check() {
        let c = conn();
        let pid = PlatformId("arcade".into());
        let t = files::seed_title_fixture(&c, &pid, "Example Blaster").expect("title");
        c.execute(
            "UPDATE titles SET source = 'mra', mra_path = 'Example Blaster.mra' WHERE id = ?1",
            [t],
        )
        .expect("mra");
        let rom =
            files::seed_rom_for_title_fixture(&c, t, "exb.zip", &hashes(), "good").expect("rom");
        let got = title(&c, TitleId(t)).expect("read").expect("title");
        assert!(!got.complete);
        assert_eq!(got.mra_path.as_deref(), Some("Example Blaster.mra"));
        c.execute("UPDATE roms SET present = 1 WHERE id = ?1", [rom])
            .expect("present");
        assert!(
            title(&c, TitleId(t))
                .expect("read")
                .expect("title")
                .complete
        );
        c.execute(
            "UPDATE titles SET mra_check = 'mismatch' WHERE id = ?1",
            [t],
        )
        .expect("check");
        assert!(
            !title(&c, TitleId(t))
                .expect("read")
                .expect("title")
                .complete
        );
    }

    #[test]
    fn a_chd_identified_by_its_tracks_counts_as_verified() {
        let c = conn();
        let pid = PlatformId("psx".into());
        let title = files::seed_title_fixture(&c, &pid, "Disc").expect("title");
        let cue = files::seed_rom_for_title_fixture(&c, title, "Disc.cue", &hashes(), "good")
            .expect("cue");
        let bin = files::seed_rom_for_title_fixture(&c, title, "Disc.bin", &hashes(), "good")
            .expect("bin");
        file(&c, &pid, "PSX/Disc/Disc.chd#01", bin, FileState::Verified);
        let t = super::title(&c, TitleId(title))
            .expect("read")
            .expect("title");
        assert!(!t.all_verified, "the cue row is still missing");
        file(&c, &pid, "PSX/Disc/Disc.chd#cue", cue, FileState::Verified);
        let t = super::title(&c, TitleId(title))
            .expect("read")
            .expect("title");
        assert!(t.all_verified && t.complete);
        assert!(t.files.iter().all(|f| f.starts_with("PSX/Disc/Disc.chd#")));
    }
}
