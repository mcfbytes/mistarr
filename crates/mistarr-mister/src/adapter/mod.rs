//! The `CoreAdapter` contract and the adapter for each platform row.
//! See `docs/PLATFORMS.md` "Adapter contract".

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use mistarr_core::hash::{ByteOrder, HeaderRule};
use mistarr_core::PlatformId;

use crate::input::{DatEntry, PlaceRom, StagedFile, StagedKind, StagedMember};
use crate::platforms::{Kind, Platform, PLATFORMS};
use crate::{Error, Result};

pub mod arcade;
mod cart;
mod disc;
pub mod neogeo;
mod xml_caps;

/// Placement rules for one MiSTer core. The platform, its directory, BIOS and the
/// extensions it loads are facts of the [`Platform`] row, not of the adapter.
///
/// ```
/// use mistarr_mister::{adapter_for, DatEntry, PlaceRom, PlatformId, StagedFile, StagedKind};
/// let snes = adapter_for(&PlatformId("snes".into())).unwrap();
/// let entry = DatEntry { name: "Example Quest (USA)".into(),
///     roms: vec![PlaceRom { name: "Example Quest (USA).sfc".into(), size: 1024, header: None }] };
/// let staged = StagedFile { path: "/s/x.sfc".into(), size: 1024, kind: StagedKind::File,
///     head: vec![], members: vec![] };
/// let plan = snes.plan_placement(&entry, &staged).unwrap();
/// assert_eq!(plan.final_rel_path, std::path::Path::new("SNES/Example Quest (USA).sfc"));
/// ```
pub trait CoreAdapter: Send + Sync {
    /// Decides the final path and the transformations that get the staged item there.
    ///
    /// # Errors
    ///
    /// Returns an error when the staged item cannot be made loadable with the
    /// permitted steps, for example a headerless NES file with no DAT header.
    fn plan_placement(&self, entry: &DatEntry, staged: &StagedFile) -> Result<PlacementPlan>;
}

/// Where a staged item ends up and how it gets there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacementPlan {
    /// Loadable file or directory, relative to the games root (`<root>/games`).
    pub final_rel_path: PathBuf,
    /// Steps to apply in order.
    pub steps: Vec<Step>,
}

impl PlacementPlan {
    /// A plan that moves staging item `from` to library path `to` unchanged.
    ///
    /// ```
    /// use mistarr_mister::{PlacementPlan, Step};
    /// let plan = PlacementPlan::rename("dl.zip".into(), "mame/exblast.zip".into());
    /// assert_eq!(plan.final_rel_path, std::path::Path::new("mame/exblast.zip"));
    /// assert!(matches!(&plan.steps[..], [Step::Rename { .. }]));
    /// ```
    #[must_use]
    pub fn rename(from: PathBuf, to: PathBuf) -> Self {
        Self {
            steps: vec![Step::Rename {
                from,
                to: to.clone(),
            }],
            final_rel_path: to,
        }
    }
}

/// One permitted transformation.
///
/// Staging paths are relative to the directory holding the staged item.
/// Library paths are relative to the games root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Extract `member` of the staged zip to staging path `to`.
    Unzip {
        /// Member name inside the archive.
        member: String,
        /// Staging path of the extracted file.
        to: PathBuf,
    },
    /// Pack staging directory `from` into staging zip `to`, members relative to `from`.
    Zip {
        /// Staging directory to pack.
        from: PathBuf,
        /// Staging path of the new archive.
        to: PathBuf,
    },
    /// Prepend `bytes` to staging file `file`.
    AddHeader {
        /// Staging file to rewrite.
        file: PathBuf,
        /// Header to prepend.
        bytes: Vec<u8>,
    },
    /// Remove the first `len` bytes of staging file `file`.
    StripHeader {
        /// Staging file to rewrite.
        file: PathBuf,
        /// Number of leading bytes to drop.
        len: u64,
    },
    /// Rewrite staging file `file` from byte order `from` to big-endian.
    SwapByteOrder {
        /// Staging file to rewrite.
        file: PathBuf,
        /// Byte order the file is in now.
        from: ByteOrder,
    },
    /// Create library directory `path` and its parents.
    CreateDir {
        /// Library path of the directory.
        path: PathBuf,
    },
    /// Move staging file or directory `from` to library path `to`.
    Rename {
        /// Staging path.
        from: PathBuf,
        /// Library path.
        to: PathBuf,
    },
}

/// The adapter for a platform id, or `None` when the id is not in the table.
///
/// ```
/// use mistarr_mister::{adapter_for, PlatformId};
/// assert!(adapter_for(&PlatformId("n64".into())).is_some());
/// assert!(adapter_for(&PlatformId("unknown".into())).is_none());
/// ```
#[must_use]
pub fn adapter_for(id: &PlatformId) -> Option<&'static dyn CoreAdapter> {
    static CELL: OnceLock<Vec<Box<dyn CoreAdapter>>> = OnceLock::new();
    let all = CELL.get_or_init(|| PLATFORMS.iter().map(build).collect());
    let index = PLATFORMS.iter().position(|p| p.id == id.0)?;
    all.get(index).map(AsRef::as_ref)
}

fn build(row: &'static Platform) -> Box<dyn CoreAdapter> {
    match (row.kind, row.header_rule) {
        (Kind::Cartridge, HeaderRule::Ines) => Box::new(cart::Nes(row)),
        (Kind::Cartridge, HeaderRule::Smc) => Box::new(cart::Snes(row)),
        (Kind::Cartridge, HeaderRule::N64) => Box::new(cart::N64(row)),
        (Kind::Cartridge, _) => Box::new(cart::Cart(row)),
        (Kind::Disc, _) => Box::new(disc::Disc(row)),
        (Kind::Romset, _) => Box::new(neogeo::NeoGeo(row)),
        (Kind::Arcade, _) => Box::new(arcade::Arcade(row)),
    }
}

/// A DAT name made safe as one exFAT path component, as placement names files.
///
/// # Errors
///
/// [`Error::InvalidName`] for a name with nothing left after cleaning.
///
/// ```
/// assert_eq!(mistarr_mister::adapter::safe_name("A/B").unwrap(), "A_B");
/// ```
pub fn safe_name(name: &str) -> Result<String> {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let cleaned = cleaned.trim().trim_end_matches('.').to_owned();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        return Err(Error::InvalidName(name.to_owned()));
    }
    Ok(cleaned)
}

/// `name` itself when it is already safe as one path component.
///
/// # Errors
///
/// [`Error::InvalidName`] when [`safe_name`] would change it.
fn exact_name(name: &str) -> Result<&str> {
    if safe_name(name)? == name {
        Ok(name)
    } else {
        Err(Error::InvalidName(name.to_owned()))
    }
}

/// Whether `path` ends in `.ext`, compared without regard to ASCII case.
pub(crate) fn has_extension(path: &Path, ext: &str) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

/// Final path component of a staged item, as a staging path.
fn staged_name(staged: &StagedFile) -> Result<PathBuf> {
    staged
        .path
        .file_name()
        .map(PathBuf::from)
        .ok_or(Error::Unplaceable("staged path has no file name"))
}

/// Last component of a member name inside an archive or directory.
fn basename(member: &str) -> &str {
    member.rsplit(['/', '\\']).next().unwrap_or(member)
}

/// A staged file or member picked to become one DAT rom.
struct Source {
    /// Staging path of the payload once any unzip step has run.
    work: PathBuf,
    /// Uncompressed size.
    size: u64,
    /// First bytes of the payload.
    head: Vec<u8>,
    /// Unzip step, when the payload is inside the staged archive.
    unzip: Option<Step>,
}

/// Picks the member of `staged` that holds `rom`: by name, then by unique size.
fn pick<'a>(
    rom: &PlaceRom,
    members: &'a [StagedMember],
    used: &[usize],
) -> Option<(usize, &'a StagedMember)> {
    let free = || {
        members
            .iter()
            .enumerate()
            .filter(|(i, _)| !used.contains(i))
    };
    free()
        .find(|(_, m)| basename(&m.name).eq_ignore_ascii_case(basename(&rom.name)))
        .or_else(|| {
            let mut sized = free().filter(|(_, m)| m.size == rom.size);
            match (sized.next(), sized.next()) {
                (Some(only), None) => Some(only),
                _ => None,
            }
        })
}

/// Resolves the payload for `rom`, marking the chosen member in `used`.
fn source_for(rom: &PlaceRom, staged: &StagedFile, used: &mut Vec<usize>) -> Result<Source> {
    let missing = || Error::MissingRom(rom.name.clone());
    match staged.kind {
        StagedKind::File => {
            if used.contains(&0) {
                return Err(missing());
            }
            used.push(0);
            Ok(Source {
                work: staged_name(staged)?,
                size: staged.size,
                head: staged.head.clone(),
                unzip: None,
            })
        }
        StagedKind::Zip | StagedKind::Dir => {
            let only = (staged.members.len() == 1 && used.is_empty()).then_some(0);
            let (i, m) = only
                .and_then(|i| staged.members.get(i).map(|m| (i, m)))
                .or_else(|| pick(rom, &staged.members, used))
                .ok_or_else(missing)?;
            used.push(i);
            let (work, unzip) = if staged.kind == StagedKind::Zip {
                let to = PathBuf::from(basename(&m.name));
                let step = Step::Unzip {
                    member: m.name.clone(),
                    to: to.clone(),
                };
                (to, Some(step))
            } else {
                (staged_name(staged)?.join(&m.name), None)
            };
            Ok(Source {
                work,
                size: m.size,
                head: m.head.clone(),
                unzip,
            })
        }
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;

    pub fn entry(name: &str, roms: &[(&str, u64)]) -> DatEntry {
        DatEntry {
            name: name.to_owned(),
            roms: roms
                .iter()
                .map(|(n, s)| PlaceRom {
                    name: (*n).to_owned(),
                    size: *s,
                    header: None,
                })
                .collect(),
        }
    }

    pub fn file(name: &str, size: u64, head: &[u8]) -> StagedFile {
        StagedFile {
            path: PathBuf::from("/stage/abc").join(name),
            size,
            kind: StagedKind::File,
            head: head.to_vec(),
            members: Vec::new(),
        }
    }

    pub fn container(kind: StagedKind, name: &str, members: &[(&str, u64, &[u8])]) -> StagedFile {
        StagedFile {
            path: PathBuf::from("/stage/abc").join(name),
            size: 0,
            kind,
            head: Vec::new(),
            members: members
                .iter()
                .map(|(n, s, h)| StagedMember {
                    name: (*n).to_owned(),
                    size: *s,
                    head: h.to_vec(),
                })
                .collect(),
        }
    }

    pub fn adapter(id: &str) -> &'static dyn CoreAdapter {
        adapter_for(&PlatformId(id.to_owned())).expect("id is in the table")
    }

    /// A fresh directory, removed with everything in it when dropped.
    pub fn scratch() -> tempfile::TempDir {
        tempfile::tempdir().expect("create scratch dir")
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::*;
    use super::*;

    #[test]
    fn every_row_has_an_adapter_and_unknown_ids_none() {
        for p in &PLATFORMS {
            assert!(adapter_for(&p.platform_id()).is_some(), "{}", p.id);
        }
        assert!(adapter_for(&PlatformId("unknown".into())).is_none());
    }

    #[test]
    fn rename_plans_one_move_to_the_final_path() {
        let plan = PlacementPlan::rename("a".into(), "NES/b.nes".into());
        assert_eq!(plan.final_rel_path, Path::new("NES/b.nes"));
        assert_eq!(
            plan.steps,
            [Step::Rename {
                from: "a".into(),
                to: "NES/b.nes".into()
            }]
        );
    }

    #[test]
    fn exact_name_refuses_what_safe_name_changes() {
        assert_eq!(exact_name("a.bin").ok(), Some("a.bin"));
        assert!(matches!(exact_name("a/b.bin"), Err(Error::InvalidName(n)) if n == "a/b.bin"));
        assert!(exact_name("a.").is_err());
    }

    #[test]
    fn extensions_compare_without_case() {
        assert!(has_extension(Path::new("NES/a.ZIP"), "zip"));
        assert!(has_extension(Path::new("a.cue"), "CUE"));
        assert!(!has_extension(Path::new("a.zip.bak"), "zip"));
        assert!(!has_extension(Path::new(".cue"), "cue"));
        assert!(!has_extension(Path::new("cue"), "cue"));
    }

    #[test]
    fn safe_name_replaces_separators_and_rejects_dots() {
        assert_eq!(safe_name("A/B: C?").ok().as_deref(), Some("A_B_ C_"));
        assert!(safe_name("..").is_err());
        assert!(safe_name("  ").is_err());
    }

    #[test]
    fn pick_prefers_name_then_unique_size() {
        let rom = PlaceRom {
            name: "b.bin".into(),
            size: 5,
            header: None,
        };
        let members = container(
            StagedKind::Dir,
            "d",
            &[("a.bin", 5, b""), ("B.BIN", 7, b"")],
        )
        .members;
        assert_eq!(pick(&rom, &members, &[]).map(|(i, _)| i), Some(1));
        let rom = PlaceRom {
            name: "z.bin".into(),
            size: 5,
            header: None,
        };
        assert_eq!(pick(&rom, &members, &[]).map(|(i, _)| i), Some(0));
        assert_eq!(pick(&rom, &members, &[0]).map(|(i, _)| i), None);
    }
}
