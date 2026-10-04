//! Cartridge adapters: one file per game, unzipped and renamed to the DAT name.

use std::path::PathBuf;

use mistarr_core::hash::{ByteOrder, HeaderRule, INES_HEADER_LEN, INES_MAGIC};

use super::{safe_name, source_for, CoreAdapter, PlacementPlan, Source, Step};
use crate::input::{DatEntry, PlaceRom, StagedFile};
use crate::platforms::Platform;
use crate::{Error, Result};

/// Any cartridge row whose core needs only the right extension; also loads zips.
pub(super) struct Cart(pub &'static Platform);

/// NES: the core needs an iNES header on disk.
pub(super) struct Nes(pub &'static Platform);

/// SNES: copier headers are stripped on disk.
pub(super) struct Snes(pub &'static Platform);

/// N64: images are normalised to big-endian.
pub(super) struct N64(pub &'static Platform);

/// Builds the plan for a single-file game, with at most one transformation.
fn plan(
    row: &Platform,
    entry: &DatEntry,
    staged: &StagedFile,
    transform: impl FnOnce(&Source, &PlaceRom) -> Result<Option<Step>>,
) -> Result<PlacementPlan> {
    let rom = entry
        .roms
        .first()
        .ok_or(Error::Unplaceable("DAT entry lists no roms"))?;
    let ext = row
        .extension_written
        .ok_or(Error::Unplaceable("platform has no file extension"))?;
    let src = source_for(rom, staged, &mut Vec::new())?;
    let final_rel_path =
        PathBuf::from(row.core_dir).join(format!("{}.{ext}", safe_name(&entry.name)?));
    let mut steps: Vec<Step> = src.unzip.clone().into_iter().collect();
    steps.extend(transform(&src, rom)?);
    steps.push(Step::Rename {
        from: src.work,
        to: final_rel_path.clone(),
    });
    Ok(PlacementPlan {
        final_rel_path,
        steps,
    })
}

impl CoreAdapter for Cart {
    fn plan_placement(&self, entry: &DatEntry, staged: &StagedFile) -> Result<PlacementPlan> {
        plan(self.0, entry, staged, |_, _| Ok(None))
    }
}

impl CoreAdapter for Nes {
    fn plan_placement(&self, entry: &DatEntry, staged: &StagedFile) -> Result<PlacementPlan> {
        plan(self.0, entry, staged, |src, rom| {
            if src.head.starts_with(INES_MAGIC) {
                return Ok(None);
            }
            match &rom.header {
                Some(h) if h.len() == INES_HEADER_LEN && h.starts_with(INES_MAGIC) => {
                    Ok(Some(Step::AddHeader {
                        file: src.work.clone(),
                        bytes: h.clone(),
                    }))
                }
                _ => Err(Error::MissingHeader),
            }
        })
    }
}

impl CoreAdapter for Snes {
    fn plan_placement(&self, entry: &DatEntry, staged: &StagedFile) -> Result<PlacementPlan> {
        plan(self.0, entry, staged, |src, _| {
            Ok(
                HeaderRule::smc_applies(src.size).then(|| Step::StripHeader {
                    file: src.work.clone(),
                    len: HeaderRule::Smc.header_len(),
                }),
            )
        })
    }
}

impl CoreAdapter for N64 {
    fn plan_placement(&self, entry: &DatEntry, staged: &StagedFile) -> Result<PlacementPlan> {
        plan(self.0, entry, staged, |src, _| {
            match ByteOrder::detect(&src.head).ok_or(Error::UnknownByteOrder)? {
                ByteOrder::BigEndian => Ok(None),
                from => Ok(Some(Step::SwapByteOrder {
                    file: src.work.clone(),
                    from,
                })),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;
    use super::*;
    use crate::input::StagedKind;
    use crate::platforms::{Kind, PLATFORMS};
    use std::path::Path;

    fn ines() -> Vec<u8> {
        let mut h = INES_MAGIC.to_vec();
        h.resize(INES_HEADER_LEN, 0);
        h
    }

    /// A head that satisfies each row's content check.
    fn good_head(id: &str) -> Vec<u8> {
        match id {
            "nes" => ines(),
            "n64" => vec![0x80, 0x37, 0x12, 0x40],
            _ => vec![0; 16],
        }
    }

    #[test]
    fn every_cartridge_row_plans_plain_and_zipped_files() {
        for p in PLATFORMS.iter().filter(|p| p.kind == Kind::Cartridge) {
            let ext = p.extension_written.expect("cartridge ext");
            let a = adapter(p.id);
            let name = "Example Quest (USA)";
            let want = PathBuf::from(p.core_dir).join(format!("{name}.{ext}"));
            let e = entry(name, &[(&format!("{name}.{ext}"), 4096)]);

            let plain = a
                .plan_placement(&e, &file(&format!("dl.{ext}"), 4096, &good_head(p.id)))
                .expect("plain file plans");
            assert_eq!(plain.final_rel_path, want, "{}", p.id);
            assert_eq!(
                plain.steps,
                [Step::Rename {
                    from: format!("dl.{ext}").into(),
                    to: want.clone()
                }],
                "{}",
                p.id
            );

            let member = format!("{name}.{ext}");
            let zipped = container(
                StagedKind::Zip,
                "dl.zip",
                &[(&member, 4096, &good_head(p.id))],
            );
            let from_zip = a.plan_placement(&e, &zipped).expect("zip plans");
            assert_eq!(
                from_zip.steps,
                [
                    Step::Unzip {
                        member: member.clone(),
                        to: member.clone().into()
                    },
                    Step::Rename {
                        from: member.into(),
                        to: want
                    },
                ],
                "{}",
                p.id
            );
        }
    }

    #[test]
    fn nes_headered_file_is_placed_unchanged() {
        let e = entry(
            "Example Quest (USA)",
            &[("Example Quest (USA).nes", 40_976)],
        );
        let plan = adapter("nes")
            .plan_placement(&e, &file("x.nes", 40_976, &ines()))
            .expect("plan");
        assert_eq!(plan.steps.len(), 1);
    }

    #[test]
    fn nes_headerless_file_gets_header_from_dat() {
        let mut e = entry(
            "Example Quest (USA)",
            &[("Example Quest (USA).nes", 40_976)],
        );
        e.roms[0].header = Some(ines());
        let plan = adapter("nes")
            .plan_placement(&e, &file("x.nes", 40_960, &[0xA9, 0]))
            .expect("plan");
        assert_eq!(
            plan.steps[0],
            Step::AddHeader {
                file: "x.nes".into(),
                bytes: ines()
            }
        );
    }

    #[test]
    fn nes_headerless_file_without_dat_header_is_refused() {
        let e = entry(
            "Example Quest (USA)",
            &[("Example Quest (USA).nes", 40_960)],
        );
        let err = adapter("nes").plan_placement(&e, &file("x.nes", 40_960, &[0xA9, 0]));
        assert!(matches!(err, Err(Error::MissingHeader)));
    }

    #[test]
    fn snes_smc_with_copier_header_is_stripped() {
        let e = entry(
            "Example Quest (USA)",
            &[("Example Quest (USA).sfc", 524_288)],
        );
        let plan = adapter("snes")
            .plan_placement(&e, &file("x.smc", 524_800, &[]))
            .expect("plan");
        assert_eq!(
            plan.steps[0],
            Step::StripHeader {
                file: "x.smc".into(),
                len: 512
            }
        );
        assert_eq!(
            plan.final_rel_path,
            Path::new("SNES/Example Quest (USA).sfc")
        );
    }

    #[test]
    fn snes_smc_without_header_is_only_renamed() {
        let e = entry(
            "Example Quest (USA)",
            &[("Example Quest (USA).sfc", 524_288)],
        );
        let plan = adapter("snes")
            .plan_placement(&e, &file("x.smc", 524_288, &[]))
            .expect("plan");
        assert_eq!(
            plan.steps,
            [Step::Rename {
                from: "x.smc".into(),
                to: "SNES/Example Quest (USA).sfc".into()
            }]
        );
    }

    #[test]
    fn n64_all_three_byte_orders() {
        let e = entry("Example Quest (USA)", &[("Example Quest (USA).z64", 8192)]);
        let cases: [(&[u8], Option<ByteOrder>); 3] = [
            (&[0x80, 0x37, 0x12, 0x40], None),
            (&[0x37, 0x80, 0x40, 0x12], Some(ByteOrder::ByteSwapped)),
            (&[0x40, 0x12, 0x37, 0x80], Some(ByteOrder::LittleEndian)),
        ];
        for (head, swap) in cases {
            let plan = adapter("n64")
                .plan_placement(&e, &file("x.n64", 8192, head))
                .expect("plan");
            assert_eq!(
                plan.final_rel_path,
                Path::new("N64/Example Quest (USA).z64")
            );
            let expected: Vec<Step> = swap
                .map(|from| Step::SwapByteOrder {
                    file: "x.n64".into(),
                    from,
                })
                .into_iter()
                .chain([Step::Rename {
                    from: "x.n64".into(),
                    to: plan.final_rel_path.clone(),
                }])
                .collect();
            assert_eq!(plan.steps, expected);
        }
    }

    #[test]
    fn n64_unknown_byte_order_is_refused() {
        let e = entry("Example Quest (USA)", &[("Example Quest (USA).z64", 8192)]);
        let err = adapter("n64").plan_placement(&e, &file("x.z64", 8192, &[1, 2, 3, 4]));
        assert!(matches!(err, Err(Error::UnknownByteOrder)));
    }

    #[test]
    fn multi_member_zip_picks_the_dat_rom() {
        let e = entry("Example Quest (USA)", &[("Example Quest (USA).gba", 4096)]);
        let z = container(
            StagedKind::Zip,
            "dl.zip",
            &[
                ("readme.txt", 10, b""),
                ("Example Quest (USA).gba", 4096, b""),
            ],
        );
        let plan = adapter("gba").plan_placement(&e, &z).expect("plan");
        assert!(
            matches!(&plan.steps[0], Step::Unzip { member, .. } if member == "Example Quest (USA).gba")
        );
    }

    #[test]
    fn empty_entry_and_unsafe_name_are_refused() {
        let empty = entry("Example Quest (USA)", &[]);
        assert!(adapter("gba")
            .plan_placement(&empty, &file("x.gba", 1, &[]))
            .is_err());
        let bad = entry("..", &[("x.gba", 1)]);
        assert!(matches!(
            adapter("gba").plan_placement(&bad, &file("x.gba", 1, &[])),
            Err(Error::InvalidName(_))
        ));
    }
}
