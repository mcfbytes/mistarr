//! The running core from `/tmp/CORENAME` and the cores installed on the SD card.

use std::path::{Path, PathBuf};

use mistarr_core::PlatformId;

use crate::platforms::{by_id, for_core, Platform};
use crate::Result;

/// Directories under the SD root that hold `.rbf` cores.
pub const CORE_DIRS: [&str; 4] = ["_Console", "_Computer", ARCADE_DIR, "_Other"];

/// Core directory whose `.rbf` files all belong to the arcade platform.
const ARCADE_DIR: &str = "_Arcade";

/// What the MiSTer main process is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreState {
    /// The menu core is loaded; background work may run.
    Menu,
    /// A core with this name is running.
    Running(String),
}

/// An installed `.rbf` core and the platforms it loads, where the table knows them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledCore {
    /// Core name with any `_YYYYMMDD` date suffix removed.
    pub name: String,
    /// Platform rows served by this core; empty when the table has none.
    pub platforms: Vec<PlatformId>,
}

/// Reads the core state from a CORENAME file; an empty file counts as the menu.
///
/// # Errors
///
/// Returns [`crate::Error::Io`] when the file cannot be read.
///
/// ```
/// use mistarr_mister::corename::{read_corename, CoreState};
/// let path = std::env::temp_dir().join("mistarr-doc-corename");
/// std::fs::write(&path, "SNES\n").unwrap();
/// assert_eq!(read_corename(&path).unwrap(), CoreState::Running("SNES".into()));
/// ```
pub fn read_corename(path: &Path) -> Result<CoreState> {
    let raw = std::fs::read_to_string(path)?;
    let name = raw.trim_matches(|c: char| c.is_whitespace() || c == '\0');
    Ok(if name.is_empty() || name == "MENU" {
        CoreState::Menu
    } else {
        CoreState::Running(name.to_owned())
    })
}

/// Lists `.rbf` cores under the [`CORE_DIRS`] of `root` and one level of subfolders,
/// deduplicated by name and sorted. Every core under `_Arcade` maps to the arcade platform.
///
/// ```
/// let root = std::env::temp_dir().join("mistarr-doc-cores");
/// std::fs::create_dir_all(root.join("_Console")).unwrap();
/// std::fs::write(root.join("_Console/SNES_20240101.rbf"), b"").unwrap();
/// let cores = mistarr_mister::corename::installed_cores(&root);
/// assert!(cores.iter().any(|c| c.name == "SNES" && c.platforms[0].0 == "snes"));
/// ```
#[must_use]
pub fn installed_cores(root: &Path) -> Vec<InstalledCore> {
    let mut found: Vec<(String, bool)> = rbf_files(root)
        .into_iter()
        .map(|f| (f.name, f.arcade))
        .collect();
    found.sort_unstable();
    found.dedup_by(|a, b| a.0 == b.0);
    found
        .into_iter()
        .map(|(name, arcade)| InstalledCore {
            platforms: if arcade {
                by_id("arcade")
                    .map(Platform::platform_id)
                    .into_iter()
                    .collect()
            } else {
                for_core(&name).iter().map(|p| p.platform_id()).collect()
            },
            name,
        })
        .collect()
}

/// One `.rbf` file found under a [`CORE_DIRS`] directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RbfFile {
    /// Core name with any `_YYYYMMDD` date suffix removed.
    pub name: String,
    /// The `YYYYMMDD` suffix, when the stem has one.
    pub date: Option<String>,
    /// Full path of the file.
    pub path: PathBuf,
    /// Found under `_Arcade`.
    pub arcade: bool,
}

/// Every `.rbf` under the [`CORE_DIRS`] of `root` and one level of subfolders.
pub(crate) fn rbf_files(root: &Path) -> Vec<RbfFile> {
    let mut found = Vec::new();
    for dir in CORE_DIRS {
        collect_rbf(&root.join(dir), 1, dir == ARCADE_DIR, &mut found);
    }
    found
}

/// Collects every `.rbf` in `dir`, descending `depth` more folder levels.
fn collect_rbf(dir: &Path, depth: u8, arcade: bool, out: &mut Vec<RbfFile>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(std::result::Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            if depth > 0 {
                collect_rbf(&path, depth - 1, arcade, out);
            }
        } else if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("rbf"))
        {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                let (name, date) = split_date(stem);
                out.push(RbfFile {
                    name: name.to_owned(),
                    date: date.map(str::to_owned),
                    arcade,
                    path,
                });
            }
        }
    }
}

/// Splits a trailing `_YYYYMMDD` release date from an `.rbf` file stem.
fn split_date(stem: &str) -> (&str, Option<&str>) {
    match stem.rsplit_once('_') {
        Some((name, date)) if date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit()) => {
            (name, Some(date))
        }
        _ => (stem, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::testutil::scratch;

    #[test]
    fn corename_states() {
        let dir = scratch("corename");
        let path = dir.join("CORENAME");
        for (text, want) in [
            ("MENU", CoreState::Menu),
            ("MENU\n", CoreState::Menu),
            ("", CoreState::Menu),
            ("N64\0", CoreState::Running("N64".into())),
        ] {
            std::fs::write(&path, text).expect("write");
            assert_eq!(read_corename(&path).expect("read"), want);
        }
        assert!(read_corename(&dir.join("absent")).is_err());
    }

    #[test]
    fn installed_cores_walks_one_level_and_maps_platforms() {
        let root = scratch("cores");
        for (dir, file) in [
            ("_Console", "NES_20240101.rbf"),
            ("_Console", "NES_20230101.rbf"),
            ("_Console/Older", "Gameboy_20220101.rbf"),
            ("_Console/Older/Deeper", "GBA_20220101.rbf"),
            ("_Computer", "Minimig_20240101.rbf"),
            ("_Arcade/cores", "examplecore_20240101.rbf"),
            ("_Other", "notes.txt"),
        ] {
            std::fs::create_dir_all(root.join(dir)).expect("mkdir");
            std::fs::write(root.join(dir).join(file), b"").expect("write");
        }
        let cores = installed_cores(&root);
        let names: Vec<_> = cores.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["Gameboy", "Minimig", "NES", "examplecore"]);
        let nes = cores.iter().find(|c| c.name == "NES").expect("nes");
        assert_eq!(
            nes.platforms,
            [PlatformId("nes".into()), PlatformId("fds".into())]
        );
        assert!(cores
            .iter()
            .find(|c| c.name == "Minimig")
            .is_some_and(|c| c.platforms.is_empty()));
    }

    #[test]
    fn every_core_under_arcade_maps_to_the_arcade_platform() {
        let root = scratch("arcade-cores");
        for (dir, file) in [
            ("_Arcade", "Arcade-Example_20240101.rbf"),
            ("_Arcade/cores", "examplecore_20240101.rbf"),
            ("_Arcade", "Example Blaster.mra"),
        ] {
            std::fs::create_dir_all(root.join(dir)).expect("mkdir");
            std::fs::write(root.join(dir).join(file), b"").expect("write");
        }
        let cores = installed_cores(&root);
        assert_eq!(cores.len(), 2);
        for core in &cores {
            assert_eq!(
                core.platforms,
                [PlatformId("arcade".into())],
                "{}",
                core.name
            );
        }
    }

    #[test]
    fn split_date_strips_only_dates() {
        assert_eq!(split_date("SNES_20240101"), ("SNES", Some("20240101")));
        assert_eq!(split_date("Atari_2600"), ("Atari_2600", None));
        assert_eq!(split_date("GBA"), ("GBA", None));
    }

    #[test]
    fn rbf_files_keep_dates_and_paths() {
        let root = scratch("rbf-files");
        std::fs::create_dir_all(root.join("_Console")).expect("mkdir");
        std::fs::write(root.join("_Console/NES_20240101.rbf"), b"").expect("write");
        let files = rbf_files(&root);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].date.as_deref(), Some("20240101"));
        assert_eq!(files[0].path, root.join("_Console/NES_20240101.rbf"));
        assert!(!files[0].arcade);
    }
}
