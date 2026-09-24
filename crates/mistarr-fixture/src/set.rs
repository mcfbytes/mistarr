//! The synthetic set of `docs/TESTING.md` layer 2: a cartridge set bound to
//! NES and a disc set bound to `PlayStation`, each with its DAT.

use std::fmt::Write as _;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use mistarr_core::hash::HeaderRule;

use crate::dat::{self, Game};
use crate::rng::SplitMix;
use crate::{io_at, Error, Result};

/// Platform id the cartridge DAT binds to.
pub const CART_PLATFORM: &str = "nes";
/// Platform id the disc DAT binds to.
pub const DISC_PLATFORM: &str = "psx";
/// Set name in the cartridge DAT header, and the fake platform's name.
pub const CART_DAT_NAME: &str = "Test Console";
/// Set name in the disc DAT header.
pub const DISC_DAT_NAME: &str = "Test Disc";
/// Directory, and torrent name, of the cartridge files.
pub const CART_SET: &str = "Test Console Set";
/// Directory, and torrent name, of the disc files.
pub const DISC_SET: &str = "Test Disc Set";

/// Parent of the explicit clone group.
pub const CLONE_PARENT: &str = "Example Quest (USA)";
/// The 1G1R pick of that group under default preferences: zipped and headerless.
pub const CLONE_PICK: &str = "Example Quest (USA) (Rev 1)";
/// An entry flagged `baddump` whose file in the set does not match the DAT.
pub const BAD_DUMP: &str = "Broken Tale (USA) [b]";
/// The BIOS entry, in the DAT only.
pub const BIOS: &str = "[BIOS] Fixture System (World)";
/// The beta, a clone in an inferred group.
pub const BETA: &str = "Sample Saga (USA) (Beta)";
/// The two-track disc game.
pub const DISC_GAME: &str = "Disc Example (USA)";

const KIB: usize = 1024;
const SIZES: [usize; 12] = [
    8 * KIB,
    16 * KIB,
    24 * KIB,
    32 * KIB,
    40 * KIB,
    64 * KIB,
    128 * KIB,
    256 * KIB,
    384 * KIB,
    512 * KIB,
    1024 * KIB,
    2048 * KIB,
];
const SECTOR: usize = 2352;
const ADJECTIVES: [&str; 8] = [
    "Fixture",
    "Synthetic",
    "Placeholder",
    "Dummy",
    "Stub",
    "Generic",
    "Plain",
    "Blank",
];
const NOUNS: [&str; 9] = [
    "Trail", "Voyage", "Puzzle", "Circuit", "Harbor", "Garden", "Tower", "Relay", "Canyon",
];
const REGIONS: [&str; 4] = ["USA", "Europe", "Japan", "World"];
const FILLERS: usize = 36;

/// How an entry's file appears in the set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Form {
    /// Headerless `.nes`; the DAT's `header` attribute supplies the header.
    Plain,
    /// `.nes` with an iNES header.
    Headered,
    /// Headerless `.nes` inside a `.zip` of the same stem.
    Zipped,
}

/// One cartridge entry of the set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// DAT entry name.
    pub name: String,
    /// Parent entry, when the DAT says so.
    pub cloneof: Option<String>,
    /// Shape of its file in the set.
    pub form: Form,
    /// Rom size without header.
    pub size: usize,
    /// Flagged `baddump`, with a file in the set that does not match.
    pub bad_dump: bool,
    /// A BIOS entry, listed in the DAT with no file in the set.
    pub bios: bool,
}

impl Entry {
    fn new(name: &str, form: Form, size: usize) -> Self {
        Self {
            name: name.to_owned(),
            cloneof: None,
            form,
            size,
            bad_dump: false,
            bios: false,
        }
    }

    fn clone_of(mut self, parent: &str) -> Self {
        self.cloneof = Some(parent.to_owned());
        self
    }

    /// The file name of this entry inside the cartridge set.
    #[must_use]
    pub fn file_name(&self) -> String {
        match self.form {
            Form::Zipped => format!("{}.zip", self.name),
            Form::Plain | Form::Headered => format!("{}.nes", self.name),
        }
    }
}

/// Every cartridge entry, in DAT order.
///
/// ```
/// let entries = mistarr_fixture::set::cart_entries();
/// assert!(entries.iter().any(|e| e.name == mistarr_fixture::set::CLONE_PICK));
/// ```
#[must_use]
pub fn cart_entries() -> Vec<Entry> {
    let mut v = vec![
        Entry::new(CLONE_PARENT, Form::Headered, 32 * KIB),
        Entry::new("Example Quest (Europe)", Form::Zipped, 32 * KIB).clone_of(CLONE_PARENT),
        Entry::new("Example Quest (Japan)", Form::Plain, 24 * KIB).clone_of(CLONE_PARENT),
        Entry::new(CLONE_PICK, Form::Zipped, 40 * KIB).clone_of(CLONE_PARENT),
        Entry::new("Sample Saga (USA)", Form::Plain, 64 * KIB),
        Entry::new("Sample Saga (Japan)", Form::Headered, 64 * KIB),
        Entry::new(BETA, Form::Plain, 48 * KIB),
        Entry::new("Mock Rally (Europe)", Form::Headered, 128 * KIB),
        Entry::new("Mock Rally (USA) (Rev 1)", Form::Zipped, 128 * KIB),
        Entry::new("Mock Rally (USA) (Rev 2)", Form::Headered, 128 * KIB),
        Entry {
            bad_dump: true,
            ..Entry::new(BAD_DUMP, Form::Plain, 24 * KIB)
        },
        Entry {
            bios: true,
            ..Entry::new(BIOS, Form::Plain, 8 * KIB)
        },
    ];
    for i in 0..FILLERS {
        let name = format!(
            "{} {} ({})",
            ADJECTIVES[i % ADJECTIVES.len()],
            NOUNS[i % NOUNS.len()],
            REGIONS[i % REGIONS.len()]
        );
        let form = match i % 5 {
            1 => Form::Zipped,
            0 | 3 => Form::Headered,
            _ => Form::Plain,
        };
        let size = SIZES[usize::try_from(SplitMix::from_label(&name).next_u64() % 12).unwrap_or(0)];
        v.push(Entry::new(&name, form, size));
    }
    v
}

/// The 16-byte iNES header of a synthetic NROM-style image of `size` bytes.
///
/// ```
/// assert_eq!(&mistarr_fixture::set::ines_header(32 * 1024)[..5], b"NES\x1a\x02");
/// ```
#[must_use]
pub fn ines_header(size: usize) -> [u8; 16] {
    let mut h = [0u8; 16];
    h[..4].copy_from_slice(b"NES\x1a");
    h[4] = u8::try_from((size / (16 * KIB)).clamp(1, 255)).unwrap_or(1);
    h
}

/// The headerless rom bytes of `entry`.
#[must_use]
pub fn rom_bytes(entry: &Entry) -> Vec<u8> {
    SplitMix::from_label(&entry.name).bytes(entry.size)
}

/// Where [`generate`] put things.
#[derive(Debug, Clone)]
pub struct Layout {
    /// The output directory.
    pub root: PathBuf,
    /// The cartridge files, a directory named [`CART_SET`].
    pub cart_dir: PathBuf,
    /// The disc files, a directory named [`DISC_SET`].
    pub disc_dir: PathBuf,
    /// The cartridge DAT.
    pub cart_dat: PathBuf,
    /// The disc DAT.
    pub disc_dat: PathBuf,
    /// The cartridge entries.
    pub entries: Vec<Entry>,
    /// Entries of both DATs.
    pub games: Vec<Game>,
}

fn write(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io_at(parent))?;
    }
    std::fs::write(path, data).map_err(io_at(path))
}

fn zipped(path: &Path, member: &str, data: &[u8]) -> Result<Vec<u8>> {
    let zip_err = |source| Error::Zip {
        path: path.to_path_buf(),
        source,
    };
    let mut buf = Vec::new();
    let mut z = zip::ZipWriter::new(Cursor::new(&mut buf));
    let opts =
        zip::write::SimpleFileOptions::default().last_modified_time(zip::DateTime::default());
    z.start_file(member, opts).map_err(zip_err)?;
    z.write_all(data).map_err(io_at(path))?;
    z.finish().map_err(zip_err)?;
    Ok(buf)
}

fn cart_game(entry: &Entry, dir: &Path) -> Result<Game> {
    let rom_name = format!("{}.nes", entry.name);
    let body = rom_bytes(entry);
    let header = ines_header(entry.size);
    let mut rom = dat::rom_from_bytes(&rom_name, &body, HeaderRule::Ines)?;
    rom.header = Some(header.to_vec());
    if entry.bad_dump {
        rom.status = Some("baddump".to_owned());
    }
    if !entry.bios {
        let mut file = match entry.form {
            Form::Headered => [&header[..], &body].concat(),
            Form::Plain | Form::Zipped => body,
        };
        if entry.bad_dump {
            file.iter_mut().take(64).for_each(|b| *b = !*b);
        }
        let path = dir.join(entry.file_name());
        if entry.form == Form::Zipped {
            file = zipped(&path, &rom_name, &file)?;
        }
        write(&path, &file)?;
    }
    Ok(Game {
        name: entry.name.clone(),
        cloneof: entry.cloneof.clone(),
        roms: vec![rom],
    })
}

fn disc_game(name: &str, tracks: &[usize], dir: &Path) -> Result<Game> {
    let mut cue = String::new();
    let mut roms = Vec::new();
    for (i, sectors) in tracks.iter().enumerate() {
        let n = i + 1;
        let file = format!("{name} (Track {n}).bin");
        let data = SplitMix::from_label(&file).bytes(sectors * SECTOR);
        let kind = if n == 1 { "MODE2/2352" } else { "AUDIO" };
        let _ = write!(
            cue,
            "FILE \"{file}\" BINARY\n  TRACK {n:02} {kind}\n    INDEX 01 00:00:00\n"
        );
        write(&dir.join(name).join(&file), &data)?;
        roms.push(dat::rom_from_bytes(&file, &data, HeaderRule::None)?);
    }
    let cue_name = format!("{name}.cue");
    write(&dir.join(name).join(&cue_name), cue.as_bytes())?;
    roms.insert(
        0,
        dat::rom_from_bytes(&cue_name, cue.as_bytes(), HeaderRule::None)?,
    );
    Ok(Game {
        name: name.to_owned(),
        cloneof: None,
        roms,
    })
}

/// Writes the set under `out`: `roms/<CART_SET>/`, `roms/<DISC_SET>/` and
/// `dats/test-console.dat`, `dats/test-disc.dat`. Output is identical on every run.
///
/// # Errors
///
/// [`Error::Io`] or [`Error::Zip`] when a file cannot be written.
pub fn generate(out: &Path) -> Result<Layout> {
    let cart_dir = out.join("roms").join(CART_SET);
    let disc_dir = out.join("roms").join(DISC_SET);
    let entries = cart_entries();
    let cart: Vec<Game> = entries
        .iter()
        .map(|e| cart_game(e, &cart_dir))
        .collect::<Result<_>>()?;
    let disc = vec![
        disc_game(DISC_GAME, &[300, 150], &disc_dir)?,
        disc_game("Disc Sample (Europe)", &[120], &disc_dir)?,
    ];
    let cart_dat = out.join("dats").join("test-console.dat");
    let disc_dat = out.join("dats").join("test-disc.dat");
    let nes = dat::platform(CART_PLATFORM)?;
    let psx = dat::platform(DISC_PLATFORM)?;
    write(
        &cart_dat,
        dat::to_xml(&dat::header_name(nes, CART_DAT_NAME), "1", &cart).as_bytes(),
    )?;
    write(
        &disc_dat,
        dat::to_xml(&dat::header_name(psx, DISC_DAT_NAME), "1", &disc).as_bytes(),
    )?;
    Ok(Layout {
        root: out.to_path_buf(),
        cart_dir,
        disc_dir,
        cart_dat,
        disc_dat,
        entries,
        games: cart.into_iter().chain(disc).collect(),
    })
}

#[cfg(test)]
mod tests {
    use mistarr_core::dat::parse_dat;
    use mistarr_core::hash::hash_reader;

    use super::*;

    #[test]
    fn entry_table_has_the_documented_shape() {
        let e = cart_entries();
        assert_eq!(e.len(), 48);
        let names: std::collections::HashSet<_> = e.iter().map(|x| &x.name).collect();
        assert_eq!(names.len(), e.len(), "names are unique");
        assert!(e.iter().all(|x| (8 * KIB..=2048 * KIB).contains(&x.size)));
        assert!(e.iter().any(|x| x.form == Form::Zipped));
        assert!(e.iter().any(|x| x.form == Form::Headered));
        assert_eq!(e.iter().filter(|x| x.bad_dump).count(), 1);
        assert_eq!(e.iter().filter(|x| x.bios).count(), 1);
        let pick = e.iter().find(|x| x.name == CLONE_PICK).unwrap();
        assert_eq!(pick.form, Form::Zipped);
        assert_eq!(pick.file_name(), "Example Quest (USA) (Rev 1).zip");
    }

    #[test]
    fn ines_header_counts_prg_banks() {
        assert_eq!(ines_header(8 * KIB)[4], 1);
        assert_eq!(ines_header(64 * KIB)[4], 4);
        assert_eq!(ines_header(8 * 1024 * KIB)[4], 255);
    }

    #[test]
    fn rom_bytes_are_deterministic() {
        let e = &cart_entries()[0];
        assert_eq!(rom_bytes(e), rom_bytes(e));
        assert_eq!(rom_bytes(e).len(), e.size);
    }

    #[test]
    fn generated_set_matches_its_dats() {
        let out = tempfile::tempdir().unwrap();
        let layout = generate(out.path()).unwrap();
        let cart = parse_dat(&std::fs::read(&layout.cart_dat).unwrap()).unwrap();
        let disc = parse_dat(&std::fs::read(&layout.disc_dat).unwrap()).unwrap();
        assert_eq!(cart.games.len() + disc.games.len(), 50);
        let bound = |n: &str| mistarr_mister::bind_dat_name(n).map(|p| p.id);
        assert_eq!(bound(&cart.header.name), Some(CART_PLATFORM));
        assert_eq!(bound(&disc.header.name), Some(DISC_PLATFORM));

        let quest = cart.games.iter().find(|g| g.name == CLONE_PARENT).unwrap();
        let file = std::fs::read(layout.cart_dir.join("Example Quest (USA).nes")).unwrap();
        let h = hash_reader(&file[..], HeaderRule::Ines, None).unwrap();
        assert_eq!(quest.roms[0].sha1.as_deref(), Some(h.sha1.as_str()));
        assert!(quest.roms[0].header.is_some());

        let bad = cart.games.iter().find(|g| g.name == BAD_DUMP).unwrap();
        let file = std::fs::read(layout.cart_dir.join(format!("{BAD_DUMP}.nes"))).unwrap();
        let h = hash_reader(&file[..], HeaderRule::Ines, None).unwrap();
        assert_ne!(bad.roms[0].sha1.as_deref(), Some(h.sha1.as_str()));
        assert_eq!(bad.roms[0].status.as_str(), "baddump");

        assert!(!layout.cart_dir.join(format!("{BIOS}.nes")).exists());
        let disc_game = disc.games.iter().find(|g| g.name == DISC_GAME).unwrap();
        assert_eq!(disc_game.roms.len(), 3);
        assert!(layout
            .disc_dir
            .join(DISC_GAME)
            .join("Disc Example (USA) (Track 2).bin")
            .is_file());
    }

    #[test]
    fn generation_is_reproducible() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let la = generate(a.path()).unwrap();
        let lb = generate(b.path()).unwrap();
        let tracker = "http://tracker.invalid/announce";
        assert_eq!(
            crate::torrent::build(&la.cart_dir, tracker, None).unwrap(),
            crate::torrent::build(&lb.cart_dir, tracker, None).unwrap()
        );
        assert_eq!(
            std::fs::read(la.cart_dat).unwrap(),
            std::fs::read(lb.cart_dat).unwrap()
        );
    }
}
