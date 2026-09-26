//! Pure helpers of the importer: locating, hashing, matching and reporting.

use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use mistarr_core::hash::{hash_forms, hash_zip_member_forms, zip_members, HashError, HeaderRule};
use mistarr_core::HashSet as Hashes;
use mistarr_mister::DatRom;

use crate::db::imports::EntryRom;

/// Bytes of a staged payload handed to the adapter as its head.
const HEAD_LEN: u64 = 16;

/// One hashed payload: a plain file, or one member of a zip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Hashed {
    /// The zip member name, `None` for a plain file.
    pub member: Option<String>,
    /// Uncompressed size on disk or in the archive.
    pub raw_size: u64,
    /// Hashes under the platform's header rule.
    pub hashes: Hashes,
    /// The whole payload's hashes when the rule stripped a header from it.
    pub whole: Option<Hashes>,
    /// The whole form of the file on disk is not known, as when the file gained a header
    /// on placement and could not be read again; `files` then stores no whole hashes.
    pub whole_unread: bool,
}

impl Hashed {
    /// A payload whose hashes are its whole content.
    #[cfg(test)]
    pub fn plain(member: Option<String>, hashes: Hashes) -> Self {
        Self {
            member,
            raw_size: hashes.size,
            hashes,
            whole: None,
            whole_unread: false,
        }
    }

    /// The forms the payload matches a rom in, the whole payload first.
    pub fn forms(&self) -> impl Iterator<Item = &Hashes> {
        self.whole.iter().chain([&self.hashes])
    }

    /// Whether the payload is `rom` in any of its forms.
    pub fn is(&self, rom: &EntryRom) -> bool {
        self.forms().any(|h| rom_matches(rom, h))
    }

    /// Takes the hashes of the placed file, `again`, after it gained a header; without them
    /// the whole form is unread.
    pub fn take_rehash(&mut self, again: Option<Hashed>) {
        if let Some(h) = again {
            self.hashes = h.hashes;
            self.whole = h.whole;
            self.whole_unread = false;
        } else {
            self.whole = None;
            self.whole_unread = true;
        }
    }

    /// What `files` stores beside the hashes of a payload hashed under the rule named `rule`:
    /// nothing when the whole form is unread, so a scan hashes the file again.
    pub fn whole_columns(&self, rule: &str) -> crate::db::files::WholeHashes {
        if self.whole_unread {
            return crate::db::files::WholeHashes::default();
        }
        let whole = self.whole.as_ref().unwrap_or(&self.hashes);
        crate::db::files::WholeHashes::whole_file(rule, whole)
    }
}

fn no_parent(path: &Path) -> bool {
    path.components()
        .all(|c| !matches!(c, Component::ParentDir))
}

/// The local path of a staged file. `staged` is the local path the poller
/// recorded; a relative one is taken inside `staging/<infohash>/`. When it is
/// missing, `torrent_path` is looked for there and one directory below, since a
/// magnet's display name may differ from the folder the client wrote. `None`
/// when the path is not inside staging.
pub(super) fn locate(
    staging: &Path,
    infohash: &str,
    staged: &str,
    torrent_path: Option<&str>,
) -> Option<PathBuf> {
    let root = staging.join(infohash);
    let path = Path::new(staged);
    let local = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    if !no_parent(&local) || !local.starts_with(staging) {
        return None;
    }
    let Some(inner) = torrent_path
        .map(Path::new)
        .filter(|p| p.is_relative() && no_parent(p))
    else {
        return Some(local);
    };
    if local.exists() {
        return Some(local);
    }
    let mut dirs: Vec<PathBuf> = fs::read_dir(&root)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default();
    dirs.sort();
    std::iter::once(root.clone())
        .chain(dirs)
        .map(|d| d.join(inner))
        .find(|p| p.is_file())
        .or(Some(local))
}

/// The header rule named in `docs/PLATFORMS.md` "Header rules".
pub(super) fn header_rule(name: &str) -> HeaderRule {
    match name {
        "ines" => HeaderRule::Ines,
        "smc" => HeaderRule::Smc,
        "a78" => HeaderRule::A78,
        "lnx" => HeaderRule::Lnx,
        "n64" => HeaderRule::N64,
        _ => HeaderRule::None,
    }
}

pub(super) fn is_zip(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
}

/// Hashes a staged file, or every member of a staged zip, under `rule`, in both forms
/// when a stripping rule finds a header.
pub(super) fn hash_item(path: &Path, rule: HeaderRule) -> Result<Vec<Hashed>, HashError> {
    if !is_zip(path) {
        let size = fs::metadata(path)?.len();
        let forms = hash_forms(File::open(path)?, rule, Some(size))?;
        return Ok(vec![Hashed {
            member: None,
            raw_size: size,
            hashes: forms.content,
            whole: forms.whole,
            whole_unread: false,
        }]);
    }
    let mut out = Vec::new();
    for m in zip_members(File::open(path)?)? {
        if !m.name.ends_with('/') {
            let forms = hash_zip_member_forms(File::open(path)?, &m.name, rule)?;
            out.push(Hashed {
                member: Some(m.name),
                raw_size: m.size,
                hashes: forms.content,
                whole: forms.whole,
                whole_unread: false,
            });
        }
    }
    Ok(out)
}

/// Up to 16 leading bytes of a file or of one zip member.
pub(super) fn read_head(path: &Path, member: Option<&str>) -> Result<Vec<u8>, HashError> {
    let mut buf = Vec::new();
    match member {
        None => {
            File::open(path)?.take(HEAD_LEN).read_to_end(&mut buf)?;
        }
        Some(name) => {
            let mut zip = zip::ZipArchive::new(File::open(path)?)?;
            zip.by_name(name)?.take(HEAD_LEN).read_to_end(&mut buf)?;
        }
    }
    Ok(buf)
}

/// Header bytes from a DAT `header` attribute written as hex, spaces allowed.
///
/// ```
/// use mistarr_server::jobs::import::parse_header;
/// assert_eq!(parse_header("4E 45 53 1a"), Some(b"NES\x1a".to_vec()));
/// assert_eq!(parse_header("no header"), None);
/// ```
#[must_use]
pub fn parse_header(text: &str) -> Option<Vec<u8>> {
    let hex: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if hex.is_empty() || !hex.len().is_multiple_of(2) {
        return None;
    }
    hex.chunks(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).ok()?, 16).ok())
        .collect()
}

pub(super) fn dat_rom(rom: &EntryRom) -> DatRom {
    DatRom {
        name: rom.name.clone(),
        size: rom.size,
        header: rom.header.as_deref().and_then(parse_header),
    }
}

/// Whether `h` is rom `rom` under `docs/VERIFICATION.md` "Matching order":
/// SHA1 when the DAT has it, else MD5, else CRC32 plus size.
pub(super) fn rom_matches(rom: &EntryRom, h: &Hashes) -> bool {
    if let Some(sha1) = &rom.sha1 {
        return *sha1 == h.sha1;
    }
    if let Some(md5) = &rom.md5 {
        return *md5 == h.md5;
    }
    rom.crc32.as_deref() == Some(h.crc32.as_str()) && rom.size == h.size
}

/// The last component of a `/`-separated name.
pub(super) fn leaf(name: &str) -> &str {
    name.rsplit(['/', '\\']).next().unwrap_or(name)
}

/// The rom of `roms` that `h` is in any form, skipping those in `used`. Among
/// identical roms it prefers `prefer`, then one whose name is `name`, then the first.
pub(super) fn pick_rom<'a>(
    roms: &'a [EntryRom],
    h: &Hashed,
    prefer: Option<i64>,
    name: Option<&str>,
    used: &[i64],
) -> Option<&'a EntryRom> {
    let hits: Vec<&EntryRom> = roms
        .iter()
        .filter(|r| !used.contains(&r.id) && h.is(r))
        .collect();
    hits.iter()
        .find(|r| Some(r.id) == prefer)
        .or_else(|| {
            hits.iter()
                .find(|r| name.is_some_and(|n| leaf(&r.name).eq_ignore_ascii_case(leaf(n))))
        })
        .or_else(|| hits.first())
        .copied()
}

/// How the members of a zip pair with the roms of a DAT entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SetMatch<'a> {
    /// Each member that is a rom of the entry, with that rom.
    pub pairs: Vec<(&'a Hashed, &'a EntryRom)>,
    /// Members that are no rom of the entry.
    pub extra: Vec<String>,
    /// Roms of the entry no member is.
    pub absent: Vec<String>,
}

impl SetMatch<'_> {
    /// Every member is a rom of the entry and every rom is a member.
    pub fn is_exact(&self) -> bool {
        self.extra.is_empty() && self.absent.is_empty()
    }
}

/// Pairs each member with a distinct rom of `roms` by hash, preferring the same name.
pub(super) fn match_members<'a>(roms: &'a [EntryRom], members: &'a [Hashed]) -> SetMatch<'a> {
    let mut used = Vec::with_capacity(members.len());
    let mut out = SetMatch {
        pairs: Vec::with_capacity(members.len()),
        extra: Vec::new(),
        absent: Vec::new(),
    };
    for m in members {
        match pick_rom(roms, m, None, m.member.as_deref(), &used) {
            Some(rom) => {
                used.push(rom.id);
                out.pairs.push((m, rom));
            }
            None => out.extra.push(m.member.clone().unwrap_or_default()),
        }
    }
    out.absent = roms
        .iter()
        .filter(|r| !used.contains(&r.id))
        .map(|r| r.name.clone())
        .collect();
    out
}

/// A quarantine report that opens with `why` and lists what arrived.
pub(super) fn explain(why: &str, actual: &[Hashed]) -> String {
    let mut out = format!("{why}\n\n");
    for a in actual {
        let label = a.member.as_deref().unwrap_or_default();
        let h = &a.hashes;
        let _ = writeln!(out, "Actual {label}: {} bytes", h.size);
        let _ = writeln!(out, "  crc32 {}  md5 {}  sha1 {}", h.crc32, h.md5, h.sha1);
        write_whole(&mut out, a);
    }
    out
}

/// The whole payload's line of a report, when a header was stripped from it.
fn write_whole(out: &mut String, a: &Hashed) {
    if let Some(w) = &a.whole {
        let (crc32, md5, sha1) = (&w.crc32, &w.md5, &w.sha1);
        let _ = writeln!(
            out,
            "  with its header, {} bytes: crc32 {crc32}  md5 {md5}  sha1 {sha1}",
            w.size
        );
    }
}

/// A library path as `files.rel_path` stores it.
pub(super) fn rel_string(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

pub(super) fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The quarantine report: the rom the transfer was chosen for, what arrived,
/// and the other entry it matches when it matches one.
pub(super) fn report(
    expected: Option<&EntryRom>,
    actual: &[Hashed],
    other: Option<&str>,
    rule: &str,
) -> String {
    let mut out = String::from(match other {
        Some(_) => "This file is not the wanted entry and was not placed.\n\n",
        None => "This file matches no entry in the loaded DATs and was not placed.\n\n",
    });
    match expected {
        Some(r) => {
            let _ = writeln!(out, "Expected: {} ({} bytes)", r.name, r.size);
            let dash = || "-".to_owned();
            let _ = writeln!(
                out,
                "  crc32 {}  md5 {}  sha1 {}",
                r.crc32.clone().unwrap_or_else(dash),
                r.md5.clone().unwrap_or_else(dash),
                r.sha1.clone().unwrap_or_else(dash)
            );
        }
        None => out.push_str("Expected: unknown rom\n"),
    }
    for a in actual {
        let label = a
            .member
            .as_deref()
            .map_or(String::new(), |m| format!(" {m}"));
        let h = &a.hashes;
        let _ = writeln!(out, "Actual{label}: {} bytes", h.size);
        let _ = writeln!(out, "  crc32 {}  md5 {}  sha1 {}", h.crc32, h.md5, h.sha1);
        write_whole(&mut out, a);
    }
    if let Some(other) = other {
        let _ = writeln!(out, "Matches instead: {other}");
    }
    let _ = writeln!(out, "Header rule: {rule}");
    out
}

/// Moves a staged item to `staging/quarantine/<infohash>/` with its report beside it.
pub(super) fn quarantine(
    staging: &Path,
    infohash: &str,
    item: &Path,
    report: &str,
) -> io::Result<PathBuf> {
    let dir = staging.join("quarantine").join(infohash);
    fs::create_dir_all(&dir)?;
    let name = file_name(item);
    let dst = dir.join(&name);
    fs::rename(item, &dst)?;
    fs::write(dir.join(format!("{name}.report.txt")), report)?;
    Ok(dst)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mistarr_core::hash::hash_reader;
    use std::io::Cursor;

    fn rom(id: i64, name: &str, h: &Hashes) -> EntryRom {
        EntryRom {
            id,
            name: name.into(),
            size: h.size,
            crc32: Some(h.crc32.clone()),
            md5: Some(h.md5.clone()),
            sha1: Some(h.sha1.clone()),
            status: "good".into(),
            header: None,
        }
    }

    fn abc() -> Hashes {
        hash_reader(Cursor::new(b"abc"), HeaderRule::None, None).expect("hash")
    }

    #[test]
    fn headers_parse_from_spaced_hex() {
        assert_eq!(parse_header("4e45531a"), Some(b"NES\x1a".to_vec()));
        assert_eq!(parse_header("4e4"), None);
        assert_eq!(parse_header(""), None);
        assert_eq!(parse_header("zz"), None);
    }

    #[test]
    fn roms_match_in_order_and_identical_ones_are_told_apart() {
        let h = abc();
        let a = rom(1, "Disc (Track 1).bin", &h);
        let b = rom(2, "Disc (Track 2).bin", &h);
        let roms = [a.clone(), b.clone()];
        let p = Hashed::plain(None, h.clone());
        assert_eq!(pick_rom(&roms, &p, None, None, &[]).map(|r| r.id), Some(1));
        assert_eq!(
            pick_rom(&roms, &p, Some(2), None, &[]).map(|r| r.id),
            Some(2)
        );
        assert_eq!(
            pick_rom(&roms, &p, None, Some("x/disc (track 2).bin"), &[]).map(|r| r.id),
            Some(2)
        );
        assert_eq!(
            pick_rom(&roms, &p, Some(1), None, &[1]).map(|r| r.id),
            Some(2)
        );
        assert!(pick_rom(&roms, &p, None, None, &[1, 2]).is_none());
        let crc_only = EntryRom {
            sha1: None,
            md5: None,
            ..a.clone()
        };
        assert!(rom_matches(&crc_only, &h));
        let wrong_size = EntryRom {
            size: 4,
            ..crc_only
        };
        assert!(!rom_matches(&wrong_size, &h));
        let md5_only = EntryRom { sha1: None, ..b };
        assert!(rom_matches(&md5_only, &h));
        let other = hash_reader(Cursor::new(b"abd"), HeaderRule::None, None).expect("hash");
        assert!(!rom_matches(&a, &other));
    }

    #[test]
    fn members_pair_with_distinct_roms_and_leftovers_are_named() {
        let h = abc();
        let other = hash_reader(Cursor::new(b"xyz"), HeaderRule::None, None).expect("hash");
        let roms = [rom(1, "a.bin", &h), rom(2, "b.bin", &h)];
        let member = |name: &str, hashes: &Hashes| Hashed::plain(Some(name.into()), hashes.clone());
        let both = [member("b.bin", &h), member("a.bin", &h)];
        let set = match_members(&roms, &both);
        assert!(set.is_exact());
        assert_eq!(
            set.pairs.iter().map(|(_, r)| r.id).collect::<Vec<_>>(),
            [2, 1]
        );
        let odd = [member("a.bin", &h), member("c.bin", &other)];
        let set = match_members(&roms, &odd);
        assert!(!set.is_exact());
        assert_eq!(set.extra, ["c.bin"]);
        assert_eq!(set.absent, ["b.bin"]);
        let text = explain("This zip lacks members.", &odd);
        assert!(text.starts_with("This zip lacks members.\n\nActual a.bin: 3 bytes\n"));
        assert!(text.contains(&other.sha1));
    }

    #[test]
    fn staged_paths_stay_in_staging_and_fall_back_to_the_torrent_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let staging = dir.path().join("staging");
        let real = staging.join("ab/Real Name/NES/x.nes");
        fs::create_dir_all(real.parent().expect("parent")).expect("mkdir");
        fs::write(&real, b"x").expect("write");
        let recorded = staging.join("ab/Magnet Name/NES/x.nes");
        let recorded = recorded.to_string_lossy();
        assert_eq!(
            locate(&staging, "ab", &recorded, Some("NES/x.nes")),
            Some(real.clone())
        );
        assert_eq!(
            locate(&staging, "ab", &recorded, None),
            Some(staging.join("ab/Magnet Name/NES/x.nes"))
        );
        assert_eq!(
            locate(&staging, "ab", "Real Name/NES/x.nes", Some("NES/x.nes")),
            Some(real)
        );
        assert_eq!(locate(&staging, "ab", "/elsewhere/x.nes", None), None);
        assert_eq!(locate(&staging, "ab", "../../x.nes", None), None);
    }

    #[test]
    fn report_names_expected_actual_and_other() {
        let h = abc();
        let expected = EntryRom {
            md5: None,
            sha1: None,
            crc32: Some("00000000".into()),
            ..rom(1, "Example Quest (USA).nes", &h)
        };
        let actual = [Hashed::plain(None, h.clone())];
        let text = report(Some(&expected), &actual, None, "ines");
        assert!(text.contains("Expected: Example Quest (USA).nes (3 bytes)"));
        assert!(text.contains(&h.sha1));
        assert!(text.contains("md5 -"));
        assert!(text.ends_with("Header rule: ines\n"));
        let member = [Hashed {
            member: Some("a.bin".into()),
            ..actual[0].clone()
        }];
        let other = report(None, &member, Some("Other Quest (USA)"), "none");
        assert!(other.contains("Actual a.bin: 3 bytes"));
        assert!(other.contains("Matches instead: Other Quest (USA)"));
        assert!(other.starts_with("This file is not the wanted entry"));
    }

    #[test]
    fn helpers_name_paths_and_rules() {
        assert_eq!(
            rel_string(Path::new("NES/Example Quest (USA).nes")),
            "NES/Example Quest (USA).nes"
        );
        assert!(is_zip(Path::new("a.ZIP")));
        assert!(!is_zip(Path::new("a.nes")));
        assert_eq!(header_rule("n64"), HeaderRule::N64);
        assert_eq!(header_rule("none"), HeaderRule::None);
        assert_eq!(leaf("a/b.bin"), "b.bin");
        let dat = dat_rom(&EntryRom {
            header: Some("4E 45".into()),
            ..rom(1, "a.nes", &abc())
        });
        assert_eq!(dat.header, Some(vec![0x4e, 0x45]));
    }

    #[test]
    fn a_placed_file_not_read_again_stores_no_whole_hashes() {
        let body = abc();
        let mut staged = Hashed::plain(None, body.clone());
        staged.take_rehash(None);
        assert_eq!(
            staged.whole_columns("ines"),
            crate::db::files::WholeHashes::default(),
            "left for a scan to hash again"
        );
        assert_eq!(staged.hashes, body, "the content form still stands");
        let mut placed = hash_reader(Cursor::new(b"NES\x1a"), HeaderRule::None, None).expect("h");
        placed.size = 19;
        let again = Hashed {
            whole: Some(placed.clone()),
            ..Hashed::plain(None, body)
        };
        staged.take_rehash(Some(again));
        assert_eq!(staged.whole_columns("ines").sha1, Some(placed.sha1));
    }

    #[test]
    fn a_headered_payload_is_the_rom_of_either_dat() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("a.nes");
        let mut data = b"NES\x1a".to_vec();
        data.resize(16, 0);
        data.extend_from_slice(b"synthetic body");
        fs::write(&path, &data).expect("write");
        let hashed = hash_item(&path, HeaderRule::Ines).expect("hash").remove(0);
        let whole = hash_reader(&data[..], HeaderRule::None, None).expect("hash");
        let body = hash_reader(&data[16..], HeaderRule::None, None).expect("hash");
        assert_eq!(
            (&hashed.hashes, hashed.whole.as_ref()),
            (&body, Some(&whole))
        );
        assert!(hashed.is(&rom(1, "a.nes", &whole)), "a headered DAT");
        assert!(hashed.is(&rom(2, "a.nes", &body)), "a headerless DAT");
        assert_eq!(hashed.forms().next(), Some(&whole), "the whole file first");
        assert_eq!(hashed.whole_columns("ines").sha1, Some(whole.sha1.clone()));
        assert_eq!(
            hashed.whole_columns("none"),
            crate::db::files::WholeHashes::default()
        );
        let plain = Hashed::plain(None, body.clone());
        assert_eq!(plain.whole_columns("ines").sha1, Some(body.sha1));
        let text = explain("Why.", std::slice::from_ref(&hashed));
        assert!(text.contains(&format!("with its header, 30 bytes: crc32 {}", whole.crc32)));
    }

    #[test]
    fn hash_item_and_read_head_cover_files_and_zips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let plain = dir.path().join("a.bin");
        fs::write(&plain, b"abcdefghijklmnopqrstuvwxyz").expect("write");
        let hashed = hash_item(&plain, HeaderRule::None).expect("hash");
        assert_eq!(hashed.len(), 1);
        assert_eq!((hashed[0].hashes.size, hashed[0].raw_size), (26, 26));
        assert_eq!(read_head(&plain, None).expect("head").len(), 16);
        let zipped = dir.path().join("a.zip");
        let mut z = zip::ZipWriter::new(File::create(&zipped).expect("create"));
        z.add_directory("d/", zip::write::SimpleFileOptions::default())
            .expect("dir");
        z.start_file("d/a.bin", zip::write::SimpleFileOptions::default())
            .expect("start");
        std::io::Write::write_all(&mut z, b"abc").expect("write");
        z.finish().expect("finish");
        let hashed = hash_item(&zipped, HeaderRule::None).expect("hash");
        assert_eq!(hashed.len(), 1);
        assert_eq!(hashed[0].member.as_deref(), Some("d/a.bin"));
        assert_eq!(read_head(&zipped, Some("d/a.bin")).expect("head"), b"abc");
        let staging = dir.path().join("staging");
        let q = quarantine(&staging, "0a0a", &plain, "report").expect("quarantine");
        assert_eq!(q, staging.join("quarantine/0a0a/a.bin"));
        assert_eq!(
            fs::read_to_string(staging.join("quarantine/0a0a/a.bin.report.txt")).expect("read"),
            "report"
        );
    }
}
