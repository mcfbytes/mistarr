//! Imports a zip an MRA names: verified by the MRA's md5, a loaded MAME DAT or nothing,
//! then placed whole; see `docs/ARCHITECTURE.md` "Import".

use std::fs;
use std::path::{Path, PathBuf};

use mistarr_mister::adapter::arcade::assemble::PartSource as _;
use mistarr_mister::adapter::arcade::mra::{
    self, zip_location, Mra, MraRom, Part, RomItem, ZipPath,
};
use mistarr_mister::adapter::arcade::zip_placement;
use mistarr_mister::{StagedFile, StagedKind};
use serde_json::{json, Value};

use super::support::{is_zip, match_members, rel_string, Hashed};
use super::{fail, finish, task, Piece, Placing, Why, BIOS_REFUSED};
use crate::db::arcade as arcade_rows;
use crate::db::downloads::{DownloadRow, DownloadState};
use crate::db::files::{self, FileState};
use crate::db::imports::{self, EntryRom, TitleEntry};
use crate::error::Result;
use crate::jobs::arcade::{self, check_rom, same_zip, Check, ZipIndex, ZipSource};

/// What the MRA says about a staged zip.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Verdict {
    /// Every `<rom>` index the zip feeds matched its md5; the members the matching
    /// alternatives read, by zip path.
    Match(Vec<(PathBuf, String)>),
    /// The md5 applies but cannot decide this zip yet, and why.
    Open(String),
    /// A `<rom>` the zip feeds does not match its md5.
    Mismatch(String),
    /// Parts naming this zip that are in none of their zips.
    Missing(Vec<String>),
    /// The assembler does not implement what the MRA asks for.
    Refused(String),
    /// Some `<rom>` index the zip feeds carries no md5.
    NoMd5,
}

/// The parts of a `<rom>`, inside interleaves too.
fn parts(rom: &MraRom) -> Vec<&Part> {
    let mut out = Vec::new();
    for item in &rom.items {
        match item {
            RomItem::Part(p) => out.push(p),
            RomItem::Interleave(il) => out.extend(il.parts.iter()),
            _ => {}
        }
    }
    out
}

/// The zips a part is read from, in the order MiSTer tries them.
fn zips_of<'a>(rom: &'a MraRom, part: &'a Part) -> &'a [String] {
    if part.zips.is_empty() {
        &rom.zips
    } else {
        &part.zips
    }
}

fn names(list: &[String], zip: &ZipPath) -> bool {
    list.iter()
        .any(|z| zip_location(z).is_some_and(|l| same_zip(&l, zip)))
}

/// Whether `rom` reads anything from `zip`.
fn feeds(rom: &MraRom, zip: &ZipPath) -> bool {
    names(&rom.zips, zip) || parts(rom).iter().any(|p| names(&p.zips, zip))
}

/// Named parts whose zips include `zip` that are in none of them; a part is not counted
/// while one of its other zips is not on disk, as it may come from there.
fn missing_members(roms: &[&MraRom], zip: &ZipPath, src: &mut ZipSource<'_>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for rom in roms {
        for part in parts(rom) {
            let (Some(name), list) = (&part.name, zips_of(rom, part)) else {
                continue;
            };
            if !names(list, zip) || out.contains(name) {
                continue;
            }
            let (mut found, mut open) = (false, false);
            for z in list {
                if src.locate(z).is_none() {
                    open = true;
                    continue;
                }
                match src.open(z, name, part.crc) {
                    Ok(Some(_)) => {
                        found = true;
                        break;
                    }
                    Ok(None) => {}
                    Err(_) => open = true,
                }
            }
            if !found && !open {
                out.push(name.clone());
            }
        }
    }
    out
}

/// Outcome of one `<rom>` index, worst last.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Outcome {
    Match,
    Open(String),
    Mismatch(String),
    Refused(String),
}

/// Checks the staged zip `staged`, standing in for `zip`, against `mra` with the zips
/// already under `games`.
fn examine(mra: &Mra, zip: &ZipPath, staged: &Path, games: &Path) -> Verdict {
    let fed: Vec<&MraRom> = mra.roms.iter().filter(|r| feeds(r, zip)).collect();
    let dirs = mra.zip_paths().into_iter().map(|z| z.dir).collect();
    let index = ZipIndex::build(games, dirs);
    let stand_in = Some((zip.clone(), staged.to_path_buf()));
    let missing = missing_members(&fed, zip, &mut ZipSource::new(&index, stand_in.clone()));
    if !missing.is_empty() {
        return Verdict::Missing(missing);
    }
    let covered = |r: &&MraRom| fed.iter().any(|o| o.index == r.index && o.md5.is_some());
    if fed.is_empty() || !fed.iter().all(covered) {
        return Verdict::NoMd5;
    }
    let mut src = ZipSource::new(&index, stand_in);
    let mut by_index: Vec<(u32, Outcome)> = Vec::new();
    let mut matched: Vec<(PathBuf, String)> = Vec::new();
    for rom in &fed {
        let Some(expected) = &rom.md5 else {
            continue;
        };
        let before = src.read.len();
        let (check, detail) = check_rom(rom, expected, &mut src);
        let detail = detail.unwrap_or_default();
        let outcome = match check {
            Check::Match => {
                matched.extend(src.read.drain(before..));
                Outcome::Match
            }
            Check::Mismatch => Outcome::Mismatch(detail),
            Check::Refused => Outcome::Refused(detail),
            Check::MissingPart => {
                let waiting: Vec<String> = std::iter::once(&rom.zips)
                    .chain(parts(rom).into_iter().map(|p| &p.zips))
                    .flatten()
                    .filter(|z| src.locate(z).is_none())
                    .filter_map(|z| zip_location(z).map(|l| l.rel_path()))
                    .fold(Vec::new(), |mut acc, z| {
                        if !acc.contains(&z) {
                            acc.push(z);
                        }
                        acc
                    });
                if waiting.is_empty() {
                    Outcome::Open(detail)
                } else {
                    Outcome::Open(format!("the md5 check waits for {}", waiting.join(", ")))
                }
            }
        };
        match by_index.iter_mut().find(|(i, _)| *i == rom.index) {
            Some(slot) if slot.1 != Outcome::Match && outcome < slot.1 => slot.1 = outcome,
            Some(_) => {}
            None => by_index.push((rom.index, outcome)),
        }
    }
    match by_index.into_iter().map(|(_, o)| o).max() {
        None | Some(Outcome::Match) => Verdict::Match(matched),
        Some(Outcome::Open(why)) => Verdict::Open(why),
        Some(Outcome::Mismatch(why)) => Verdict::Mismatch(why),
        Some(Outcome::Refused(why)) => Verdict::Refused(why),
    }
}

/// The set name a MAME DAT gives the zip `file`.
fn set_name(file: &str) -> &str {
    let split = file
        .len()
        .checked_sub(4)
        .and_then(|i| Some((file.get(..i)?, file.get(i..)?)));
    match split {
        Some((stem, ext)) if ext.eq_ignore_ascii_case(".zip") => stem,
        _ => file,
    }
}

fn joined(names: &[String]) -> String {
    names.join(", ")
}

/// What becomes of a staged MRA zip once examined.
enum Action {
    /// The download fails with this reason and the zip stays in staging.
    Fail(String),
    /// The zip is quarantined.
    Quarantine(Why),
    /// The zip is placed as these pieces, with this log note; the md5 check read these members.
    Place(Vec<Piece>, Value, Vec<(PathBuf, String)>),
}

/// Builds the piece of a member with its state and, for the DAT path, its rom.
type MakePiece<'a> = dyn Fn(&Hashed, Option<FileState>, Option<&EntryRom>) -> Piece + 'a;

/// Turns a verdict into an action: the md5 path first, then `dat`, then no hash source.
/// `piece` builds a piece of a member with its state and, for the DAT path, its rom.
fn decide(
    verdict: Verdict,
    dat: Option<&TitleEntry>,
    members: &[Hashed],
    piece: &MakePiece<'_>,
    staged: &Path,
) -> Action {
    let all = |state: FileState| {
        members
            .iter()
            .map(|m| piece(m, Some(state), None))
            .collect::<Vec<_>>()
    };
    match verdict {
        Verdict::Refused(why) => {
            Action::Fail(format!("cannot check this zip against the MRA: {why}"))
        }
        Verdict::Missing(missing) => {
            let list = joined(&missing);
            Action::Quarantine(Why {
                reason: format!("the zip lacks members the MRA names ({list}); it was quarantined"),
                report: format!(
                    "This zip lacks members the MRA names and was not placed.\nMissing: {list}"
                ),
                detail: json!({ "missing": missing }),
            })
        }
        Verdict::Mismatch(detail) => Action::Quarantine(Why {
            reason: format!("the zip does not match the MRA's md5 ({detail}); it was quarantined"),
            report: format!("This zip does not match the MRA's md5 and was not placed.\n{detail}"),
            detail: json!({ "md5": detail }),
        }),
        Verdict::Match(read) => {
            let pieces = members
                .iter()
                .map(|m| {
                    let used = read
                        .iter()
                        .any(|(p, n)| p == staged && m.member.as_deref() == Some(n.as_str()));
                    let state = if used {
                        FileState::Verified
                    } else {
                        FileState::Unverified
                    };
                    piece(m, Some(state), None)
                })
                .collect();
            Action::Place(pieces, json!({ "verification": "mra_md5" }), read)
        }
        Verdict::Open(why) => Action::Place(
            all(FileState::Unverified),
            json!({ "verification": "mra_md5", "reason": why }),
            Vec::new(),
        ),
        Verdict::NoMd5 => match dat {
            Some(entry) if entry.is_bios() => Action::Fail(BIOS_REFUSED.to_owned()),
            Some(entry) => {
                let set = match_members(&entry.roms, members);
                if !set.is_exact() {
                    return Action::Quarantine(Why {
                        reason: format!(
                            "the zip does not match the DAT entry {}; it was quarantined",
                            entry.name
                        ),
                        report: format!(
                            "This zip does not match the DAT entry {} and was not placed.\n\
                             Members not in the entry: {}\nRoms not in the zip: {}",
                            entry.name,
                            joined(&set.extra),
                            joined(&set.absent)
                        ),
                        detail: json!({ "dat_entry": entry.name, "extra": set.extra, "absent": set.absent }),
                    });
                }
                let pieces = set
                    .pairs
                    .iter()
                    .map(|(m, r)| piece(m, None, Some(r)))
                    .collect();
                let note = json!({ "verification": "dat", "dat_entry": entry.name });
                Action::Place(pieces, note, Vec::new())
            }
            None => Action::Place(
                all(FileState::Unverified),
                json!({ "verification": "none", "reason": "no hash source" }),
                Vec::new(),
            ),
        },
    }
}

/// The zip rom `rom_id` names and the live DAT entry of the same set name for its
/// directory, if any: an HBMAME DAT for a `hbmame` zip, any other DAT otherwise.
fn lookup(
    c: &rusqlite::Connection,
    rom_id: i64,
) -> Result<(Option<arcade_rows::ZipRom>, Option<TitleEntry>)> {
    let Some(z) = arcade_rows::zip_rom(c, rom_id)? else {
        return Ok((None, None));
    };
    let hbmame = z.zip_dir.eq_ignore_ascii_case("hbmame");
    let set = set_name(&z.name);
    let dat = match arcade_rows::dat_entry_named(c, arcade::PLATFORM, set, hbmame)? {
        Some(t) => imports::title_entry(c, t)?,
        None => None,
    };
    Ok((Some(z), dat))
}

impl Placing<'_> {
    /// An MRA download: one zip the MRA names, verified by the MRA's md5 when it covers
    /// the zip, else by a loaded MAME DAT entry of the same name, else placed unverified.
    pub(super) async fn mra(&self, row: &DownloadRow) -> Result<()> {
        let app = self.app();
        let ids = [row.id];
        let rom_id = row.rom_id;
        let (zip_rom, dat) = app.db.read(move |c| lookup(c, rom_id)).await?;
        let (Some(zip_rom), Some(rom)) = (
            zip_rom,
            self.entry.roms.iter().find(|r| r.id == rom_id).cloned(),
        ) else {
            return fail(app, &ids, "the wanted zip is not in the catalog").await;
        };
        let zip = ZipPath {
            dir: zip_rom.zip_dir.clone(),
            file: zip_rom.name.clone(),
        };
        let local = match self.local(row).await? {
            Ok(p) => p,
            Err(reason) => return fail(app, &ids, reason).await,
        };
        let staged = StagedFile {
            path: local.clone(),
            size: fs::metadata(&local).map_or(0, |m| m.len()),
            kind: StagedKind::Zip,
            head: Vec::new(),
            members: Vec::new(),
        };
        let plan = match zip_placement(&zip, &staged) {
            Ok(p) => p,
            Err(e) => return fail(app, &ids, &format!("cannot place the file: {e}")).await,
        };
        if !local.exists() && self.games.join(&plan.final_rel_path).is_file() {
            finish(app, &ids, DownloadState::Done, None).await?;
            return self.refresh(&zip).await;
        }
        if !is_zip(&local) {
            return fail(app, &ids, "an MRA zip is imported from a zip").await;
        }
        self.ctx.checkpoint().await?;
        let members = match self.hash(&local).await? {
            Ok(h) => h,
            Err(reason) => return fail(app, &ids, &reason).await,
        };
        let mra_file = app
            .config()
            .paths
            .root
            .join(arcade::ARCADE_DIR)
            .join(&zip_rom.mra_path);
        let (games, path, at) = (self.games.clone(), local.clone(), zip.clone());
        let verdict = tokio::task::spawn_blocking(move || {
            mra::read(&mra_file).map(|m| examine(&m, &at, &path, &games))
        })
        .await
        .map_err(|e| task(&e))?;
        let verdict = match verdict {
            Ok(v) => v,
            Err(e) => {
                let reason = format!("cannot read the MRA of this entry: {e}");
                return fail(app, &ids, &reason).await;
            }
        };
        let piece = |m: &Hashed, state: Option<FileState>, dat_rom: Option<&EntryRom>| Piece {
            download: row.id,
            source: local.clone(),
            hashed: m.clone(),
            rom: dat_rom.unwrap_or(&rom).clone(),
            state,
        };
        match decide(verdict, dat.as_ref(), &members, &piece, &local) {
            Action::Fail(reason) => fail(app, &ids, &reason).await,
            Action::Quarantine(why) => {
                self.quarantine_with(row.id, rom_id, &local, &members, Some(why))
                    .await
            }
            Action::Place(pieces, note, read) => {
                let originals = std::slice::from_ref(&local);
                let placed = self
                    .place_plan(&plan, &staged, pieces, &ids, originals, Some(note))
                    .await?;
                if placed {
                    self.verify_siblings(&read, &local).await?;
                    self.refresh(&zip).await?;
                }
                Ok(())
            }
        }
    }

    /// Marks the members an md5 match read from zips already in `games/` verified, as
    /// the zips of this title they belong to.
    async fn verify_siblings(&self, read: &[(PathBuf, String)], staged: &Path) -> Result<()> {
        let mut rows: Vec<(String, i64)> = Vec::new();
        for (path, member) in read.iter().filter(|(p, _)| p != staged) {
            let (Ok(rel), Some(name)) = (path.strip_prefix(&self.games), path.file_name()) else {
                continue;
            };
            let name = name.to_string_lossy();
            let Some(rom) = self
                .entry
                .roms
                .iter()
                .find(|r| r.name.eq_ignore_ascii_case(&name))
            else {
                continue;
            };
            rows.push((format!("{}#{member}", rel_string(rel)), rom.id));
        }
        if rows.is_empty() {
            return Ok(());
        }
        let pid = self.pid();
        self.app()
            .db
            .write(move |c| {
                let tx = c.transaction()?;
                for (rel, rom) in &rows {
                    files::mark_verified(&tx, &pid, rel, *rom)?;
                }
                crate::db::commit(tx)
            })
            .await
    }

    /// Redoes zip presence and the md5 check of every MRA title naming `zip`.
    async fn refresh(&self, zip: &ZipPath) -> Result<()> {
        let app = self.app();
        let (dir, file) = (zip.dir.clone(), zip.file.clone());
        let titles = app
            .db
            .read(move |c| arcade_rows::titles_naming(c, arcade::PLATFORM, &dir, &file))
            .await?;
        let arcade_dir = app.config().paths.root.join(arcade::ARCADE_DIR);
        let games = self.games.clone();
        let found =
            tokio::task::spawn_blocking(move || arcade::refresh(&arcade_dir, &games, &titles))
                .await
                .map_err(|e| task(&e))?;
        app.db
            .write(move |c| arcade::store_refreshed(c, &found))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    use mistarr_core::hash::Md5Stream;

    fn write_zip(path: &Path, members: &[(&str, &[u8])]) {
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        let mut z = zip::ZipWriter::new(fs::File::create(path).expect("create"));
        for (name, body) in members {
            z.start_file(*name, zip::write::SimpleFileOptions::default())
                .expect("start");
            z.write_all(body).expect("write");
        }
        z.finish().expect("finish");
    }

    fn md5_of(parts: &[&[u8]]) -> String {
        let mut m = Md5Stream::new();
        for p in parts {
            m.update(p);
        }
        m.finish()
    }

    fn at(name: &str) -> ZipPath {
        zip_location(name).expect("location")
    }

    #[test]
    fn verdicts_follow_md5_members_and_siblings() {
        let dir = tempfile::tempdir().expect("tempdir");
        let games = dir.path().join("games");
        let staged = dir.path().join("staged.zip");
        write_zip(&staged, &[("a.bin", b"AA"), ("extra.bin", b"E")]);
        let md5 = md5_of(&[b"AA", b"BB"]);
        let two = mra::parse(
            format!(
                r#"<m><rom index="0" zip="exblast.zip|exparent.zip" md5="{md5}">
                   <part name="a.bin"/><part name="b.bin" zip="exparent.zip"/></rom></m>"#
            )
            .as_bytes(),
        )
        .expect("mra");
        let pending = examine(&two, &at("exblast.zip"), &staged, &games);
        assert_eq!(
            pending,
            Verdict::Open("the md5 check waits for mame/exparent.zip".into())
        );
        write_zip(&games.join("mame/exparent.zip"), &[("b.bin", b"BB")]);
        let Verdict::Match(read) = examine(&two, &at("exblast.zip"), &staged, &games) else {
            panic!("no match");
        };
        assert!(read.contains(&(staged.clone(), "a.bin".into())));
        assert!(read
            .iter()
            .any(|(p, n)| p.ends_with("mame/exparent.zip") && n == "b.bin"));
        write_zip(&games.join("mame/exparent.zip"), &[("b.bin", b"BX")]);
        assert!(matches!(
            examine(&two, &at("exblast.zip"), &staged, &games),
            Verdict::Mismatch(_)
        ));

        let lacking = mra::parse(br#"<m><rom index="0" zip="exblast.zip"><part name="gone.bin"/><part name="a.bin"/></rom></m>"#)
            .expect("mra");
        assert_eq!(
            examine(&lacking, &at("exblast.zip"), &staged, &games),
            Verdict::Missing(vec!["gone.bin".into()])
        );
        let plain =
            mra::parse(br#"<m><rom index="0" zip="exblast.zip"><part name="a.bin"/></rom></m>"#)
                .expect("mra");
        assert_eq!(
            examine(&plain, &at("exblast.zip"), &staged, &games),
            Verdict::NoMd5
        );
        let odd = mra::parse(
            format!(r#"<m><rom index="0" zip="exblast.zip" md5="{md5}"><group/></rom></m>"#)
                .as_bytes(),
        )
        .expect("mra");
        assert!(matches!(
            examine(&odd, &at("exblast.zip"), &staged, &games),
            Verdict::Refused(r) if r.contains("not supported")
        ));
    }

    #[test]
    fn only_members_read_by_a_matching_alternative_count_as_read() {
        let dir = tempfile::tempdir().expect("tempdir");
        let staged = dir.path().join("staged.zip");
        write_zip(&staged, &[("a.bin", b"AA"), ("x.bin", b"XX")]);
        let (wrong, right) = (md5_of(&[b"nothing"]), md5_of(&[b"AA"]));
        let alternatives = mra::parse(
            format!(
                r#"<m><rom index="0" zip="exblast.zip" md5="{wrong}"><part name="x.bin"/></rom>
                   <rom index="0" zip="exblast.zip" md5="{right}"><part name="a.bin"/></rom></m>"#
            )
            .as_bytes(),
        )
        .expect("mra");
        let games = dir.path().join("games");
        let verdict = examine(&alternatives, &at("exblast.zip"), &staged, &games);
        assert_eq!(verdict, Verdict::Match(vec![(staged, "a.bin".into())]));
    }

    #[test]
    fn set_names_drop_the_zip_extension() {
        assert_eq!(set_name("exblast.zip"), "exblast");
        assert_eq!(set_name("ExBlast.ZIP"), "ExBlast");
        assert_eq!(set_name("exblast"), "exblast");
    }
}
