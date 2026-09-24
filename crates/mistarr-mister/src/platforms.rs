//! The platform table from `docs/PLATFORMS.md` and DAT-name binding.

use std::sync::OnceLock;

use mistarr_core::PlatformId;
use regex_lite::Regex;

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
};

const DISC: Platform = Platform {
    kind: Kind::Disc,
    load_extensions: &["cue", "iso", "chd"],
    ..CART
};

/// Every platform, in the order of `docs/PLATFORMS.md`.
pub static PLATFORMS: [Platform; 33] = [
    Platform {
        id: "nes",
        name: "Nintendo Entertainment System",
        core_dir: "NES",
        extension_written: Some("nes"),
        load_extensions: &["nes"],
        dat_name_patterns: &["nintendo entertainment system", "nes"],
        header_rule: "ines",
        ..CART
    },
    Platform {
        id: "fds",
        name: "Famicom Disk System",
        core_dir: "NES",
        extension_written: Some("fds"),
        load_extensions: &["fds"],
        dat_name_patterns: &["famicom disk system", "family computer disk system"],
        bios: Some("boot0.rom"),
        ..CART
    },
    Platform {
        id: "snes",
        name: "Super Nintendo Entertainment System",
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
        name: "Nintendo 64",
        core_dir: "N64",
        extension_written: Some("z64"),
        load_extensions: &["z64", "v64", "n64"],
        dat_name_patterns: &["nintendo 64"],
        header_rule: "n64",
        ..CART
    },
    Platform {
        id: "gb",
        name: "Game Boy",
        core_dir: "GAMEBOY",
        extension_written: Some("gb"),
        load_extensions: &["gb"],
        dat_name_patterns: &["game boy"],
        verify_on_board: true,
        ..CART
    },
    Platform {
        id: "gbc",
        name: "Game Boy Color",
        core_dir: "GAMEBOY",
        extension_written: Some("gbc"),
        load_extensions: &["gbc"],
        dat_name_patterns: &["game boy color"],
        verify_on_board: true,
        ..CART
    },
    Platform {
        id: "gba",
        name: "Game Boy Advance",
        core_dir: "GBA",
        extension_written: Some("gba"),
        load_extensions: &["gba"],
        dat_name_patterns: &["game boy advance"],
        ..CART
    },
    Platform {
        id: "megadrive",
        name: "Mega Drive - Genesis",
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
        name: "32X",
        core_dir: "S32X",
        extension_written: Some("32x"),
        load_extensions: &["32x"],
        dat_name_patterns: &["32x"],
        ..CART
    },
    Platform {
        id: "sms",
        name: "Master System - Mark III",
        core_dir: "SMS",
        extension_written: Some("sms"),
        load_extensions: &["sms"],
        dat_name_patterns: &["master system mark iii"],
        ..CART
    },
    Platform {
        id: "gg",
        name: "Game Gear",
        core_dir: "SMS",
        extension_written: Some("gg"),
        load_extensions: &["gg"],
        dat_name_patterns: &["game gear"],
        ..CART
    },
    Platform {
        id: "sg1000",
        name: "SG-1000",
        core_dir: "SG1000",
        extension_written: Some("sg"),
        load_extensions: &["sg"],
        dat_name_patterns: &["sg 1000"],
        ..CART
    },
    Platform {
        id: "pce",
        name: "PC Engine - TurboGrafx-16",
        core_dir: "TGFX16",
        extension_written: Some("pce"),
        load_extensions: &["pce"],
        dat_name_patterns: &["pc engine turbografx 16"],
        core_names: &["TurboGrafx16"],
        ..CART
    },
    Platform {
        id: "sgx",
        name: "SuperGrafx",
        core_dir: "TGFX16",
        extension_written: Some("sgx"),
        load_extensions: &["sgx"],
        dat_name_patterns: &["supergrafx"],
        core_names: &["TurboGrafx16"],
        ..CART
    },
    Platform {
        id: "atari2600",
        name: "Atari 2600",
        core_dir: "Atari2600",
        extension_written: Some("a26"),
        load_extensions: &["a26"],
        dat_name_patterns: &["atari 2600"],
        ..CART
    },
    Platform {
        id: "atari5200",
        name: "Atari 5200",
        core_dir: "Atari5200",
        extension_written: Some("a52"),
        load_extensions: &["a52"],
        dat_name_patterns: &["atari 5200"],
        ..CART
    },
    Platform {
        id: "atari7800",
        name: "Atari 7800",
        core_dir: "Atari7800",
        extension_written: Some("a78"),
        load_extensions: &["a78"],
        dat_name_patterns: &["atari 7800"],
        header_rule: "a78",
        ..CART
    },
    Platform {
        id: "lynx",
        name: "Atari Lynx",
        core_dir: "AtariLynx",
        extension_written: Some("lnx"),
        load_extensions: &["lnx"],
        dat_name_patterns: &["atari lynx"],
        header_rule: "lnx",
        ..CART
    },
    Platform {
        id: "coleco",
        name: "ColecoVision",
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
        name: "Intellivision",
        core_dir: "Intellivision",
        extension_written: Some("int"),
        load_extensions: &["int"],
        dat_name_patterns: &["intellivision"],
        bios: Some("boot0.rom"),
        ..CART
    },
    Platform {
        id: "ws",
        name: "WonderSwan",
        core_dir: "WonderSwan",
        extension_written: Some("ws"),
        load_extensions: &["ws"],
        dat_name_patterns: &["wonderswan"],
        ..CART
    },
    Platform {
        id: "wsc",
        name: "WonderSwan Color",
        core_dir: "WonderSwan",
        extension_written: Some("wsc"),
        load_extensions: &["wsc"],
        dat_name_patterns: &["wonderswan color"],
        ..CART
    },
    Platform {
        id: "ngp",
        name: "Neo Geo Pocket",
        core_dir: "NGP",
        extension_written: Some("ngp"),
        load_extensions: &["ngp", "ngc"],
        dat_name_patterns: &["neo geo pocket"],
        verify_on_board: true,
        ..CART
    },
    Platform {
        id: "vectrex",
        name: "Vectrex",
        core_dir: "Vectrex",
        extension_written: Some("vec"),
        load_extensions: &["vec"],
        dat_name_patterns: &["vectrex"],
        ..CART
    },
    Platform {
        id: "pokemini",
        name: "Pokemon Mini",
        core_dir: "PokemonMini",
        extension_written: Some("min"),
        load_extensions: &["min"],
        dat_name_patterns: &["pokemon mini"],
        ..CART
    },
    Platform {
        id: "sv",
        name: "Supervision",
        core_dir: "SuperVision",
        extension_written: Some("sv"),
        load_extensions: &["sv"],
        dat_name_patterns: &["supervision"],
        ..CART
    },
    Platform {
        id: "psx",
        name: "PlayStation",
        core_dir: "PSX",
        dat_name_patterns: &["playstation$"],
        bios: Some("boot.rom"),
        ..DISC
    },
    Platform {
        id: "saturn",
        name: "Sega Saturn",
        core_dir: "Saturn",
        dat_name_patterns: &["sega saturn"],
        bios: Some("boot.rom"),
        ..DISC
    },
    Platform {
        id: "megacd",
        name: "Mega CD - Sega CD",
        core_dir: "MegaCD",
        dat_name_patterns: &["mega cd sega cd"],
        bios: Some("cd_bios.rom"),
        ..DISC
    },
    Platform {
        id: "pcecd",
        name: "PC Engine CD - TurboGrafx-CD",
        core_dir: "TGFX16-CD",
        dat_name_patterns: &["pc engine cd turbografx cd"],
        bios: Some("cd_bios.rom"),
        core_names: &["TurboGrafx16"],
        ..DISC
    },
    Platform {
        id: "neocd",
        name: "Neo Geo CD",
        core_dir: "NeoGeo-CD",
        dat_name_patterns: &["neo geo cd"],
        bios: Some("top-sp1.bin"),
        verify_on_board: true,
        core_names: &["NeoGeo"],
        ..DISC
    },
    Platform {
        id: "neogeo",
        name: "Neo Geo",
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
        core_dir: "mame",
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
    let norm = normalise(name);
    let mut best: Option<(usize, &'static Platform)> = None;
    for (row, re) in compiled() {
        if let Some(m) = re.find(&norm) {
            if best.map_or(true, |(len, _)| m.len() > len) {
                best = Some((m.len(), row));
            }
        }
    }
    best.map(|(_, row)| row)
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
}
