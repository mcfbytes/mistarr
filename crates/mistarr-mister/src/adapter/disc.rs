//! Disc adapter: every track of an entry moves into `games/<Core>/<Title>/`.

use std::path::{Path, PathBuf};

use super::{extension, row_methods, safe_name, source_for, CoreAdapter, PlacementPlan, Step};
use crate::input::{DatEntry, StagedFile};
use crate::platforms::Platform;
use crate::{Error, Result};

/// A cue with its tracks, or a single image. Never zipped; CHD accepted on scan.
pub(super) struct Disc(pub &'static Platform);

/// The title shared by every disc of a game: the entry name without its `(Disc N)` tag.
fn title(entry_name: &str) -> String {
    let lower = entry_name.to_ascii_lowercase();
    let Some(start) = lower.find("(disc ") else {
        return entry_name.to_owned();
    };
    let end = lower[start..]
        .find(')')
        .map_or(entry_name.len(), |i| start + i + 1);
    let joined = format!(
        "{} {}",
        entry_name[..start].trim_end(),
        entry_name[end..].trim_start()
    );
    joined.trim().to_owned()
}

impl CoreAdapter for Disc {
    row_methods!();

    fn plan_placement(&self, entry: &DatEntry, staged: &StagedFile) -> Result<PlacementPlan> {
        let dir = PathBuf::from(self.0.core_dir).join(safe_name(&title(&entry.name))?);
        let mut steps = vec![Step::CreateDir { path: dir.clone() }];
        let mut used = Vec::new();
        for rom in &entry.roms {
            let target = safe_name(&rom.name)?;
            if target != rom.name {
                return Err(Error::InvalidName(rom.name.clone()));
            }
            let src = source_for(rom, staged, &mut used)?;
            steps.extend(src.unzip);
            steps.push(Step::Rename {
                from: src.work,
                to: dir.join(&target),
            });
        }
        let with_ext = |want: &str| {
            entry
                .roms
                .iter()
                .find(|r| extension(Path::new(&r.name)).as_deref() == Some(want))
        };
        let primary = with_ext("cue")
            .or_else(|| with_ext("iso"))
            .or(entry.roms.first())
            .ok_or(Error::Unplaceable("DAT entry lists no roms"))?;
        let final_rel_path = dir.join(&primary.name);
        Ok(PlacementPlan {
            final_rel_path,
            steps,
        })
    }

    fn accepts(&self, path: &Path) -> bool {
        extension(path).is_some_and(|e| self.0.load_extensions.contains(&e.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;
    use super::*;
    use crate::input::StagedKind;
    use crate::platforms::{Kind, PLATFORMS};

    const CUE: &str = "Example Quest (USA).cue";
    const T1: &str = "Example Quest (USA) (Track 1).bin";
    const T2: &str = "Example Quest (USA) (Track 2).bin";
    const T3: &str = "Example Quest (USA) (Track 3).bin";

    fn three_tracks() -> DatEntry {
        entry(
            "Example Quest (USA)",
            &[(CUE, 300), (T1, 1000), (T2, 2000), (T3, 3000)],
        )
    }

    #[test]
    fn every_disc_row_places_three_tracks_and_a_cue() {
        for p in PLATFORMS.iter().filter(|p| p.kind == Kind::Disc) {
            let staged = container(
                StagedKind::Dir,
                "Example Quest (USA)",
                &[
                    (T3, 3000, b""),
                    (CUE, 300, b""),
                    (T1, 1000, b""),
                    (T2, 2000, b""),
                ],
            );
            let plan = adapter(p.id)
                .plan_placement(&three_tracks(), &staged)
                .expect("plan");
            let dir = PathBuf::from(p.core_dir).join("Example Quest (USA)");
            assert_eq!(plan.final_rel_path, dir.join(CUE), "{}", p.id);
            let src = PathBuf::from("Example Quest (USA)");
            assert_eq!(
                plan.steps,
                [
                    Step::CreateDir { path: dir.clone() },
                    Step::Rename {
                        from: src.join(CUE),
                        to: dir.join(CUE)
                    },
                    Step::Rename {
                        from: src.join(T1),
                        to: dir.join(T1)
                    },
                    Step::Rename {
                        from: src.join(T2),
                        to: dir.join(T2)
                    },
                    Step::Rename {
                        from: src.join(T3),
                        to: dir.join(T3)
                    },
                ],
                "{}",
                p.id
            );
            assert!(!plan.steps.iter().any(|s| matches!(s, Step::Zip { .. })));
        }
    }

    #[test]
    fn every_disc_row_accepts_cue_iso_and_chd_but_not_zip() {
        for p in PLATFORMS.iter().filter(|p| p.kind == Kind::Disc) {
            let a = adapter(p.id);
            for ext in ["cue", "iso", "chd", "CHD"] {
                assert!(
                    a.accepts(Path::new(&format!("Example Quest (USA).{ext}"))),
                    "{}",
                    p.id
                );
            }
            assert!(!a.accepts(Path::new("Example Quest (USA).zip")), "{}", p.id);
            assert!(!a.accepts(Path::new(T1)), "{}", p.id);
        }
    }

    #[test]
    fn renamed_tracks_take_dat_names_so_the_verified_cue_stays_valid() {
        let staged = container(
            StagedKind::Dir,
            "dl",
            &[
                ("a.cue", 300, b""),
                ("1.bin", 1000, b""),
                ("2.bin", 2000, b""),
                ("3.bin", 3000, b""),
            ],
        );
        let plan = adapter("psx")
            .plan_placement(&three_tracks(), &staged)
            .expect("plan");
        assert_eq!(
            plan.steps[2],
            Step::Rename {
                from: "dl/1.bin".into(),
                to: PathBuf::from("PSX/Example Quest (USA)").join(T1)
            }
        );
    }

    #[test]
    fn zipped_disc_is_unzipped_into_the_title_directory() {
        let staged = container(
            StagedKind::Zip,
            "dl.zip",
            &[
                (CUE, 300, b""),
                (T1, 1000, b""),
                (T2, 2000, b""),
                (T3, 3000, b""),
            ],
        );
        let plan = adapter("saturn")
            .plan_placement(&three_tracks(), &staged)
            .expect("plan");
        assert_eq!(
            plan.steps
                .iter()
                .filter(|s| matches!(s, Step::Unzip { .. }))
                .count(),
            4
        );
        assert_eq!(plan.steps.len(), 9);
    }

    #[test]
    fn missing_track_is_refused() {
        let staged = container(
            StagedKind::Dir,
            "dl",
            &[(CUE, 300, b""), (T1, 1000, b""), (T2, 2000, b"")],
        );
        let err = adapter("psx").plan_placement(&three_tracks(), &staged);
        assert!(matches!(err, Err(Error::MissingRom(name)) if name == T3));
    }

    #[test]
    fn single_iso_file() {
        let e = entry(
            "Example Quest (Europe)",
            &[("Example Quest (Europe).iso", 5000)],
        );
        let plan = adapter("megacd")
            .plan_placement(&e, &file("x.iso", 5000, &[]))
            .expect("plan");
        assert_eq!(
            plan.final_rel_path,
            Path::new("MegaCD/Example Quest (Europe)/Example Quest (Europe).iso")
        );
    }

    #[test]
    fn multi_disc_games_share_a_directory() {
        let d2 = entry(
            "Example Quest (USA) (Disc 2)",
            &[("Example Quest (USA) (Disc 2).cue", 10)],
        );
        let plan = adapter("pcecd")
            .plan_placement(&d2, &file("d2.cue", 10, &[]))
            .expect("plan");
        assert_eq!(
            plan.steps[0],
            Step::CreateDir {
                path: "TGFX16-CD/Example Quest (USA)".into()
            }
        );
        assert_eq!(title("Example Quest (Disc 1) (USA)"), "Example Quest (USA)");
        assert_eq!(title("Example Quest"), "Example Quest");
    }

    #[test]
    fn track_names_with_separators_are_refused() {
        let e = entry("Example Quest (USA)", &[("../x.cue", 10)]);
        let err = adapter("psx").plan_placement(&e, &file("x.cue", 10, &[]));
        assert!(matches!(err, Err(Error::InvalidName(_))));
    }
}
