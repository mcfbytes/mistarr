//! The platform table from `docs/PLATFORMS.md` and DAT-name binding.

use std::sync::OnceLock;

use mistarr_core::PlatformId;
use regex_lite::Regex;

use crate::launch::{LaunchCore, LaunchSlot, LoadMode};

/// How a platform's games are laid out and which adapter family places them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Kind {
    /// One file per game, optionally zipped in staging.
    Cartridge,
    /// A cue with tracks or a single image, in a per-title directory.
    Disc,
    /// A zip or directory per game whose members the core expects by name.
    Romset,
    /// MAME zips under `games/mame`, requested by MRA files.
    Arcade,
}

/// One row of the platform table.
#[derive(Debug, PartialEq, Eq)]
pub struct Platform {
    /// Stable slug used in the database and API.
    pub id: &'static str,
    /// Descriptive platform name for display.
    pub name: &'static str,
    /// Directory under `games/` the core reads from.
    pub core_dir: &'static str,
    /// Other directory names accepted on scan.
    pub legacy_dirs: &'static [&'static str],
    /// Adapter family.
    pub kind: Kind,
    /// Extension, without the dot, given to placed files; `None` when names come from the DAT.
    pub extension_written: Option<&'static str>,
    /// Extensions, without the dot, the core loads as-is.
    pub load_extensions: &'static [&'static str],
    /// Regular expressions matched against the normalised DAT name; see [`bind_dat_name`].
    pub dat_name_patterns: &'static [&'static str],
    /// Header rule name from the "Header rules" table.
    pub header_rule: &'static str,
    /// BIOS file name the core documents, reported and never handled.
    pub bios: Option<&'static str>,
    /// Row must be confirmed against a live board.
    pub verify_on_board: bool,
    /// `.rbf` core names that load this platform, besides [`Platform::core_dir`].
    pub core_names: &'static [&'static str],
    /// The libretro playlist name that thumbnail URLs are built from.
    pub libretro_playlist: &'static str,
    /// Cores that launch this platform's games, most preferred first; empty when
    /// games start another way.
    pub launch: &'static [LaunchCore],
}

impl Platform {
    /// The row's id as a [`PlatformId`].
    ///
    /// ```
    /// let row = mistarr_mister::platforms::by_id("nes").unwrap();
    /// assert_eq!(row.platform_id().0, "nes");
    /// ```
    #[must_use]
    pub fn platform_id(&self) -> PlatformId {
        PlatformId(self.id.to_owned())
    }
}

const CART: Platform = Platform {
    id: "",
    name: "",
    core_dir: "",
    legacy_dirs: &[],
    kind: Kind::Cartridge,
    extension_written: None,
    load_extensions: &[],
    dat_name_patterns: &[],
    header_rule: "none",
    bios: None,
    verify_on_board: false,
    core_names: &[],
    libretro_playlist: "",
    launch: &[],
};

const DISC: Platform = Platform {
    kind: Kind::Disc,
    load_extensions: &["cue", "iso", "chd"],
    ..CART
};

/// Loads a game file into slot `index` after `delay` seconds; values await board verification.
const fn file(index: u8, delay: u8) -> LaunchSlot {
    slot(LoadMode::File, index, delay)
}

/// Mounts a disc image in slot `index` after `delay` seconds; values await board verification.
const fn mount(index: u8, delay: u8) -> LaunchSlot {
    slot(LoadMode::Mount, index, delay)
}

/// A core outside `_Arcade` named `name`.
const fn core(name: &'static str, slot: LaunchSlot) -> LaunchCore {
    LaunchCore {
        name,
        arcade_dir: false,
        slot,
    }
}

/// A core the row names explicitly that may live under `_Arcade`.
const fn arcade_core(name: &'static str, slot: LaunchSlot) -> LaunchCore {
    LaunchCore {
        name,
        arcade_dir: true,
        slot,
    }
}

const fn slot(mode: LoadMode, index: u8, delay: u8) -> LaunchSlot {
    LaunchSlot {
        mode,
        index,
        delay,
        verify_on_board: true,
    }
}

/// Every platform, in the order of `docs/PLATFORMS.md`.
pub static PLATFORMS: [Platform; 33] = [
    Platform {
        id: "nes",
        launch: &[core("NES", file(1, 2))],
        name: "Nintendo Entertainment System",
        libretro_playlist: "Nintendo - Nintendo Entertainment System",
        core_dir: "NES",
        extension_written: Some("nes"),
        load_extensions: &["nes"],
        dat_name_patterns: &["nintendo entertainment system", "nes"],
        header_rule: "ines",
        ..CART
    },
    Platform {
        id: "fds",
        launch: &[core("NES", file(1, 2))],
        name: "Famicom Disk System",
        libretro_playlist: "Nintendo - Family Computer Disk System",
        core_dir: "NES",
        extension_written: Some("fds"),
        load_extensions: &["fds"],
        dat_name_patterns: &["famicom disk system", "family computer disk system"],
        bios: Some("boot0.rom"),
        ..CART
    },
    Platform {
        id: "snes",
        launch: &[core("SNES", file(0, 2))],
        name: "Super Nintendo Entertainment System",
        libretro_playlist: "Nintendo - Super Nintendo Entertainment System",
        core_dir: "SNES",
        extension_written: Some("sfc"),
        load_extensions: &["sfc", "smc"],
        dat_name_patterns: &[
            "super nintendo entertainment system",
            "super famicom",
            "satellaview",
        ],
        header_rule: "smc",
        ..CART
    },
    Platform {
        id: "n64",
        launch: &[core("N64", file(1, 1))],
        name: "Nintendo 64",
        libretro_playlist: "Nintendo - Nintendo 64",
        core_dir: "N64",
        extension_written: Some("z64"),
        load_extensions: &["z64", "v64", "n64"],
        dat_name_patterns: &["nintendo 64"],
        header_rule: "n64",
        ..CART
    },
    Platform {
        id: "gb",
        launch: &[core("Gameboy", file(1, 2))],
        name: "Game Boy",
        libretro_playlist: "Nintendo - Game Boy",
        core_dir: "GAMEBOY",
        extension_written: Some("gb"),
        load_extensions: &["gb"],
        dat_name_patterns: &["game boy"],
        verify_on_board: true,
        ..CART
    },
    Platform {
        id: "gbc",
        launch: &[core("Gameboy", file(1, 2))],
        name: "Game Boy Color",
        libretro_playlist: "Nintendo - Game Boy Color",
        core_dir: "GAMEBOY",
        extension_written: Some("gbc"),
        load_extensions: &["gbc"],
        dat_name_patterns: &["game boy color"],
        verify_on_board: true,
        ..CART
    },
    Platform {
        id: "gba",
        launch: &[core("GBA", file(1, 2))],
        name: "Game Boy Advance",
        libretro_playlist: "Nintendo - Game Boy Advance",
        core_dir: "GBA",
        extension_written: Some("gba"),
        load_extensions: &["gba"],
        dat_name_patterns: &["game boy advance"],
        ..CART
    },
    Platform {
        id: "megadrive",
        launch: &[core("MegaDrive", file(1, 1)), core("Genesis", file(1, 1))],
        name: "Mega Drive - Genesis",
        libretro_playlist: "Sega - Mega Drive - Genesis",
        core_dir: "Genesis",
        legacy_dirs: &["MegaDrive"],
        extension_written: Some("md"),
        load_extensions: &["md", "gen", "bin"],
        dat_name_patterns: &["mega drive genesis"],
        verify_on_board: true,
        ..CART
    },
    Platform {
        id: "s32x",
        launch: &[core("S32X", file(1, 1))],
        name: "32X",
        libretro_playlist: "Sega - 32X",
        core_dir: "S32X",
        extension_written: Some("32x"),
        load_extensions: &["32x"],
        dat_name_patterns: &["32x"],
        ..CART
    },
    Platform {
        id: "sms",
        launch: &[core("SMS", file(1, 1))],
        name: "Master System - Mark III",
        libretro_playlist: "Sega - Master System - Mark III",
        core_dir: "SMS",
        extension_written: Some("sms"),
        load_extensions: &["sms"],
        dat_name_patterns: &["master system mark iii"],
        ..CART
    },
    Platform {
        id: "gg",
        launch: &[core("SMS", file(2, 1))],
        name: "Game Gear",
        libretro_playlist: "Sega - Game Gear",
        core_dir: "SMS",
        extension_written: Some("gg"),
        load_extensions: &["gg"],
        dat_name_patterns: &["game gear"],
        ..CART
    },
    Platform {
        id: "sg1000",
        launch: &[core("ColecoVision", file(0, 1))],
        name: "SG-1000",
        libretro_playlist: "Sega - SG-1000",
        core_dir: "SG1000",
        extension_written: Some("sg"),
        load_extensions: &["sg"],
        dat_name_patterns: &["sg 1000"],
        ..CART
    },
    Platform {
        id: "pce",
        launch: &[core("TurboGrafx16", file(0, 1))],
        name: "PC Engine - TurboGrafx-16",
        libretro_playlist: "NEC - PC Engine - TurboGrafx 16",
        core_dir: "TGFX16",
        extension_written: Some("pce"),
        load_extensions: &["pce"],
        dat_name_patterns: &["pc engine turbografx 16"],
        core_names: &["TurboGrafx16"],
        ..CART
    },
    Platform {
        id: "sgx",
        launch: &[core("TurboGrafx16", file(1, 1))],
        name: "SuperGrafx",
        libretro_playlist: "NEC - PC Engine SuperGrafx",
        core_dir: "TGFX16",
        extension_written: Some("sgx"),
        load_extensions: &["sgx"],
        dat_name_patterns: &["supergrafx"],
        core_names: &["TurboGrafx16"],
        ..CART
    },
    Platform {
        id: "atari2600",
        launch: &[core("Atari2600", file(1, 1)), core("Atari7800", file(1, 1))],
        name: "Atari 2600",
        libretro_playlist: "Atari - 2600",
        core_dir: "Atari2600",
        extension_written: Some("a26"),
        load_extensions: &["a26"],
        dat_name_patterns: &["atari 2600"],
        ..CART
    },
    Platform {
        id: "atari5200",
        launch: &[core("Atari5200", mount(1, 1))],
        name: "Atari 5200",
        libretro_playlist: "Atari - 5200",
        core_dir: "Atari5200",
        extension_written: Some("a52"),
        load_extensions: &["a52"],
        dat_name_patterns: &["atari 5200"],
        ..CART
    },
    Platform {
        id: "atari7800",
        launch: &[core("Atari7800", file(1, 1))],
        name: "Atari 7800",
        libretro_playlist: "Atari - 7800",
        core_dir: "Atari7800",
        extension_written: Some("a78"),
        load_extensions: &["a78"],
        dat_name_patterns: &["atari 7800"],
        header_rule: "a78",
        ..CART
    },
    Platform {
        id: "lynx",
        launch: &[core("AtariLynx", file(1, 1))],
        name: "Atari Lynx",
        libretro_playlist: "Atari - Lynx",
        core_dir: "AtariLynx",
        extension_written: Some("lnx"),
        load_extensions: &["lnx"],
        dat_name_patterns: &["atari lynx"],
        header_rule: "lnx",
        ..CART
    },
    Platform {
        id: "coleco",
        launch: &[core("ColecoVision", file(1, 1))],
        name: "ColecoVision",
        libretro_playlist: "Coleco - ColecoVision",
        core_dir: "Coleco",
        extension_written: Some("col"),
        load_extensions: &["col"],
        dat_name_patterns: &["colecovision"],
        bios: Some("boot0.rom"),
        core_names: &["ColecoVision"],
        ..CART
    },
    Platform {
        id: "intv",
        launch: &[core("Intellivision", file(1, 1))],
        name: "Intellivision",
        libretro_playlist: "Mattel - Intellivision",
        core_dir: "Intellivision",
        extension_written: Some("int"),
        load_extensions: &["int"],
        dat_name_patterns: &["intellivision"],
        bios: Some("boot0.rom"),
        ..CART
    },
    Platform {
        id: "ws",
        launch: &[core("WonderSwan", file(1, 1))],
        name: "WonderSwan",
        libretro_playlist: "Bandai - WonderSwan",
        core_dir: "WonderSwan",
        extension_written: Some("ws"),
        load_extensions: &["ws"],
        dat_name_patterns: &["wonderswan"],
        ..CART
    },
    Platform {
        id: "wsc",
        launch: &[core("WonderSwan", file(1, 1))],
        name: "WonderSwan Color",
        libretro_playlist: "Bandai - WonderSwan Color",
        core_dir: "WonderSwan",
        extension_written: Some("wsc"),
        load_extensions: &["wsc"],
        dat_name_patterns: &["wonderswan color"],
        ..CART
    },
    Platform {
        id: "ngp",
        launch: &[arcade_core("jtngp", file(1, 2))],
        name: "Neo Geo Pocket",
        libretro_playlist: "SNK - Neo Geo Pocket",
        core_dir: "NGP",
        extension_written: Some("ngp"),
        load_extensions: &["ngp", "ngc"],
        dat_name_patterns: &["neo geo pocket"],
        verify_on_board: true,
        ..CART
    },
    Platform {
        id: "vectrex",
        launch: &[core("Vectrex", file(1, 1))],
        name: "Vectrex",
        libretro_playlist: "GCE - Vectrex",
        core_dir: "Vectrex",
        extension_written: Some("vec"),
        load_extensions: &["vec"],
        dat_name_patterns: &["vectrex"],
        ..CART
    },
    Platform {
        id: "pokemini",
        launch: &[core("PokemonMini", file(1, 1))],
        name: "Pokemon Mini",
        libretro_playlist: "Nintendo - Pokemon Mini",
        core_dir: "PokemonMini",
        extension_written: Some("min"),
        load_extensions: &["min"],
        dat_name_patterns: &["pokemon mini"],
        ..CART
    },
    Platform {
        id: "sv",
        launch: &[core("SuperVision", file(1, 1))],
        name: "Supervision",
        libretro_playlist: "Watara - Supervision",
        core_dir: "SuperVision",
        extension_written: Some("sv"),
        load_extensions: &["sv"],
        dat_name_patterns: &["supervision"],
        ..CART
    },
    Platform {
        id: "psx",
        launch: &[core("PSX", mount(1, 1))],
        name: "PlayStation",
        libretro_playlist: "Sony - PlayStation",
        core_dir: "PSX",
        dat_name_patterns: &["playstation$"],
        bios: Some("boot.rom"),
        ..DISC
    },
    Platform {
        id: "saturn",
        launch: &[core("Saturn", mount(0, 2))],
        name: "Sega Saturn",
        libretro_playlist: "Sega - Saturn",
        core_dir: "Saturn",
        dat_name_patterns: &["sega saturn"],
        bios: Some("boot.rom"),
        ..DISC
    },
    Platform {
        id: "megacd",
        launch: &[core("MegaCD", mount(0, 1))],
        name: "Mega CD - Sega CD",
        libretro_playlist: "Sega - Mega-CD - Sega CD",
        core_dir: "MegaCD",
        dat_name_patterns: &["mega cd sega cd"],
        bios: Some("cd_bios.rom"),
        ..DISC
    },
    Platform {
        id: "pcecd",
        launch: &[core("TurboGrafx16", mount(0, 1))],
        name: "PC Engine CD - TurboGrafx-CD",
        libretro_playlist: "NEC - PC Engine CD - TurboGrafx-CD",
        core_dir: "TGFX16-CD",
        dat_name_patterns: &["pc engine cd turbografx cd"],
        bios: Some("cd_bios.rom"),
        core_names: &["TurboGrafx16"],
        ..DISC
    },
    Platform {
        id: "neocd",
        launch: &[core("NeoGeo", mount(1, 1))],
        name: "Neo Geo CD",
        libretro_playlist: "SNK - Neo Geo CD",
        core_dir: "NeoGeo-CD",
        dat_name_patterns: &["neo geo cd"],
        bios: Some("top-sp1.bin"),
        verify_on_board: true,
        core_names: &["NeoGeo"],
        ..DISC
    },
    Platform {
        id: "neogeo",
        launch: &[core("NeoGeo", file(1, 1))],
        name: "Neo Geo",
        libretro_playlist: "SNK - Neo Geo",
        core_dir: "NeoGeo",
        kind: Kind::Romset,
        load_extensions: &["zip"],
        dat_name_patterns: &["neo geo"],
        bios: Some("000-lo.lo, sfix.sfix, sp-s2.sp1"),
        ..CART
    },
    Platform {
        id: "arcade",
        name: "Arcade",
        libretro_playlist: "MAME",
        core_dir: "mame",
        legacy_dirs: &["hbmame"],
        kind: Kind::Arcade,
        load_extensions: &["zip"],
        dat_name_patterns: &["mame", "arcade"],
        ..CART
    },
];

/// Looks up a row by its id.
///
/// ```
/// use mistarr_mister::platforms::by_id;
/// assert_eq!(by_id("snes").map(|p| p.core_dir), Some("SNES"));
/// assert!(by_id("unknown").is_none());
/// ```
#[must_use]
pub fn by_id(id: &str) -> Option<&'static Platform> {
    PLATFORMS.iter().find(|p| p.id == id)
}

/// Binds a DAT header name to a platform, or `None` when nothing matches.
///
/// The name is lowercased, bracketed groups are dropped and every run of
/// other characters becomes one space. Each pattern must match on word
/// boundaries; the longest match wins, so a more specific row beats its prefix.
///
/// ```
/// use mistarr_mister::bind_dat_name;
/// assert_eq!(bind_dat_name("Maker - Game Boy Color").map(|p| p.id), Some("gbc"));
/// assert_eq!(bind_dat_name("Maker - Game Boy").map(|p| p.id), Some("gb"));
/// assert!(bind_dat_name("Test Console").is_none());
/// ```
#[must_use]
pub fn bind_dat_name(name: &str) -> Option<&'static Platform> {
    best_match(name).map(|(_, row)| row)
}

/// Guesses a platform with no DAT loaded from a torrent's `names` (the dropped
/// file's stem and its info name) and its `dirs`, each with the number of
/// files it holds. Each name is matched as in [`bind_dat_name`]. The names
/// decide when any matches; otherwise the matching directories that hold the
/// most files do. Two platforms at the deciding level mean no guess.
///
/// ```
/// use mistarr_mister::platforms::guess_platform;
/// let names = ["Example_Archive - No-Intro", "Nintendo - Super Nintendo Entertainment System"];
/// assert_eq!(guess_platform(names, []).map(|p| p.id), Some("snes"));
/// let dirs = [("Nintendo - Game Boy", 4), ("Nintendo - Game Boy Color", 4)];
/// assert!(guess_platform(["Example Archive"], dirs).is_none());
/// ```
#[must_use]
pub fn guess_platform<'a>(
    names: impl IntoIterator<Item = &'a str>,
    dirs: impl IntoIterator<Item = (&'a str, usize)>,
) -> Option<&'static Platform> {
    let from_names: Vec<&'static Platform> = names.into_iter().filter_map(bind_dat_name).collect();
    if !from_names.is_empty() {
        return unanimous(&from_names);
    }
    let matched: Vec<(usize, &'static Platform)> = dirs
        .into_iter()
        .filter_map(|(dir, n)| bind_dat_name(dir).map(|p| (n, p)))
        .collect();
    let top = matched.iter().map(|(n, _)| *n).max()?;
    let deciding: Vec<&'static Platform> = matched
        .into_iter()
        .filter(|(n, _)| *n == top)
        .map(|(_, p)| p)
        .collect();
    unanimous(&deciding)
}

/// The one platform every entry names, or `None` when they differ or there are none.
fn unanimous(found: &[&'static Platform]) -> Option<&'static Platform> {
    let first = *found.first()?;
    found.iter().all(|p| p.id == first.id).then_some(first)
}

/// The longest pattern match in `name` and its row.
fn best_match(name: &str) -> Option<(usize, &'static Platform)> {
    let norm = normalise(name);
    let mut best: Option<(usize, &'static Platform)> = None;
    for (row, re) in compiled() {
        if let Some(m) = re.find(&norm) {
            if best.is_none_or(|(len, _)| m.len() > len) {
                best = Some((m.len(), row));
            }
        }
    }
    best
}

/// Rows whose core is the `.rbf` named `core` (date suffix already removed).
///
/// ```
/// use mistarr_mister::platforms::for_core;
/// let ids: Vec<_> = for_core("SMS").iter().map(|p| p.id).collect();
/// assert_eq!(ids, ["sms", "gg"]);
/// ```
#[must_use]
pub fn for_core(core: &str) -> Vec<&'static Platform> {
    PLATFORMS
        .iter()
        .filter(|p| {
            p.core_dir.eq_ignore_ascii_case(core)
                || p.legacy_dirs.iter().any(|d| d.eq_ignore_ascii_case(core))
                || p.core_names.iter().any(|n| n.eq_ignore_ascii_case(core))
        })
        .collect()
}

fn compiled() -> &'static [(&'static Platform, Regex)] {
    static CELL: OnceLock<Vec<(&'static Platform, Regex)>> = OnceLock::new();
    CELL.get_or_init(|| {
        PLATFORMS
            .iter()
            .flat_map(|p| p.dat_name_patterns.iter().map(move |pat| (p, pat)))
            .filter_map(|(p, pat)| {
                Regex::new(&format!(r"\b(?:{pat})\b"))
                    .ok()
                    .map(|re| (p, re))
            })
            .collect()
    })
}

fn normalise(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut depth = 0u32;
    for c in name.chars().flat_map(char::to_lowercase) {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            _ if depth > 0 => {}
            c if c.is_alphanumeric() => out.push(c),
            _ if !out.is_empty() && !out.ends_with(' ') => out.push(' '),
            _ => {}
        }
    }
    let trimmed = out.trim_end().len();
    out.truncate(trimmed);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bind(name: &str) -> Option<&'static str> {
        bind_dat_name(name).map(|p| p.id)
    }

    #[test]
    fn every_pattern_compiles() {
        let total: usize = PLATFORMS.iter().map(|p| p.dat_name_patterns.len()).sum();
        assert_eq!(compiled().len(), total);
    }

    #[test]
    fn ids_are_unique_and_rows_complete() {
        for (i, p) in PLATFORMS.iter().enumerate() {
            assert!(!p.id.is_empty() && !p.core_dir.is_empty() && !p.name.is_empty());
            assert!(!p.dat_name_patterns.is_empty(), "{}", p.id);
            assert!(!p.load_extensions.is_empty(), "{}", p.id);
            assert!(PLATFORMS[i + 1..].iter().all(|q| q.id != p.id), "{}", p.id);
            assert!(!p.libretro_playlist.is_empty(), "{}", p.id);
            assert!(
                ["none", "ines", "smc", "a78", "lnx", "n64"].contains(&p.header_rule),
                "{}",
                p.id
            );
            if p.kind == Kind::Cartridge {
                let ext = p
                    .extension_written
                    .expect("cartridge rows write an extension");
                assert!(p.load_extensions.contains(&ext), "{}", p.id);
            } else {
                assert!(p.extension_written.is_none(), "{}", p.id);
            }
        }
    }

    #[test]
    fn every_row_but_arcade_has_launch_parameters() {
        for p in &PLATFORMS {
            assert_eq!(p.launch.is_empty(), p.kind == Kind::Arcade, "{}", p.id);
            for core in p.launch {
                assert!(core.slot.delay > 0, "{}", p.id);
                if p.kind == Kind::Disc {
                    assert_eq!(core.slot.mode, LoadMode::Mount, "{}", p.id);
                }
            }
        }
        let slot = |id: &str| by_id(id).expect("row").launch[0].slot;
        let nes = slot("nes");
        assert_eq!((nes.mode.letter(), nes.index, nes.delay), ('f', 1, 2));
        assert_eq!(slot("atari5200").mode, LoadMode::Mount);
        assert_eq!(slot("saturn").delay, 2);
        let ngp = by_id("ngp").expect("row").launch[0];
        assert!(ngp.arcade_dir && ngp.name == "jtngp");
        let a2600: Vec<_> = by_id("atari2600")
            .expect("row")
            .launch
            .iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(a2600, ["Atari2600", "Atari7800"]);
    }

    #[test]
    fn each_row_binds_its_own_name() {
        for p in &PLATFORMS {
            assert_eq!(
                bind(&format!("Maker - {}", p.name)),
                Some(p.id),
                "{}",
                p.name
            );
        }
    }

    #[test]
    fn longest_match_wins() {
        assert_eq!(bind("Nintendo - Game Boy"), Some("gb"));
        assert_eq!(bind("Nintendo - Game Boy Color"), Some("gbc"));
        assert_eq!(bind("Nintendo - Game Boy Advance"), Some("gba"));
        assert_eq!(bind("NEC - PC Engine - TurboGrafx-16"), Some("pce"));
        assert_eq!(bind("NEC - PC Engine CD - TurboGrafx-CD"), Some("pcecd"));
        assert_eq!(bind("NEC - PC Engine CD & TurboGrafx CD"), Some("pcecd"));
        assert_eq!(bind("NEC - PC Engine SuperGrafx"), Some("sgx"));
        assert_eq!(
            bind("Nintendo - Nintendo Entertainment System (Headered)"),
            Some("nes")
        );
        assert_eq!(
            bind("Nintendo - Super Nintendo Entertainment System"),
            Some("snes")
        );
        assert_eq!(
            bind("Nintendo - Family Computer Disk System (FDS)"),
            Some("fds")
        );
        assert_eq!(bind("Bandai - WonderSwan Color"), Some("wsc"));
        assert_eq!(bind("Bandai - WonderSwan"), Some("ws"));
        assert_eq!(bind("SNK - Neo Geo Pocket Color"), Some("ngp"));
        assert_eq!(bind("SNK - Neo Geo CD"), Some("neocd"));
        assert_eq!(bind("SNK - Neo Geo"), Some("neogeo"));
        assert_eq!(bind("Sega - Mega CD & Sega CD"), Some("megacd"));
        assert_eq!(bind("Sega - Mega Drive - Genesis"), Some("megadrive"));
        assert_eq!(bind("Atari - 2600"), Some("atari2600"));
        assert_eq!(bind("MAME 0.100"), Some("arcade"));
    }

    #[test]
    fn word_boundaries_and_near_misses() {
        assert_eq!(bind("Sony - PlayStation"), Some("psx"));
        assert_eq!(bind("Sony - PlayStation 2"), None);
        assert_eq!(bind("Sony - PlayStation Portable"), None);
        assert_eq!(bind("Nintendo - Nintendo 64DD"), None);
        assert_eq!(bind("Nintendo - Nintendo 64 (BigEndian)"), Some("n64"));
        assert_eq!(bind("Genesis Collection"), None);
        assert_eq!(bind(""), None);
    }

    #[test]
    fn normalise_drops_groups_and_punctuation() {
        assert_eq!(normalise("A - B (x) [y] C-D "), "a b c d");
    }

    #[test]
    fn for_core_maps_rbf_names() {
        let ids = |c| for_core(c).iter().map(|p| p.id).collect::<Vec<_>>();
        assert_eq!(ids("NES"), ["nes", "fds"]);
        assert_eq!(ids("Gameboy"), ["gb", "gbc"]);
        assert_eq!(ids("MegaDrive"), ["megadrive"]);
        assert_eq!(ids("TurboGrafx16"), ["pce", "sgx", "pcecd"]);
        assert!(ids("Minimig").is_empty());
    }

    #[test]
    fn platform_id_matches_row() {
        assert_eq!(
            by_id("psx").map(Platform::platform_id),
            Some(PlatformId("psx".into()))
        );
    }

    /// Rows marked **verify**, each with a `board_verify_<id>` test below.
    const BOARD_VERIFIED: [&str; 5] = ["gb", "gbc", "megadrive", "ngp", "neocd"];

    #[test]
    fn verify_rows_have_board_tests() {
        let ids: Vec<_> = PLATFORMS
            .iter()
            .filter(|p| p.verify_on_board)
            .map(|p| p.id)
            .collect();
        assert_eq!(ids, BOARD_VERIFIED);
    }

    /// Rows whose launch parameters are marked **verify**, all checked by
    /// `board_verify_launch_cores`; the slot values are confirmed by starting a game from the UI.
    const BOARD_LAUNCH: [&str; 32] = [
        "nes",
        "fds",
        "snes",
        "n64",
        "gb",
        "gbc",
        "gba",
        "megadrive",
        "s32x",
        "sms",
        "gg",
        "sg1000",
        "pce",
        "sgx",
        "atari2600",
        "atari5200",
        "atari7800",
        "lynx",
        "coleco",
        "intv",
        "ws",
        "wsc",
        "ngp",
        "vectrex",
        "pokemini",
        "sv",
        "psx",
        "saturn",
        "megacd",
        "pcecd",
        "neocd",
        "neogeo",
    ];

    #[test]
    fn verify_launch_rows_have_a_board_test() {
        let ids: Vec<_> = PLATFORMS
            .iter()
            .filter(|p| p.launch.iter().any(|c| c.slot.verify_on_board))
            .map(|p| p.id)
            .collect();
        assert_eq!(ids, BOARD_LAUNCH);
    }

    #[test]
    #[ignore = "needs a MiSTer SD card at MISTARR_BOARD_ROOT"]
    fn board_verify_launch_cores() {
        let root =
            std::env::var("MISTARR_BOARD_ROOT").expect("MISTARR_BOARD_ROOT is the SD card root");
        let missing: Vec<_> = BOARD_LAUNCH
            .iter()
            .filter(|id| {
                let row = by_id(id).expect("id is in the table");
                crate::launch::find_core(std::path::Path::new(&root), row).is_none()
            })
            .collect();
        assert!(missing.is_empty(), "no launch core found for {missing:?}");
    }

    fn assert_board_dir(id: &str) {
        let root =
            std::env::var("MISTARR_BOARD_ROOT").expect("MISTARR_BOARD_ROOT is the SD card root");
        let row = by_id(id).expect("id is in the table");
        let dir = std::path::Path::new(&root).join("games").join(row.core_dir);
        assert!(dir.is_dir(), "{} does not exist", dir.display());
    }

    #[test]
    #[ignore = "needs a MiSTer SD card at MISTARR_BOARD_ROOT"]
    fn board_verify_gb() {
        assert_board_dir("gb");
    }

    #[test]
    #[ignore = "needs a MiSTer SD card at MISTARR_BOARD_ROOT"]
    fn board_verify_gbc() {
        assert_board_dir("gbc");
    }

    #[test]
    #[ignore = "needs a MiSTer SD card at MISTARR_BOARD_ROOT"]
    fn board_verify_megadrive() {
        assert_board_dir("megadrive");
    }

    #[test]
    #[ignore = "needs a MiSTer SD card at MISTARR_BOARD_ROOT"]
    fn board_verify_ngp() {
        assert_board_dir("ngp");
    }

    #[test]
    #[ignore = "needs a MiSTer SD card at MISTARR_BOARD_ROOT"]
    fn board_verify_neocd() {
        assert_board_dir("neocd");
    }

    fn guess(names: &[&str]) -> Option<&'static str> {
        guess_platform(names.iter().copied(), []).map(|p| p.id)
    }

    fn guess_dirs(dirs: &[(&str, usize)]) -> Option<&'static str> {
        guess_platform(["Example Archive"], dirs.iter().copied()).map(|p| p.id)
    }

    #[test]
    fn a_set_torrent_is_guessed_from_its_names() {
        let file = "Example_Archive - No-Intro - Nintendo - Super Nintendo Entertainment System";
        assert_eq!(guess(&[file]), Some("snes"));
        assert_eq!(
            guess(&["Example_Archive", "No-Intro", "Nintendo - Game Boy Color"]),
            Some("gbc")
        );
        assert_eq!(guess(&["Maker - Game Boy", "Maker - Game Boy Color"]), None);
        assert_eq!(guess(&["Maker - Game Boy Color", "Nintendo - Game Boy Color"]), Some("gbc"));
        assert_eq!(guess(&["Sony - PlayStation (2026)"]), Some("psx"));
        assert_eq!(guess(&[]), None);
        assert_eq!(guess(&["Example_Archive", "misc"]), None);
    }

    #[test]
    fn directories_decide_by_file_count_when_the_names_do_not() {
        let half = [("Nintendo - Game Boy", 5), ("Nintendo - Game Boy Color", 5)];
        assert_eq!(guess_dirs(&half), None, "a 50/50 split is no guess");
        let most = [("Nintendo - Game Boy", 3), ("Nintendo - Game Boy Color", 7)];
        assert_eq!(guess_dirs(&most), Some("gbc"));
        let nested = [("Sets", 8), ("Nintendo - Game Boy", 8), ("misc", 8)];
        assert_eq!(guess_dirs(&nested), Some("gb"));
        let named = guess_platform(["Sega - Mega Drive - Genesis"], most).map(|p| p.id);
        assert_eq!(named, Some("megadrive"), "the names outrank the directories");
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        /// Separator-only noise, which never forms a platform word.
        fn noise() -> impl Strategy<Value = String> {
            "[ _.0-9-]{0,12}"
        }

        proptest! {
            #[test]
            fn never_panics(names in prop::collection::vec(".{0,40}", 0..6)) {
                let refs: Vec<&str> = names.iter().map(String::as_str).collect();
                let dirs = refs.iter().map(|d| (*d, d.len()));
                let _ = guess_platform(refs.clone(), dirs);
            }

            #[test]
            fn a_playlist_name_in_noise_is_guessed_as_its_dat_binding(
                row in 0..PLATFORMS.len(), before in noise(), after in "[ _.-]{0,6}"
            ) {
                let playlist = PLATFORMS[row].libretro_playlist;
                let wrapped = format!("{before} - {playlist}{after}");
                let want = bind_dat_name(playlist).map(|p| p.id);
                prop_assert_eq!(guess(&[&wrapped]), want);
                prop_assert_eq!(guess(&["Example Archive", &wrapped, "misc"]), want);
            }

            #[test]
            fn the_guess_is_what_every_matching_name_binds(
                names in prop::collection::vec("[A-Za-z _-]{0,30}", 0..5)
            ) {
                let refs: Vec<&str> = names.iter().map(String::as_str).collect();
                let got = guess(&refs);
                let each: Vec<_> = refs.iter().filter_map(|n| bind_dat_name(n).map(|p| p.id)).collect();
                let agreed = each.first().filter(|f| each.iter().all(|e| e == *f)).copied();
                prop_assert_eq!(got, agreed);
            }

            #[test]
            fn names_that_disagree_give_no_guess_in_any_order(swap: bool) {
                let (a, b) = ("Maker - Game Boy", "Maker - Game Boy Advance");
                let names = if swap { [b, a] } else { [a, b] };
                prop_assert_eq!(guess(&names), None);
                prop_assert_eq!(guess(&[b]), Some("gba"));
            }
        }
    }
}
