//! Matching files to roms through the database and the states they take, shared by the
//! scan, the recompute, CHD images and the importer; see `docs/VERIFICATION.md`.

use std::collections::HashMap;

use mistarr_core::hash::{HeaderForms, HeaderRule};
use mistarr_core::{HashSet as Hashes, PlatformId};
use mistarr_mister::platforms::{self, Kind};
use rusqlite::Connection;

use crate::db::files::{self, FileRow, FileState, NewFile};
use crate::db::ids::{RomId, TitleId};
use crate::db::roms::{self, RomMatch};
use crate::db::titles::RomStatus;
use crate::error::Result;

/// Matches a fully hashed payload in its forms and decides its state, per
/// `docs/DATA-MODEL.md` "files.state".
///
/// # Errors
///
/// [`crate::error::Error::Db`] on SQLite failure.
pub(crate) fn classify(
    conn: &Connection,
    platform_id: &PlatformId,
    actual_name: &str,
    forms: &HeaderForms,
) -> Result<(Option<RomId>, FileState)> {
    let m = match_forms(
        conn,
        platform_id,
        forms.whole.iter().chain([&forms.content]),
    )?;
    Ok(cartridge_state(platform_id, m.as_ref(), actual_name))
}

/// The rom `forms` match under `docs/VERIFICATION.md` "Matching order" in each, a caller
/// passing the whole file before its content: the first form to match a live rom, else
/// the first to match a retired one, so a live rom of any form wins over a retired one.
///
/// # Errors
///
/// [`crate::error::Error::Db`] on SQLite failure.
pub(crate) fn match_forms<'a>(
    conn: &Connection,
    platform_id: &PlatformId,
    forms: impl IntoIterator<Item = &'a Hashes>,
) -> Result<Option<RomMatch>> {
    let forms: Vec<&Hashes> = forms.into_iter().collect();
    let size = |h: &Hashes| i64::try_from(h.size).unwrap_or(i64::MAX);
    for h in &forms {
        let (sha1, md5, crc32) = (&h.sha1, &h.md5, &h.crc32);
        if let Some(m) = roms::match_live_rom(conn, platform_id, sha1, md5, crc32, size(h))? {
            return Ok(Some(m));
        }
    }
    for h in &forms {
        let (sha1, md5, crc32) = (&h.sha1, &h.md5, &h.crc32);
        if let Some(m) = roms::match_rom(conn, platform_id, sha1, md5, crc32, size(h))? {
            return Ok(Some(m));
        }
    }
    Ok(None)
}

/// The rom id and state a cartridge file or zip member of `platform` named `own_name`
/// takes from its match: `bad` for a bad dump, else `verified` or `misnamed` by [`name_fits`].
pub(crate) fn cartridge_state(
    platform: &PlatformId,
    m: Option<&RomMatch>,
    own_name: &str,
) -> (Option<RomId>, FileState) {
    let Some(m) = m else {
        return (None, FileState::Unverified);
    };
    let state = if m.status == RomStatus::BadDump {
        FileState::Bad
    } else if name_fits(platform, &m.name, &m.game, own_name) {
        FileState::Verified
    } else {
        FileState::Misnamed
    };
    (Some(m.rom_id), state)
}

/// Whether `own_name` is a name the adapter expects for the rom named `rom_name` of the
/// game `game` on `platform`, per `docs/VERIFICATION.md` "File names": the rom's file
/// name, or its stem or the game's placed name with an extension the
/// platform loads that is the rom's, the one placement writes, or any when the platform
/// does not load the rom's.
///
/// ```
/// use mistarr_core::PlatformId;
/// use mistarr_server::jobs::matching::name_fits;
/// let (nes, game) = (PlatformId("nes".into()), "Example Quest (USA)");
/// assert!(name_fits(&nes, "Example Quest (USA).nes", game, "Example Quest (USA).nes"));
/// assert!(name_fits(&nes, "Example Quest (USA).unh", game, "Example Quest (USA).nes"));
/// assert!(!name_fits(&nes, "Example Quest (USA).unh", game, "Example Quest (Japan).nes"));
/// ```
#[must_use]
pub fn name_fits(platform: &PlatformId, rom_name: &str, game: &str, own_name: &str) -> bool {
    let rom_name = files::basename(rom_name);
    if rom_name == own_name {
        return true;
    }
    let (Some(row), Some((own_stem, own_ext))) =
        (platforms::by_id(&platform.0), split_extension(own_name))
    else {
        return false;
    };
    let loads = |ext: &str| {
        row.load_extensions
            .iter()
            .chain(row.extension_written.iter())
            .any(|e| e.eq_ignore_ascii_case(ext))
    };
    if !loads(own_ext) {
        return false;
    }
    let (rom_stem, rom_ext) = split_extension(rom_name).unwrap_or((rom_name, ""));
    let written = row
        .extension_written
        .is_some_and(|e| e.eq_ignore_ascii_case(own_ext));
    let ext_fits = own_ext.eq_ignore_ascii_case(rom_ext) || written || !loads(rom_ext);
    let placed =
        || written && mistarr_mister::adapter::safe_name(game).is_ok_and(|g| g == own_stem);
    ext_fits && (own_stem == rom_stem || placed())
}

/// Splits a file name at a final dot followed by what reads as an extension: one to
/// four ASCII letters or digits, at least one a letter. `None` when there is none.
fn split_extension(name: &str) -> Option<(&str, &str)> {
    let (stem, ext) = name.rsplit_once('.')?;
    let looks = !stem.is_empty()
        && (1..=4).contains(&ext.len())
        && ext.bytes().all(|b| b.is_ascii_alphanumeric())
        && ext.bytes().any(|b| b.is_ascii_alphabetic());
    looks.then_some((stem, ext))
}

/// Marks `verified` every `misnamed` cartridge file whose name [`name_fits`] its rom,
/// as a scan would decide it now; returns how many. Disc tracks keep their state.
///
/// # Errors
///
/// [`crate::error::Error::Db`] on SQLite failure.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert_eq!(mistarr_server::jobs::matching::settle_names(&conn).unwrap(), 0);
/// ```
pub fn settle_names(conn: &Connection) -> Result<usize> {
    let mut settled = 0;
    for row in files::misnamed(conn)? {
        let disc = platforms::by_id(&row.platform_id.0).is_some_and(|p| p.kind == Kind::Disc);
        if !disc
            && name_fits(
                &row.platform_id,
                &row.rom_name,
                &row.game,
                own_name(&row.rel_path),
            )
        {
            files::set_match(conn, row.id, Some(row.rom_id), FileState::Verified)?;
            settled += 1;
        }
    }
    Ok(settled)
}

/// The name a row's file or zip member has, compared against the rom's name.
pub(crate) fn own_name(rel_path: &str) -> &str {
    files::basename(rel_path.rsplit_once('#').map_or(rel_path, |(_, m)| m))
}

/// The live rom a fully hashed row's stored hashes match, per `docs/VERIFICATION.md`
/// "Matching stored hashes"; a row without a sha1 or md5 never matches. A row with
/// whole-file hashes that differ from its hashes tries the whole file at its size first,
/// then its content at the size less the header. Otherwise the hashes are of the content
/// after the row's header rule while `size` is the size on disk, so the CRC32 tier also
/// tries the size less the header that rule strips.
///
/// # Errors
///
/// [`crate::error::Error::Db`] on SQLite failure.
pub(crate) fn stored_match(
    conn: &Connection,
    platform_id: &PlatformId,
    f: &FileRow,
) -> Result<Option<RomMatch>> {
    if f.md5.is_none() && f.sha1.is_none() {
        return Ok(None);
    }
    let hash = |h: &Option<String>| h.clone().unwrap_or_default();
    let (sha1, md5, crc32) = (hash(&f.sha1), hash(&f.md5), hash(&f.crc32));
    let rule = f
        .header_rule
        .as_deref()
        .and_then(|n| n.parse::<HeaderRule>().ok())
        .unwrap_or_default();
    let header = i64::try_from(rule.header_len()).unwrap_or(0);
    let w = &f.whole;
    let has_whole = w.sha1.is_some() || w.md5.is_some();
    if has_whole && (&w.sha1, &w.md5) != (&f.sha1, &f.md5) {
        let (wsha1, wmd5, wcrc) = (hash(&w.sha1), hash(&w.md5), hash(&w.crc32));
        if let Some(m) = roms::match_live_rom(conn, platform_id, &wsha1, &wmd5, &wcrc, f.size)? {
            return Ok(Some(m));
        }
        let size = f.size - header;
        return roms::match_live_rom(conn, platform_id, &sha1, &md5, &crc32, size);
    }
    if let Some(m) = roms::match_live_rom(conn, platform_id, &sha1, &md5, &crc32, f.size)? {
        return Ok(Some(m));
    }
    if has_whole {
        // No header was found: the hashes already are the whole file's.
        return Ok(None);
    }
    let stripped = match rule {
        HeaderRule::Smc => f.size % 1024 == 512,
        _ => header > 0 && f.size > header,
    };
    if !stripped || crc32.is_empty() {
        return Ok(None);
    }
    // The hash tiers failed above whatever the size; only the CRC32 tier is left.
    roms::match_live_rom(conn, platform_id, "", "", &crc32, f.size - header)
}

/// One hashed track of a disc game directory, before the all-or-nothing rule
/// decides its final state. `hashes` is `None` when the track could not be
/// read; it is then always `unverified`.
pub(crate) struct Track {
    pub(crate) rel_path: String,
    pub(crate) name: String,
    pub(crate) size: i64,
    pub(crate) mtime: i64,
    pub(crate) hashes: Option<Hashes>,
    pub(crate) matched: Option<RomMatch>,
}

/// Decides each track's final state from the all-or-nothing rule, evaluated
/// once per matched title rather than once for the whole directory.
///
/// # Errors
///
/// [`crate::error::Error::Db`] on SQLite failure.
pub(crate) fn classify_disc_tracks(conn: &Connection, tracks: Vec<Track>) -> Result<Vec<NewFile>> {
    let mut groups: HashMap<TitleId, Vec<usize>> = HashMap::new();
    for (i, t) in tracks.iter().enumerate() {
        if let Some(m) = &t.matched {
            groups.entry(m.title_id).or_default().push(i);
        }
    }
    let mut complete: HashMap<TitleId, bool> = HashMap::new();
    for (&title_id, idxs) in &groups {
        let want = roms::count_roms_for_title(conn, title_id)?;
        let ok = i64::try_from(idxs.len()).unwrap_or(-1) == want
            && idxs.iter().all(|&i| {
                tracks[i]
                    .matched
                    .as_ref()
                    .is_some_and(|m| m.status != RomStatus::BadDump)
            });
        complete.insert(title_id, ok);
    }

    let mut rows = Vec::with_capacity(tracks.len());
    for t in tracks {
        let (rom_id, state) = match &t.matched {
            None => (None, FileState::Unverified),
            Some(m) if m.status == RomStatus::BadDump => (Some(m.rom_id), FileState::Bad),
            Some(m) => {
                let is_complete = complete.get(&m.title_id).copied().unwrap_or(false);
                let state = if !is_complete {
                    FileState::Unverified
                } else if files::basename(&m.name) == t.name {
                    FileState::Verified
                } else {
                    FileState::Misnamed
                };
                (Some(m.rom_id), state)
            }
        };
        let (crc32, md5, sha1, header_rule) = match t.hashes {
            Some(h) => (
                Some(h.crc32),
                Some(h.md5),
                Some(h.sha1),
                Some("none".to_owned()),
            ),
            None => (None, None, None, None),
        };
        rows.push(NewFile {
            rel_path: t.rel_path,
            size: t.size,
            mtime: t.mtime,
            crc32,
            md5,
            sha1,
            header_rule,
            whole: files::WholeHashes::default(),
            rom_id,
            state,
            reason: None,
        });
    }
    Ok(rows)
}

/// Matches `rows` again by their stored hashes against live roms and returns how many
/// went from no rom to a rom. A cartridge file takes the state a scan would give it; a
/// disc track is classified again with the other tracks of its directory by the scan's
/// all-or-nothing rule, and a CHD's tracks together as its own set.
///
/// # Errors
///
/// [`crate::error::Error::Db`] on SQLite failure.
pub(crate) fn set_matches(
    conn: &Connection,
    platform: &PlatformId,
    rows: &[FileRow],
) -> Result<usize> {
    let disc = platforms::by_id(&platform.0).is_some_and(|p| p.kind == Kind::Disc);
    let mut matched = 0;
    let mut units: Vec<&str> = Vec::new();
    for f in rows {
        if let Some((dir, _)) = f.rel_path.rsplit_once('/').filter(|_| disc) {
            if !units.contains(&dir) {
                units.push(dir);
            }
            continue;
        }
        let m = stored_match(conn, platform, f)?;
        let (rom, state) = cartridge_state(platform, m.as_ref(), own_name(&f.rel_path));
        matched += usize::from(f.rom_id.is_none() && rom.is_some());
        set_changed(conn, f, rom, state)?;
    }
    for dir in units {
        let (rows, containers) =
            super::chd::split_disc_rows(files::in_directory(conn, platform, dir)?);
        for c in containers {
            matched += super::chd::rematch_container(conn, platform, &c, crate::unix_now())?;
        }
        let mut tracks = Vec::with_capacity(rows.len());
        for f in &rows {
            tracks.push(Track {
                rel_path: f.rel_path.clone(),
                name: files::basename(&f.rel_path).to_owned(),
                size: f.size,
                mtime: f.mtime,
                hashes: f.hashes(),
                matched: stored_match(conn, platform, f)?,
            });
        }
        for (f, t) in rows.iter().zip(classify_disc_tracks(conn, tracks)?) {
            matched += usize::from(f.rom_id.is_none() && t.rom_id.is_some());
            set_changed(conn, f, t.rom_id, t.state)?;
        }
    }
    Ok(matched)
}

/// [`files::set_match`] only when the rom or state differs, so a recompute that changes
/// nothing writes nothing.
fn set_changed(conn: &Connection, f: &FileRow, rom: Option<RomId>, state: FileState) -> Result<()> {
    if f.rom_id == rom && f.state == state {
        return Ok(());
    }
    files::set_match(conn, f.id, rom, state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::fixtures::conn;
    use mistarr_core::hash::hash_forms;

    fn hashes() -> Hashes {
        Hashes {
            size: 4,
            crc32: "0a0b0c0d".into(),
            md5: "0".repeat(32),
            sha1: "1".repeat(40),
        }
    }

    #[test]
    fn file_names_fit_the_rom_or_what_placement_writes() {
        let game = "Example Quest (USA)";
        // (platform, rom name, file name, fits)
        let cases = [
            (
                "nes",
                "Example Quest (USA).nes",
                "Example Quest (USA).nes",
                true,
            ),
            (
                "nes",
                "Example Quest (USA).unh",
                "Example Quest (USA).nes",
                true,
            ),
            (
                "nes",
                "Example Quest (USA).unh",
                "Example Quest (USA).NES",
                true,
            ),
            (
                "nes",
                "Example Quest (USA).NES",
                "Example Quest (USA).nes",
                true,
            ),
            (
                "nes",
                "Example Quest (USA)",
                "Example Quest (USA).nes",
                true,
            ),
            (
                "nes",
                "sub/Example Quest (USA).nes",
                "Example Quest (USA).nes",
                true,
            ),
            ("nes", "Example Quest v1.1", "Example Quest v1.1.nes", true),
            ("nes", "Example Quest v1.1", "Example Quest v1.nes", false),
            (
                "nes",
                "Example Quest (USA).unh",
                "Example Quest (USA).unh.zip",
                false,
            ),
            (
                "nes",
                "Example Quest (USA).unh",
                "Example Quest (Japan).nes",
                false,
            ),
            (
                "nes",
                "Example Quest (USA).unh",
                "Example Quest (USA).fds",
                false,
            ),
            ("nes", "q.nes", "Example Quest (USA).nes", true),
            ("nes", "q.nes", "Other Game (USA).nes", false),
            (
                "snes",
                "Example Quest (USA).smc",
                "Example Quest (USA).sfc",
                true,
            ),
            (
                "snes",
                "Example Quest (USA).sfc",
                "Example Quest (USA).smc",
                false,
            ),
            ("snes", "q.smc", "Example Quest (USA).smc", false),
            (
                "nowhere",
                "Example Quest (USA).unh",
                "Example Quest (USA).nes",
                false,
            ),
        ];
        for (platform, rom, file, fits) in cases {
            let p = PlatformId(platform.into());
            assert_eq!(
                name_fits(&p, rom, game, file),
                fits,
                "{platform}: {rom} as {file}"
            );
        }
    }

    #[test]
    fn extensions_need_a_letter_and_at_most_four_characters() {
        assert_eq!(split_extension("a.unh"), Some(("a", "unh")));
        assert_eq!(split_extension("a.32x"), Some(("a", "32x")));
        assert_eq!(split_extension("Example Quest v1.1"), None);
        assert_eq!(split_extension("a (b.c d)"), None);
        assert_eq!(split_extension("a.toolong"), None);
        assert_eq!(split_extension(".nes"), None);
    }

    #[test]
    fn own_names_are_the_file_or_zip_member_leaf() {
        assert_eq!(own_name("NES/a.nes"), "a.nes");
        assert_eq!(own_name("NES/a.zip#sub/b.nes"), "b.nes");
        assert_eq!(own_name("PSX/Disc (USA)/t.bin"), "t.bin");
    }

    #[test]
    fn a_match_decides_a_cartridge_state_by_status_and_name() {
        let nes = PlatformId("nes".into());
        let m = |status| RomMatch {
            rom_id: RomId(7),
            title_id: TitleId(1),
            name: "Example Quest (USA).nes".into(),
            status,
            game: "Example Quest (USA)".into(),
        };
        let good = m(RomStatus::Good);
        assert_eq!(
            cartridge_state(&nes, None, "a.nes"),
            (None, FileState::Unverified)
        );
        assert_eq!(
            cartridge_state(&nes, Some(&good), "Example Quest (USA).nes"),
            (Some(RomId(7)), FileState::Verified)
        );
        assert_eq!(
            cartridge_state(&nes, Some(&good), "Other (USA).nes"),
            (Some(RomId(7)), FileState::Misnamed)
        );
        assert_eq!(
            cartridge_state(&nes, Some(&m(RomStatus::BadDump)), "a.nes"),
            (Some(RomId(7)), FileState::Bad)
        );
    }

    #[test]
    fn settling_names_verifies_only_files_that_now_fit() {
        let c = conn();
        let nes = PlatformId("nes".into());
        let h = hashes();
        let rom = crate::db::fixtures::dat(&nes)
            .title("Example Quest (USA)")
            .rom("Example Quest (USA).unh", &h, RomStatus::Good)
            .write(&c)
            .expect("rom")
            .first_rom();
        let row = |rel: &str| NewFile {
            rel_path: rel.to_owned(),
            size: 20,
            mtime: 1,
            crc32: Some(h.crc32.clone()),
            md5: Some(h.md5.clone()),
            sha1: Some(h.sha1.clone()),
            header_rule: Some("ines".into()),
            rom_id: Some(rom),
            state: FileState::Misnamed,
            reason: None,
            whole: files::WholeHashes::default(),
        };
        let fits =
            files::upsert(&c, &nes, &row("NES/q.zip#Example Quest (USA).nes"), 1).expect("row");
        let other = files::upsert(&c, &nes, &row("NES/Other Name.nes"), 1).expect("row");
        let upper = files::upsert(&c, &nes, &row("NES/Example Quest (USA).NES"), 1).expect("row");
        let psx = PlatformId("psx".into());
        let track = crate::db::fixtures::dat(&psx)
            .title("Example Disc (USA)")
            .rom("Example Disc (USA).img", &h, RomStatus::Good)
            .write(&c)
            .expect("rom")
            .first_rom();
        let disc = NewFile {
            rom_id: Some(track),
            ..row("PSX/Example Disc (USA)/Example Disc (USA).cue")
        };
        let disc = files::upsert(&c, &psx, &disc, 1).expect("row");
        assert!(name_fits(
            &psx,
            "Example Disc (USA).img",
            "Example Disc (USA)",
            "Example Disc (USA).cue"
        ));
        assert_eq!(settle_names(&c).expect("settle"), 2);
        let state = |id| files::get(&c, id).expect("get").expect("row").state;
        assert_eq!(state(fits), FileState::Verified);
        assert_eq!(state(upper), FileState::Verified, "upper-case extension");
        assert_eq!(state(other), FileState::Misnamed);
        assert_eq!(
            state(disc),
            FileState::Misnamed,
            "disc tracks are left alone"
        );
        assert_eq!(
            settle_names(&c).expect("settle"),
            0,
            "nothing left to settle"
        );
    }

    #[test]
    fn a_live_rom_of_the_content_beats_a_retired_rom_of_the_whole_file() {
        let c = conn();
        let nes = PlatformId("nes".into());
        let mut file = b"NES\x1a".to_vec();
        file.resize(16, 0);
        file.extend_from_slice(b"synthetic body of a retired and a live rom");
        let forms = hash_forms(&file[..], HeaderRule::Ines, None).expect("hash");
        let whole = forms.whole.clone().expect("a header");
        let retired = crate::db::fixtures::dat(&nes)
            .title("Old (USA)")
            .rom("Old (USA).nes", &whole, RomStatus::Good)
            .write(&c)
            .expect("retired rom")
            .first_rom();
        c.execute("UPDATE roms SET retired = 1 WHERE id = ?1", [retired])
            .expect("retire");
        let live = crate::db::fixtures::dat(&nes)
            .title("New (USA)")
            .rom("New (USA).nes", &forms.content, RomStatus::Good)
            .write(&c)
            .expect("live rom")
            .first_rom();
        let (rom, _) = classify(&c, &nes, "New (USA).nes", &forms).expect("classify");
        assert_eq!(rom, Some(live));
        c.execute("UPDATE roms SET retired = 1 WHERE id = ?1", [live])
            .expect("retire");
        let (rom, _) = classify(&c, &nes, "Old (USA).nes", &forms).expect("classify");
        assert_eq!(
            rom,
            Some(retired),
            "a retired rom still matches when nothing live does"
        );
    }

    #[test]
    fn a_disc_verifies_only_when_every_track_of_its_title_matched() {
        let c = conn();
        let psx = PlatformId("psx".into());
        let h = hashes();
        let rom = crate::db::fixtures::dat(&psx)
            .title("Disc (USA)")
            .rom("Disc (USA).bin", &h, RomStatus::Good)
            .write(&c)
            .expect("rom")
            .first_rom();
        let m = match_forms(&c, &psx, [&h]).expect("match").expect("a rom");
        assert_eq!(m.rom_id, rom);
        let track = |name: &str, matched: Option<RomMatch>| Track {
            rel_path: format!("PSX/Disc (USA)/{name}"),
            name: name.to_owned(),
            size: 4,
            mtime: 1,
            hashes: Some(h.clone()),
            matched,
        };
        let rows = classify_disc_tracks(&c, vec![track("Disc (USA).bin", Some(m.clone()))])
            .expect("classify");
        assert_eq!(rows[0].state, FileState::Verified);
        assert_eq!(rows[0].header_rule.as_deref(), Some("none"));
        let rows =
            classify_disc_tracks(&c, vec![track("other.bin", Some(m)), track("x.m3u", None)])
                .expect("classify");
        let states: Vec<_> = rows.iter().map(|r| r.state).collect();
        assert_eq!(states, [FileState::Misnamed, FileState::Unverified]);
    }
}
