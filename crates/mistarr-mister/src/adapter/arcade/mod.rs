//! Arcade adapter: MAME zips placed whole under `games/mame`, wanted via MRA files.

pub mod assemble;
pub mod mra;

use std::path::{Path, PathBuf};

use self::mra::ZipPath;
use super::{extension, row_methods, safe_name, staged_name, CoreAdapter, PlacementPlan, Step};
use crate::input::{DatEntry, StagedFile, StagedKind};
use crate::platforms::Platform;
use crate::{Error, Result};

/// No romset rebuilding, merging or splitting: a zip is placed as it arrived.
pub(super) struct Arcade(pub &'static Platform);

impl CoreAdapter for Arcade {
    row_methods!();

    fn plan_placement(&self, entry: &DatEntry, staged: &StagedFile) -> Result<PlacementPlan> {
        let zip_name = format!("{}.zip", safe_name(&entry.name)?);
        let final_rel_path = PathBuf::from(self.0.core_dir).join(&zip_name);
        let from = staged_name(staged)?;
        let steps = match staged.kind {
            StagedKind::Zip => vec![Step::Rename {
                from,
                to: final_rel_path.clone(),
            }],
            StagedKind::Dir => vec![
                Step::Zip {
                    from,
                    to: PathBuf::from(&zip_name),
                },
                Step::Rename {
                    from: PathBuf::from(zip_name),
                    to: final_rel_path.clone(),
                },
            ],
            StagedKind::File => return Err(Error::Unplaceable("arcade sets are placed as zips")),
        };
        Ok(PlacementPlan {
            final_rel_path,
            steps,
        })
    }

    fn accepts(&self, path: &Path) -> bool {
        extension(path).as_deref() == Some("zip")
    }
}

/// Directories under `games/` an MRA zip is placed into; the library scan walks both.
pub const ZIP_DIRS: [&str; 2] = ["mame", "hbmame"];

/// Plans placing a staged zip whole where an MRA reads it from, `games/mame/` or
/// `games/hbmame/` under the MRA's file name. The zip is never unpacked or rebuilt.
///
/// # Errors
///
/// [`Error::Unplaceable`] for a staged item that is not a zip or a zip read from another
/// directory, [`Error::InvalidName`] for a file name that is not safe to write.
///
/// ```
/// use mistarr_mister::adapter::arcade::{mra::zip_location, zip_placement};
/// use mistarr_mister::{StagedFile, StagedKind};
/// let staged = StagedFile { path: "/s/x.zip".into(), size: 1, kind: StagedKind::Zip,
///     head: vec![], members: vec![] };
/// let plan = zip_placement(&zip_location("/HBMAME/exhb.zip").unwrap(), &staged).unwrap();
/// assert_eq!(plan.final_rel_path, std::path::Path::new("hbmame/exhb.zip"));
/// ```
pub fn zip_placement(zip: &ZipPath, staged: &StagedFile) -> Result<PlacementPlan> {
    if staged.kind != StagedKind::Zip {
        return Err(Error::Unplaceable("an MRA zip is placed as a zip"));
    }
    let Some(dir) = ZIP_DIRS.iter().find(|d| d.eq_ignore_ascii_case(&zip.dir)) else {
        return Err(Error::Unplaceable(
            "an MRA zip is placed only into games/mame or games/hbmame",
        ));
    };
    if safe_name(&zip.file)? != zip.file {
        return Err(Error::InvalidName(zip.file.clone()));
    }
    let final_rel_path = PathBuf::from(dir).join(&zip.file);
    Ok(PlacementPlan {
        steps: vec![Step::Rename {
            from: staged_name(staged)?,
            to: final_rel_path.clone(),
        }],
        final_rel_path,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;
    use super::*;

    #[test]
    fn zip_is_placed_whole_under_mame() {
        let e = entry("exblast", &[("cpu.bin", 16)]);
        let z = container(StagedKind::Zip, "exblast.zip", &[("cpu.bin", 16, b"")]);
        let plan = adapter("arcade").plan_placement(&e, &z).expect("plan");
        assert_eq!(plan.final_rel_path, Path::new("mame/exblast.zip"));
        assert_eq!(
            plan.steps,
            [Step::Rename {
                from: "exblast.zip".into(),
                to: "mame/exblast.zip".into()
            }]
        );
        assert_eq!(
            adapter("arcade").games_dir(Path::new("/media/fat")),
            Path::new("/media/fat/games/mame")
        );
    }

    #[test]
    fn directory_is_zipped_then_placed() {
        let e = entry("exblast", &[("cpu.bin", 16)]);
        let d = container(StagedKind::Dir, "exblast", &[("cpu.bin", 16, b"")]);
        let plan = adapter("arcade").plan_placement(&e, &d).expect("plan");
        assert_eq!(
            plan.steps,
            [
                Step::Zip {
                    from: "exblast".into(),
                    to: "exblast.zip".into()
                },
                Step::Rename {
                    from: "exblast.zip".into(),
                    to: "mame/exblast.zip".into()
                },
            ]
        );
    }

    #[test]
    fn mra_zips_go_whole_to_mame_or_hbmame_under_their_own_name() {
        let z = container(StagedKind::Zip, "download.zip", &[("cpu.bin", 16, b"")]);
        let at = |name: &str| mra::zip_location(name).expect("location");
        let plan = zip_placement(&at("exblast.zip"), &z).expect("plan");
        assert_eq!(
            plan.steps,
            [Step::Rename {
                from: "download.zip".into(),
                to: "mame/exblast.zip".into()
            }]
        );
        let hb = zip_placement(&at("/hbmame/exhb.zip"), &z).expect("plan");
        assert_eq!(hb.final_rel_path, Path::new("hbmame/exhb.zip"));
        assert!(zip_placement(&at("/other/exhb.zip"), &z).is_err());
        assert!(zip_placement(&at("sub/exhb.zip"), &z).is_err());
        assert!(zip_placement(&at("ex:hb.zip"), &z).is_err());
        let d = container(StagedKind::Dir, "exblast", &[("cpu.bin", 16, b"")]);
        assert!(zip_placement(&at("exblast.zip"), &d).is_err());
    }

    #[test]
    fn loose_file_is_refused_and_only_zips_accepted() {
        let e = entry("exblast", &[("cpu.bin", 16)]);
        assert!(adapter("arcade")
            .plan_placement(&e, &file("cpu.bin", 16, &[]))
            .is_err());
        assert!(adapter("arcade").accepts(Path::new("exblast.zip")));
        assert!(!adapter("arcade").accepts(Path::new("exblast.7z")));
        assert_eq!(adapter("arcade").requires_bios(), None);
    }
}
