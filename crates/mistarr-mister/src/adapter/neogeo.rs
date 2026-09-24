//! Neo Geo adapter: the DAT game is the unit, placed whole as a zip or directory.

use std::path::{Path, PathBuf};

use super::{
    basename, extension, row_methods, safe_name, staged_name, CoreAdapter, PlacementPlan, Step,
};
use crate::input::{DatEntry, StagedFile, StagedKind};
use crate::platforms::Platform;
use crate::{Error, Result};

/// Romset placement; members are verified against the DAT, not the zip's own hash.
pub(super) struct NeoGeo(pub &'static Platform);

impl CoreAdapter for NeoGeo {
    row_methods!();

    fn plan_placement(&self, entry: &DatEntry, staged: &StagedFile) -> Result<PlacementPlan> {
        let name = safe_name(&entry.name)?;
        let final_rel_path = match staged.kind {
            StagedKind::Zip => PathBuf::from(self.0.core_dir).join(format!("{name}.zip")),
            StagedKind::Dir => PathBuf::from(self.0.core_dir).join(name),
            StagedKind::File => {
                return Err(Error::Unplaceable(
                    "Neo Geo games are placed as a zip or directory",
                ))
            }
        };
        for rom in &entry.roms {
            let present = staged
                .members
                .iter()
                .any(|m| basename(&m.name).eq_ignore_ascii_case(basename(&rom.name)));
            if !present {
                return Err(Error::MissingRom(rom.name.clone()));
            }
        }
        Ok(PlacementPlan {
            steps: vec![Step::Rename {
                from: staged_name(staged)?,
                to: final_rel_path.clone(),
            }],
            final_rel_path,
        })
    }

    fn accepts(&self, path: &Path) -> bool {
        extension(path).as_deref() == Some("zip") || path.is_dir()
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;
    use super::*;

    fn game() -> DatEntry {
        entry("examplequest", &[("001-p1.p1", 1024), ("001-s1.s1", 512)])
    }

    #[test]
    fn zip_is_placed_whole() {
        let z = container(
            StagedKind::Zip,
            "dl.zip",
            &[("001-p1.p1", 1024, b""), ("001-s1.s1", 512, b"")],
        );
        let plan = adapter("neogeo").plan_placement(&game(), &z).expect("plan");
        assert_eq!(plan.final_rel_path, Path::new("NeoGeo/examplequest.zip"));
        assert_eq!(
            plan.steps,
            [Step::Rename {
                from: "dl.zip".into(),
                to: "NeoGeo/examplequest.zip".into()
            }]
        );
    }

    #[test]
    fn directory_is_placed_whole() {
        let d = container(
            StagedKind::Dir,
            "examplequest",
            &[("001-p1.p1", 1024, b""), ("001-s1.s1", 512, b"")],
        );
        let plan = adapter("neogeo").plan_placement(&game(), &d).expect("plan");
        assert_eq!(plan.final_rel_path, Path::new("NeoGeo/examplequest"));
    }

    #[test]
    fn incomplete_set_and_loose_file_are_refused() {
        let z = container(StagedKind::Zip, "dl.zip", &[("001-p1.p1", 1024, b"")]);
        assert!(matches!(
            adapter("neogeo").plan_placement(&game(), &z),
            Err(Error::MissingRom(n)) if n == "001-s1.s1"
        ));
        assert!(adapter("neogeo")
            .plan_placement(&game(), &file("x.p1", 1, &[]))
            .is_err());
    }

    #[test]
    fn accepts_zip_and_directory() {
        let a = adapter("neogeo");
        assert!(a.accepts(Path::new("examplequest.zip")));
        assert!(a.accepts(&scratch("neogeo-dir")));
        assert!(!a.accepts(Path::new("examplequest.p1")));
    }
}
