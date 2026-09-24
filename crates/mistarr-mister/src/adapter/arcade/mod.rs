//! Arcade adapter: MAME zips placed whole under `games/mame`, wanted via MRA files.

pub mod mra;

use std::path::{Path, PathBuf};

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
