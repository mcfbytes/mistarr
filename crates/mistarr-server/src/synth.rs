//! A synthetic catalogue shaped like a full set of loaded DATs, for query tests, benchmarks
//! and `mistarr bench-seed`; see `docs/TESTING.md` "Browse speed".

use std::collections::HashSet;
use std::fmt::Write as _;

use rusqlite::{params, Connection};

use crate::db::{self, titles};
use crate::error::Result;

/// One platform of the catalogue at scale 1.
#[derive(Debug, Clone, Copy)]
pub struct Console {
    /// Platform id.
    pub id: &'static str,
    /// Titles at scale 1.
    pub titles: usize,
    /// Rom layout of each title.
    pub layout: Layout,
}

/// How a platform's titles hold their roms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// One rom file.
    Cartridge,
    /// One rom file whose DAT entry carries a header rule.
    Headered,
    /// A cue sheet and up to this many track files.
    Disc(usize),
    /// MRA titles naming one zip each.
    Mra,
}

/// The catalogue: DAT-sized platforms, disc sets with several tracks, arcade MRAs and an
/// add-on family, about 42 000 titles and 70 000 roms at scale 1.
pub const CONSOLES: [Console; 30] = [
    Console {
        id: "psx",
        titles: 11_000,
        layout: Layout::Disc(4),
    },
    Console {
        id: "saturn",
        titles: 3_000,
        layout: Layout::Disc(6),
    },
    Console {
        id: "nes",
        titles: 4_500,
        layout: Layout::Headered,
    },
    Console {
        id: "snes",
        titles: 4_000,
        layout: Layout::Cartridge,
    },
    Console {
        id: "megadrive",
        titles: 3_000,
        layout: Layout::Cartridge,
    },
    Console {
        id: "gb",
        titles: 1_800,
        layout: Layout::Cartridge,
    },
    Console {
        id: "gbc",
        titles: 1_500,
        layout: Layout::Cartridge,
    },
    Console {
        id: "gba",
        titles: 3_300,
        layout: Layout::Cartridge,
    },
    Console {
        id: "n64",
        titles: 1_000,
        layout: Layout::Cartridge,
    },
    Console {
        id: "sms",
        titles: 900,
        layout: Layout::Cartridge,
    },
    Console {
        id: "gg",
        titles: 700,
        layout: Layout::Cartridge,
    },
    Console {
        id: "pce",
        titles: 700,
        layout: Layout::Cartridge,
    },
    Console {
        id: "atari2600",
        titles: 700,
        layout: Layout::Cartridge,
    },
    Console {
        id: "arcade",
        titles: 2_500,
        layout: Layout::Mra,
    },
    Console {
        id: "megacd",
        titles: 450,
        layout: Layout::Disc(3),
    },
    Console {
        id: "s32x",
        titles: 80,
        layout: Layout::Cartridge,
    },
    Console {
        id: "fds",
        titles: 300,
        layout: Layout::Headered,
    },
    Console {
        id: "sg1000",
        titles: 300,
        layout: Layout::Cartridge,
    },
    Console {
        id: "atari5200",
        titles: 150,
        layout: Layout::Cartridge,
    },
    Console {
        id: "atari7800",
        titles: 150,
        layout: Layout::Headered,
    },
    Console {
        id: "lynx",
        titles: 150,
        layout: Layout::Headered,
    },
    Console {
        id: "coleco",
        titles: 250,
        layout: Layout::Cartridge,
    },
    Console {
        id: "intv",
        titles: 250,
        layout: Layout::Cartridge,
    },
    Console {
        id: "ws",
        titles: 150,
        layout: Layout::Cartridge,
    },
    Console {
        id: "wsc",
        titles: 150,
        layout: Layout::Cartridge,
    },
    Console {
        id: "ngp",
        titles: 100,
        layout: Layout::Cartridge,
    },
    Console {
        id: "vectrex",
        titles: 60,
        layout: Layout::Cartridge,
    },
    Console {
        id: "pcecd",
        titles: 600,
        layout: Layout::Disc(3),
    },
    Console {
        id: "neogeo",
        titles: 250,
        layout: Layout::Cartridge,
    },
    Console {
        id: "sgx",
        titles: 10,
        layout: Layout::Cartridge,
    },
];

/// A word in a few groups of every platform.
pub const RARE: &str = "Zyquor";

/// A word in many groups of every platform except [`BROWSED`], where it is in one.
pub const ELSEWHERE: &str = "Vexmir";

/// The large platforms the benchmarks browse.
pub const BROWSED: [&str; 3] = ["nes", "snes", "psx"];

/// Syllables of the generated words, most frequent first; no `q`, `x`, `y` or `z`, so
/// [`RARE`] and [`ELSEWHERE`] appear only where they are placed.
const SYLLABLES: [&str; 40] = [
    "sta", "man", "the", "ra", "lo", "ven", "ka", "dor", "mi", "tar", "el", "gon", "ri", "sol",
    "ber", "ni", "an", "to", "vel", "ga", "mor", "fen", "li", "bra", "cor", "du", "pa", "sen",
    "tho", "win", "ke", "ro", "hal", "mu", "den", "ple", "gri", "ot", "fa", "nu",
];

/// Region tags of the variants in a group, in order.
const REGIONS: [(&str, &str); 8] = [
    ("USA", "En"),
    ("Europe", "En,Fr,De"),
    ("Japan", "Ja"),
    ("World", "En"),
    ("Germany", "De"),
    ("France", "Fr"),
    ("Spain", "Es"),
    ("Korea", "Ko"),
];

/// What [`seed`] stored.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Seeded {
    /// Title rows.
    pub titles: u64,
    /// Rom rows.
    pub roms: u64,
    /// File rows.
    pub files: u64,
}

/// A deterministic xorshift generator.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        let n = u64::try_from(n.max(1)).unwrap_or(1);
        usize::try_from(self.next() % n).unwrap_or(0)
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }

    /// An index below `n`, Zipf-skewed so index 0 is the most frequent.
    fn zipf(&mut self, n: usize) -> usize {
        // Inverse of the continuous 1/x distribution over [1, n + 1).
        let u = f64::from(u32::try_from(self.next() >> 40).unwrap_or(0)) / f64::from(1_u32 << 24);
        #[allow(clippy::cast_precision_loss)]
        let x = ((n + 1) as f64).powf(u);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let i = x as usize;
        i.saturating_sub(1).min(n - 1)
    }

    fn hex(&mut self, digits: usize) -> String {
        let mut out = String::with_capacity(digits);
        while out.len() < digits {
            let _ = write!(out, "{:016x}", self.next());
        }
        out.truncate(digits);
        out
    }
}

/// A vocabulary of capitalised words built from Zipf-skewed syllables.
fn vocabulary(rng: &mut Rng, size: usize) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut words = Vec::with_capacity(size);
    while words.len() < size {
        let syllables = 1 + rng.below(3);
        let mut w: String = (0..syllables)
            .map(|_| SYLLABLES[rng.zipf(SYLLABLES.len())])
            .collect();
        if let Some(first) = w.get_mut(0..1) {
            first.make_ascii_uppercase();
        }
        if seen.insert(w.clone()) {
            words.push(w);
        }
    }
    words
}

/// A group's base name: one to four Zipf-chosen words, sometimes after "The".
fn base_name(rng: &mut Rng, words: &[String]) -> String {
    let count = 1 + rng.zipf(4);
    let mut parts: Vec<&str> = Vec::with_capacity(count + 1);
    if rng.chance(12) {
        parts.push("The");
    }
    for _ in 0..count {
        parts.push(&words[rng.zipf(words.len())]);
    }
    parts.join(" ")
}

/// `count` titles of `console` scaled by `scale`, at least one group's worth.
fn scaled(console: &Console, scale: f64) -> usize {
    #[allow(clippy::cast_precision_loss)]
    let n = console.titles as f64 * scale;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let n = n.round() as usize;
    n.max(3)
}

/// Writes the catalogue at `scale` (1.0 is full size) into a migrated database with its
/// platforms seeded, in one transaction through [`db::commit`], and returns the counts.
/// Clone groups average three variants; flags, revisions, files and verified states
/// follow DAT-like rates, and the same `seed` gives the same rows.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure, e.g. platforms not seeded.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// mistarr_server::db::platforms::seed(&mut conn, &mistarr_mister::platforms::PLATFORMS).unwrap();
/// let seeded = mistarr_server::synth::seed(&mut conn, 0.002, 1).unwrap();
/// assert!(seeded.titles > 0 && seeded.roms >= seeded.titles);
/// ```
pub fn seed(conn: &mut Connection, scale: f64, seed: u64) -> Result<Seeded> {
    let tx = conn.transaction()?;
    let mut rng = Rng(seed | 1);
    let words = vocabulary(&mut rng, 3_000);
    let mut out = Seeded::default();
    for console in &CONSOLES {
        seed_console(&tx, &mut rng, &words, console, scale, &mut out)?;
    }
    db::commit(tx)?;
    Ok(out)
}

/// `count` distinct DAT game names drawn like [`seed`]'s: clone groups of up to five
/// regions, with some revision and beta tags, for staging a DAT load.
///
/// ```
/// let names = mistarr_server::synth::game_names(30, 1);
/// assert_eq!(names.len(), 30);
/// assert!(names[0].ends_with("(USA)"));
/// ```
#[must_use]
pub fn game_names(count: usize, seed: u64) -> Vec<String> {
    let mut rng = Rng(seed | 1);
    let words = vocabulary(&mut rng, 3_000);
    let mut bases = HashSet::new();
    let mut out = Vec::with_capacity(count);
    while out.len() < count {
        let mut base = base_name(&mut rng, &words);
        let stem = base.clone();
        let mut n = 2;
        while !bases.insert(base.clone()) {
            base = format!("{stem} {n}");
            n += 1;
        }
        let variants = (1 + rng.below(5)).min(count - out.len());
        for (v, (region, _)) in REGIONS.iter().enumerate().take(variants) {
            let mut name = format!("{base} ({region})");
            if v > 0 && rng.chance(10) {
                name.push_str(" (Rev 1)");
            }
            if v > 0 && rng.chance(8) {
                name.push_str(" (Beta)");
            }
            out.push(name);
        }
    }
    out
}

#[allow(clippy::too_many_lines)] // One DAT's worth of rows, read top to bottom.
fn seed_console(
    c: &Connection,
    rng: &mut Rng,
    words: &[String],
    console: &Console,
    scale: f64,
    out: &mut Seeded,
) -> Result<()> {
    let mra = console.layout == Layout::Mra;
    let (dat_name, source) = if mra {
        ("_Arcade".to_owned(), "mra")
    } else {
        (format!("{} (synthetic)", console.id), "dat")
    };
    let total = scaled(console, scale);
    c.execute(
        "INSERT INTO dat_versions (platform_id, dat_name, version, source_file, loaded_at, game_count, source)
         VALUES (?1, ?2, '1', 'synthetic.dat', 0, ?3, ?4)",
        params![console.id, dat_name, i64::try_from(total).unwrap_or(0), source],
    )?;
    let version = c.last_insert_rowid();
    let mut title = c.prepare_cached(
        "INSERT INTO titles (platform_id, dat_version_id, name, base_name, parent_id, revision,
                             is_1g1r_pick, wanted, group_key, inferred, source, setname, mra_path)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
    )?;
    let mut parent_of = c.prepare_cached("UPDATE titles SET parent_id = id WHERE id = ?1")?;
    let mut tag =
        c.prepare_cached("INSERT INTO title_regions (title_id, pos, region) VALUES (?1, 0, ?2)")?;
    let mut language = c.prepare_cached(
        "INSERT INTO title_languages (title_id, pos, language) VALUES (?1, ?2, ?3)",
    )?;
    let mut rom = c.prepare_cached(
        "INSERT INTO roms (title_id, name, size, crc32, sha1, status, header, zip_dir, present)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    let mut file = c.prepare_cached(
        "INSERT INTO files (platform_id, rel_path, size, mtime, rom_id, state, scanned_at)
         VALUES (?1, ?2, ?3, 0, ?4, ?5, 0)",
    )?;
    let browsed = BROWSED.contains(&console.id);
    let mut names = HashSet::new();
    let mut made = 0;
    let mut group = 0;
    while made < total {
        let variants = (1 + rng.below(5)).min(total - made);
        let mut base = base_name(rng, words);
        if group % 1_000 == 7 {
            base.push(' ');
            base.push_str(RARE);
        }
        if (browsed && group == 11) || (!browsed && rng.chance(8)) {
            base.push(' ');
            base.push_str(ELSEWHERE);
        }
        let mut n = 2;
        let stem = base.clone();
        while !names.insert(base.clone()) {
            base = format!("{stem} {n}");
            n += 1;
        }
        let bios = rng.chance(1);
        let files_here = !mra && rng.chance(35);
        let setname = format!("s{group:05}");
        let mut parent = 0;
        for v in 0..variants {
            let (region, langs) = REGIONS[v % REGIONS.len()];
            let mut flags: Vec<String> = Vec::new();
            if bios {
                flags.push("bios".into());
            } else if v > 0 && rng.chance(8) {
                flags.push(["beta", "proto", "demo", "unl"][rng.below(4)].into());
            }
            let revision = (v > 0 && rng.chance(10)).then(|| "Rev 1".to_owned());
            let mut name = if bios {
                format!("[BIOS] {base}")
            } else {
                base.clone()
            };
            let _ = write!(name, " ({region})");
            if let Some(r) = &revision {
                let _ = write!(name, " ({r})");
            }
            for f in flags.iter().filter(|f| *f != "bios") {
                let _ = write!(name, " ({}{})", f[..1].to_uppercase(), &f[1..]);
            }
            title.execute(params![
                console.id,
                version,
                name,
                base,
                (v > 0).then_some(parent),
                revision,
                v == 0,
                rng.chance(1),
                format!("{}{}", if mra { "mra:" } else { "" }, base.to_lowercase()),
                mra,
                if mra { "mra" } else { "dat" },
                mra.then(|| format!("{setname}{v}")),
                mra.then(|| format!("{base} ({region}).mra")),
            ])?;
            let id = c.last_insert_rowid();
            if v == 0 {
                parent = id;
                parent_of.execute([id])?;
            }
            titles::set_flags(c, titles::TitleId(id), &flags)?;
            tag.execute(params![id, region])?;
            for (pos, l) in langs.split(',').enumerate() {
                language.execute(params![id, i64::try_from(pos).unwrap_or(0), l])?;
            }
            let rom_names: Vec<String> = match console.layout {
                Layout::Cartridge | Layout::Headered => vec![format!("{name}.bin")],
                Layout::Disc(max) => std::iter::once(format!("{name}.cue"))
                    .chain((1..=1 + rng.below(max)).map(|t| format!("{name} (Track {t}).bin")))
                    .collect(),
                Layout::Mra => vec![format!("{setname}{v}.zip")],
            };
            for r in rom_names {
                let size = if mra { 0 } else { 4_096 + rng.below(1 << 20) };
                rom.execute(params![
                    id,
                    r,
                    i64::try_from(size).unwrap_or(0),
                    (!mra).then(|| rng.hex(8)),
                    (!mra).then(|| rng.hex(40)),
                    if mra { "nodump" } else { "good" },
                    (console.layout == Layout::Headered).then(|| "0".repeat(32)),
                    mra.then_some("mame"),
                    mra && rng.chance(30),
                ])?;
                let rom_id = c.last_insert_rowid();
                out.roms += 1;
                if files_here && (v == 0 || rng.chance(20)) {
                    let state = match rng.below(20) {
                        0 => "misnamed",
                        1 => "bad",
                        _ => "verified",
                    };
                    file.execute(params![
                        console.id,
                        format!("{}/{r}", console.id),
                        i64::try_from(size).unwrap_or(0),
                        rom_id,
                        state
                    ])?;
                    out.files += 1;
                }
            }
            out.titles += 1;
        }
        if !mra && rng.chance(2) {
            file.execute(params![
                console.id,
                format!("{}/unmatched {group}.bin", console.id),
                1_024,
                Option::<i64>::None,
                "unverified"
            ])?;
            out.files += 1;
        }
        made += variants;
        group += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
