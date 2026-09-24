//! Logiqx DATs built from files on disk, hashed the way mistarr's scanner
//! hashes them. Format: `docs/VERIFICATION.md` "DAT parsing".

use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use mistarr_core::hash::{hash_reader, hash_zip_member, zip_members, HashError, HeaderRule};
use mistarr_core::HashSet;
use mistarr_mister::platforms::{self, Platform};
use mistarr_mister::Kind;

use crate::{io_at, slash_path, walk, Error, Result};

/// One `<rom>` of a game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rom {
    /// File name the DAT gives the rom.
    pub name: String,
    /// Size and hashes, after the platform's header rule.
    pub hashes: HashSet,
    /// `status` attribute, such as `baddump`; `None` means good.
    pub status: Option<String>,
    /// Header bytes the header rule skipped, written as the `header` attribute.
    pub header: Option<Vec<u8>>,
}

/// One `<game>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Game {
    /// Entry name.
    pub name: String,
    /// Parent entry name, when this is a clone.
    pub cloneof: Option<String>,
    /// The entry's roms, in order.
    pub roms: Vec<Rom>,
}

/// Looks up a row of the platform table by id.
///
/// # Errors
///
/// [`Error::UnknownPlatform`] when no row has `id`.
pub fn platform(id: &str) -> Result<&'static Platform> {
    platforms::by_id(id).ok_or_else(|| Error::UnknownPlatform(id.to_owned()))
}

/// The DAT header name for a set called `name` on `platform`, which binds to
/// that platform by the rules of `docs/PLATFORMS.md`.
///
/// ```
/// let nes = mistarr_fixture::dat::platform("nes").unwrap();
/// let name = mistarr_fixture::dat::header_name(nes, "Test Console");
/// assert_eq!(mistarr_mister::bind_dat_name(&name).map(|p| p.id), Some("nes"));
/// ```
#[must_use]
pub fn header_name(platform: &Platform, name: &str) -> String {
    format!("{name} - {}", platform.name)
}

/// The hashing rule named in the platform table's `header_rule` column.
#[must_use]
pub fn header_rule(name: &str) -> HeaderRule {
    match name {
        "ines" => HeaderRule::Ines,
        "smc" => HeaderRule::Smc,
        "a78" => HeaderRule::A78,
        "lnx" => HeaderRule::Lnx,
        "n64" => HeaderRule::N64,
        _ => HeaderRule::None,
    }
}

/// Rules whose skipped bytes a headered DAT records in the `header` attribute.
fn records_header(rule: HeaderRule) -> bool {
    matches!(rule, HeaderRule::Ines | HeaderRule::A78 | HeaderRule::Lnx)
}

/// Hashes `data` with `rule` into a [`Rom`] named `name`.
///
/// # Errors
///
/// Never in practice; reading from memory cannot fail.
pub fn rom_from_bytes(name: &str, data: &[u8], rule: HeaderRule) -> Result<Rom> {
    let hashes = hash_reader(data, rule, None).map_err(io_at(Path::new(name)))?;
    Ok(rom(
        name,
        hashes,
        data,
        u64::try_from(data.len()).unwrap_or(u64::MAX),
        rule,
    ))
}

/// A rom whose header, if the rule skipped one, is the first `total - size`
/// bytes of `head`, the start of an item `total` bytes long.
fn rom(name: &str, hashes: HashSet, head: &[u8], total: u64, rule: HeaderRule) -> Rom {
    let skipped = usize::try_from(total.saturating_sub(hashes.size)).unwrap_or(usize::MAX);
    let header = (records_header(rule) && skipped > 0 && skipped <= head.len())
        .then(|| head[..skipped].to_vec());
    Rom {
        name: name.to_owned(),
        hashes,
        status: None,
        header,
    }
}

fn rom_from_file(path: &Path, name: &str, rule: HeaderRule) -> Result<Rom> {
    let file = File::open(path).map_err(io_at(path))?;
    let hashes = hash_reader(BufReader::new(file), rule, None).map_err(io_at(path))?;
    let total = std::fs::metadata(path).map_err(io_at(path))?.len();
    Ok(rom(name, hashes, &read_head(path)?, total, rule))
}

fn read_head(path: &Path) -> Result<Vec<u8>> {
    let mut head = Vec::new();
    File::open(path)
        .and_then(|f| f.take(512).read_to_end(&mut head))
        .map_err(io_at(path))?;
    Ok(head)
}

fn hash_err(path: &Path) -> impl Fn(HashError) -> Error + '_ {
    move |e| match e {
        HashError::Io(source) => Error::Io {
            path: path.to_path_buf(),
            source,
        },
        HashError::Zip(source) => Error::Zip {
            path: path.to_path_buf(),
            source,
        },
    }
}

fn roms_from_zip(path: &Path, rule: HeaderRule) -> Result<Vec<Rom>> {
    let zip_err = |source| Error::Zip {
        path: path.to_path_buf(),
        source,
    };
    let open = || File::open(path).map(BufReader::new).map_err(io_at(path));
    let members = zip_members(open()?).map_err(hash_err(path))?;
    let mut archive = zip::ZipArchive::new(open()?).map_err(zip_err)?;
    let mut out = Vec::new();
    for m in members.iter().filter(|m| !m.name.ends_with('/')) {
        let hashes = hash_zip_member(open()?, &m.name, rule).map_err(hash_err(path))?;
        let mut head = Vec::new();
        archive
            .by_name(&m.name)
            .map_err(zip_err)?
            .take(512)
            .read_to_end(&mut head)
            .map_err(io_at(path))?;
        out.push(rom(&m.name, hashes, &head, m.size, rule));
    }
    Ok(out)
}

fn stem(rel: &Path) -> Result<String> {
    rel.file_stem()
        .and_then(|s| s.to_str())
        .map(str::to_owned)
        .ok_or_else(|| Error::BadName(rel.to_path_buf()))
}

fn leaf(rel: &Path) -> Result<String> {
    rel.file_name()
        .and_then(|s| s.to_str())
        .map(str::to_owned)
        .ok_or_else(|| Error::BadName(rel.to_path_buf()))
}

fn is_zip(rel: &Path) -> bool {
    rel.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
}

/// Describes every file under `dir` as games of `platform`. A zip is one game
/// of its members; on a disc platform each top-level directory is one game of
/// the files in it; any other file is a game of one rom named after it.
///
/// # Errors
///
/// [`Error::Io`] or [`Error::Zip`] when a file cannot be read,
/// [`Error::Empty`] when there is nothing to describe.
pub fn games_from_dir(dir: &Path, platform: &Platform) -> Result<Vec<Game>> {
    let rule = header_rule(platform.header_rule);
    let mut games: Vec<Game> = Vec::new();
    for rel in walk(dir)? {
        let abs = dir.join(&rel);
        let mut parts = rel.components();
        let top = parts.next().map(|c| c.as_os_str().to_string_lossy());
        let nested = parts.next().is_some();
        if platform.kind == Kind::Disc && nested {
            let name = top
                .map(String::from)
                .ok_or_else(|| Error::BadName(rel.clone()))?;
            let inner = rel.strip_prefix(&name).unwrap_or(&rel);
            let rom = rom_from_file(&abs, &slash_path(inner), rule)?;
            match games.iter_mut().find(|g| g.name == name) {
                Some(g) => g.roms.push(rom),
                None => games.push(Game {
                    name,
                    cloneof: None,
                    roms: vec![rom],
                }),
            }
        } else if is_zip(&rel) {
            games.push(Game {
                name: stem(&rel)?,
                cloneof: None,
                roms: roms_from_zip(&abs, rule)?,
            });
        } else {
            games.push(Game {
                name: stem(&rel)?,
                cloneof: None,
                roms: vec![rom_from_file(&abs, &leaf(&rel)?, rule)?],
            });
        }
    }
    if games.is_empty() {
        return Err(Error::Empty(dir.to_path_buf()));
    }
    games.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(games)
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// Renders a Logiqx DAT with `name` as both header name and description.
///
/// ```
/// let xml = mistarr_fixture::dat::to_xml("Test - NES", "1", &[]);
/// assert!(xml.contains("<name>Test - NES</name>"));
/// ```
#[must_use]
pub fn to_xml(name: &str, version: &str, games: &[Game]) -> String {
    let mut x = String::from("<?xml version=\"1.0\"?>\n<datafile>\n\t<header>\n");
    let _ = writeln!(x, "\t\t<name>{}</name>", escape(name));
    let _ = writeln!(x, "\t\t<description>{}</description>", escape(name));
    let _ = writeln!(x, "\t\t<version>{}</version>", escape(version));
    x.push_str("\t</header>\n");
    for g in games {
        let _ = write!(x, "\t<game name=\"{}\"", escape(&g.name));
        if let Some(parent) = &g.cloneof {
            let _ = write!(x, " cloneof=\"{}\"", escape(parent));
        }
        x.push_str(">\n");
        let _ = writeln!(x, "\t\t<description>{}</description>", escape(&g.name));
        for r in &g.roms {
            let h = &r.hashes;
            let _ = write!(
                x,
                "\t\t<rom name=\"{}\" size=\"{}\" crc=\"{}\" md5=\"{}\" sha1=\"{}\"",
                escape(&r.name),
                h.size,
                h.crc32,
                h.md5,
                h.sha1
            );
            if let Some(status) = &r.status {
                let _ = write!(x, " status=\"{}\"", escape(status));
            }
            if let Some(header) = &r.header {
                let hex: Vec<String> = header.iter().map(|b| format!("{b:02X}")).collect();
                let _ = write!(x, " header=\"{}\"", hex.join(" "));
            }
            x.push_str("/>\n");
        }
        x.push_str("\t</game>\n");
    }
    x.push_str("</datafile>\n");
    x
}

/// Builds the DAT for the files under `dir`, as `mistarr-fixture dat` prints it.
///
/// # Errors
///
/// [`Error::UnknownPlatform`] for an unknown id, otherwise as [`games_from_dir`].
pub fn build(dir: &Path, platform_id: &str, name: &str, version: &str) -> Result<String> {
    let row = platform(platform_id)?;
    let games = games_from_dir(dir, row)?;
    Ok(to_xml(&header_name(row, name), version, &games))
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use super::*;

    fn ines(body: &[u8]) -> Vec<u8> {
        let mut v = b"NES\x1a\x02\x01".to_vec();
        v.resize(16, 0);
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn every_platform_binds_back_from_its_header_name() {
        for row in &platforms::PLATFORMS {
            let name = header_name(row, "Test Console");
            let bound = mistarr_mister::bind_dat_name(&name).map(|p| p.id);
            assert_eq!(bound, Some(row.id), "{name}");
        }
    }

    #[test]
    fn unknown_platform_is_an_error() {
        assert!(matches!(platform("nope"), Err(Error::UnknownPlatform(_))));
        assert_eq!(platform("psx").unwrap().id, "psx");
    }

    #[test]
    fn header_rules_follow_the_table_names() {
        assert_eq!(header_rule("ines"), HeaderRule::Ines);
        assert_eq!(header_rule("smc"), HeaderRule::Smc);
        assert_eq!(header_rule("a78"), HeaderRule::A78);
        assert_eq!(header_rule("lnx"), HeaderRule::Lnx);
        assert_eq!(header_rule("n64"), HeaderRule::N64);
        assert_eq!(header_rule("none"), HeaderRule::None);
    }

    #[test]
    fn headered_rom_hashes_the_body_and_records_the_header() {
        let rom = rom_from_bytes("a.nes", &ines(b"body"), HeaderRule::Ines).unwrap();
        let plain = rom_from_bytes("a.nes", b"body", HeaderRule::Ines).unwrap();
        assert_eq!(rom.hashes, plain.hashes);
        assert_eq!(rom.hashes.size, 4);
        assert_eq!(rom.header.as_deref(), Some(&ines(b"")[..]));
        assert_eq!(plain.header, None);
    }

    #[test]
    fn a_large_headered_file_records_its_header() {
        let dir = tempfile::tempdir().unwrap();
        let body: Vec<u8> = (0..32 * 1024u32)
            .map(|i| u8::try_from(i % 251).unwrap())
            .collect();
        std::fs::write(dir.path().join("Large Example (USA).nes"), ines(&body)).unwrap();
        let mut buf = Vec::new();
        let mut z = zip::ZipWriter::new(Cursor::new(&mut buf));
        z.start_file(
            "Large Zip (USA).nes",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        z.write_all(&ines(&body)).unwrap();
        z.finish().unwrap();
        std::fs::write(dir.path().join("Large Zip (USA).zip"), &buf).unwrap();
        let games = games_from_dir(dir.path(), platform("nes").unwrap()).unwrap();
        for g in &games {
            let rom = &g.roms[0];
            assert_eq!(rom.hashes.size, 32 * 1024, "{}", g.name);
            assert_eq!(rom.header.as_deref(), Some(&ines(b"")[..]), "{}", g.name);
        }
    }

    #[test]
    fn a_directory_becomes_games() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Example Quest (USA).nes"), ines(b"q")).unwrap();
        let mut buf = Vec::new();
        let mut z = zip::ZipWriter::new(Cursor::new(&mut buf));
        z.start_file(
            "Sample Saga (USA).nes",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        z.write_all(b"saga").unwrap();
        z.finish().unwrap();
        std::fs::write(dir.path().join("Sample Saga (USA).zip"), &buf).unwrap();
        let games = games_from_dir(dir.path(), platform("nes").unwrap()).unwrap();
        assert_eq!(games.len(), 2);
        assert_eq!(games[0].name, "Example Quest (USA)");
        assert_eq!(games[0].roms[0].name, "Example Quest (USA).nes");
        assert_eq!(games[0].roms[0].hashes.size, 1);
        assert_eq!(games[1].roms[0].name, "Sample Saga (USA).nes");
        assert_eq!(games[1].roms[0].hashes.size, 4);
    }

    #[test]
    fn disc_directories_are_one_game_each() {
        let dir = tempfile::tempdir().unwrap();
        let game = dir.path().join("Disc Example (USA)");
        std::fs::create_dir_all(&game).unwrap();
        std::fs::write(game.join("Disc Example (USA).cue"), b"cue").unwrap();
        std::fs::write(game.join("Disc Example (USA) (Track 1).bin"), b"t1").unwrap();
        let games = games_from_dir(dir.path(), platform("psx").unwrap()).unwrap();
        assert_eq!(games.len(), 1);
        let names: Vec<_> = games[0].roms.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            ["Disc Example (USA) (Track 1).bin", "Disc Example (USA).cue"]
        );
    }

    #[test]
    fn empty_directory_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = games_from_dir(dir.path(), platform("nes").unwrap()).unwrap_err();
        assert!(matches!(err, Error::Empty(_)));
    }

    #[test]
    fn built_dat_parses_and_binds() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Fixture & Friends (USA).nes"), ines(b"x")).unwrap();
        let xml = build(dir.path(), "nes", "Homebrew Test", "2").unwrap();
        let dat = mistarr_core::dat::parse_dat(xml.as_bytes()).unwrap();
        assert_eq!(
            dat.header.name,
            "Homebrew Test - Nintendo Entertainment System"
        );
        assert_eq!(dat.header.version, "2");
        assert_eq!(dat.games[0].name, "Fixture & Friends (USA)");
        let rom = &dat.games[0].roms[0];
        assert_eq!(rom.size, 1);
        assert_eq!(
            rom.header.as_deref(),
            Some("4E 45 53 1A 02 01 00 00 00 00 00 00 00 00 00 00")
        );
    }

    #[test]
    fn xml_carries_clone_and_status() {
        let mut rom = rom_from_bytes("b.nes", b"b", HeaderRule::None).unwrap();
        rom.status = Some("baddump".into());
        let games = [Game {
            name: "B (Japan)".into(),
            cloneof: Some("B (USA)".into()),
            roms: vec![rom],
        }];
        let xml = to_xml("T", "1", &games);
        let dat = mistarr_core::dat::parse_dat(xml.as_bytes()).unwrap();
        assert_eq!(dat.games[0].clone_of.as_deref(), Some("B (USA)"));
        assert_eq!(dat.games[0].roms[0].status.as_str(), "baddump");
    }

    #[test]
    fn escape_covers_xml_specials() {
        assert_eq!(escape(r#"a&b<c>"'"#), "a&amp;b&lt;c&gt;&quot;&apos;");
    }
}
