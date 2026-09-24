//! The `CoreAdapter` contract and the adapter for each platform row.
//! See `docs/PLATFORMS.md` "Adapter contract".

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use mistarr_core::PlatformId;

use crate::input::{DatEntry, DatRom, StagedFile, StagedKind, StagedMember};
use crate::platforms::{Kind, Platform, PLATFORMS};
use crate::{Error, Result};

pub mod arcade;
mod cart;
mod disc;
mod neogeo;

/// Placement rules for one MiSTer core.
///
/// ```
/// use std::path::Path;
/// use mistarr_mister::{adapter_for, PlatformId};
/// let snes = adapter_for(&PlatformId("snes".into())).unwrap();
/// assert_eq!(snes.games_dir(Path::new("/media/fat")), Path::new("/media/fat/games/SNES"));
/// assert_eq!(snes.requires_bios(), None);
/// assert_eq!(snes.platform().0, "snes");
/// ```
pub trait CoreAdapter: Send + Sync {
    /// The platform this adapter places files for.
    fn platform(&self) -> PlatformId;
    /// The directory the core reads from, e.g. `root/games/NES`.
    fn games_dir(&self, root: &Path) -> PathBuf;
    /// Decides the final path and the transformations that get the staged item there.
    ///
    /// # Errors
    ///
    /// Returns an error when the staged item cannot be made loadable with the
    /// permitted steps, for example a headerless NES file with no DAT header.
    fn plan_placement(&self, entry: &DatEntry, staged: &StagedFile) -> Result<PlacementPlan>;
    /// True if this file, as found on disk, is loadable by the core without change.
    ///
    /// ```
    /// use std::path::Path;
    /// use mistarr_mister::{adapter_for, PlatformId};
    /// let gba = adapter_for(&PlatformId("gba".into())).unwrap();
    /// assert!(gba.accepts(Path::new("Example Quest (USA).gba")));
    /// assert!(!gba.accepts(Path::new("Example Quest (USA).txt")));
    /// ```
    fn accepts(&self, path: &Path) -> bool;
    /// BIOS file the core documents, reported on the status screen and never handled.
    fn requires_bios(&self) -> Option<&'static str>;
}

/// Where a staged item ends up and how it gets there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacementPlan {
    /// Loadable file or directory, relative to the games root (`<root>/games`).
    pub final_rel_path: PathBuf,
    /// Steps to apply in order.
    pub steps: Vec<Step>,
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

/// Byte order of an N64 image, told apart by its first four bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteOrder {
    /// Native order, conventionally `.z64`.
    BigEndian,
    /// 16-bit words swapped, conventionally `.v64`.
    ByteSwapped,
    /// 32-bit words reversed, conventionally `.n64`.
    LittleEndian,
}

impl ByteOrder {
    /// Detects the byte order from the start of an image.
    ///
    /// ```
    /// use mistarr_mister::ByteOrder;
    /// assert_eq!(ByteOrder::detect(&[0x80, 0x37, 0x12, 0x40]), Some(ByteOrder::BigEndian));
    /// assert_eq!(ByteOrder::detect(b"NES\x1a"), None);
    /// ```
    #[must_use]
    pub fn detect(head: &[u8]) -> Option<Self> {
        match head.get(..4)? {
            [0x80, 0x37, 0x12, 0x40] => Some(Self::BigEndian),
            [0x37, 0x80, 0x40, 0x12] => Some(Self::ByteSwapped),
            [0x40, 0x12, 0x37, 0x80] => Some(Self::LittleEndian),
            _ => None,
        }
    }
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
    match (row.kind, row.id) {
        (_, "nes") => Box::new(cart::Nes(row)),
        (_, "snes") => Box::new(cart::Snes(row)),
        (_, "n64") => Box::new(cart::N64(row)),
        (Kind::Disc, _) => Box::new(disc::Disc(row)),
        (Kind::Romset, _) => Box::new(neogeo::NeoGeo(row)),
        (Kind::Arcade, _) => Box::new(arcade::Arcade(row)),
        _ => Box::new(cart::Cart(row)),
    }
}

/// Implements the trait methods that only read the platform row in field `.0`.
macro_rules! row_methods {
    () => {
        fn platform(&self) -> ::mistarr_core::PlatformId {
            self.0.platform_id()
        }
        fn games_dir(&self, root: &::std::path::Path) -> ::std::path::PathBuf {
            root.join("games").join(self.0.core_dir)
        }
        fn requires_bios(&self) -> Option<&'static str> {
            self.0.bios
        }
    };
}
use row_methods;

/// A DAT name made safe as one exFAT path component.
fn safe_name(name: &str) -> Result<String> {
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

/// Lowercased extension of a path, without the dot.
fn extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
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
    rom: &DatRom,
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
fn source_for(rom: &DatRom, staged: &StagedFile, used: &mut Vec<usize>) -> Result<Source> {
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
                .map(|(n, s)| DatRom {
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

    pub fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mistarr-mister-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::*;
    use super::*;

    #[test]
    fn every_row_has_an_adapter_for_its_own_id() {
        for p in &PLATFORMS {
            let a = adapter(p.id);
            assert_eq!(a.platform().0, p.id);
            assert_eq!(a.requires_bios(), p.bios);
            assert_eq!(
                a.games_dir(Path::new("/r")),
                Path::new("/r/games").join(p.core_dir)
            );
        }
    }

    #[test]
    fn byte_order_detection() {
        assert_eq!(
            ByteOrder::detect(&[0x37, 0x80, 0x40, 0x12, 0]),
            Some(ByteOrder::ByteSwapped)
        );
        assert_eq!(
            ByteOrder::detect(&[0x40, 0x12, 0x37, 0x80]),
            Some(ByteOrder::LittleEndian)
        );
        assert_eq!(ByteOrder::detect(&[0x80, 0x37]), None);
    }

    #[test]
    fn safe_name_replaces_separators_and_rejects_dots() {
        assert_eq!(safe_name("A/B: C?").ok().as_deref(), Some("A_B_ C_"));
        assert!(safe_name("..").is_err());
        assert!(safe_name("  ").is_err());
    }

    #[test]
    fn pick_prefers_name_then_unique_size() {
        let rom = DatRom {
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
        let rom = DatRom {
            name: "z.bin".into(),
            size: 5,
            header: None,
        };
        assert_eq!(pick(&rom, &members, &[]).map(|(i, _)| i), Some(0));
        assert_eq!(pick(&rom, &members, &[0]).map(|(i, _)| i), None);
    }
}
