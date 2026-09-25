//! What a fetched file is, from its first bytes and then its whole content.

use std::io::{BufReader, Read, Write};
use std::path::Path;

use mistarr_core::dat::DatStream;

use crate::jobs::dat_import::MAX_DAT_BYTES;
use crate::jobs::source_import::MAX_SOURCE_BYTES;
use crate::jobs::url_fetch::spool::{Pace, Paced};

/// Bytes read before the type is decided, unless the file is shorter.
pub const SNIFF_BYTES: usize = 64;

/// Buffer of the rebuilt pack's writes.
const CHUNK: usize = 1024 * 1024;

/// Games parsed between checks for cancellation.
const CHECK_EVERY: usize = 500;

/// What the first bytes of a fetched file say it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Found {
    /// An XML document, perhaps a Logiqx DAT or a DB export.
    Xml,
    /// A zip archive, perhaps a DAT pack.
    Zip,
    /// A bencoded dictionary, perhaps a `.torrent`.
    Torrent,
}

impl Found {
    /// The largest file of this type that is accepted, matching the uploads' limits.
    ///
    /// ```
    /// use mistarr_server::jobs::url_fetch::content::Found;
    /// assert_eq!(Found::Torrent.cap(), 16 << 20);
    /// assert_eq!(Found::Zip.cap(), 512 << 20);
    /// ```
    #[must_use]
    pub fn cap(self) -> u64 {
        match self {
            Self::Torrent => MAX_SOURCE_BYTES,
            Self::Xml | Self::Zip => MAX_DAT_BYTES,
        }
    }

    /// Where a file of this type goes: `dats` or `sources`.
    ///
    /// ```
    /// use mistarr_server::jobs::url_fetch::content::Found;
    /// assert_eq!(Found::Xml.target(), "dats");
    /// ```
    #[must_use]
    pub fn target(self) -> &'static str {
        match self {
            Self::Torrent => "sources",
            Self::Xml | Self::Zip => "dats",
        }
    }
}

/// The type the first bytes of a file announce, or `None` for anything that is not an
/// XML document, a zip archive or a bencoded dictionary, such as an HTML page or an image.
///
/// ```
/// use mistarr_server::jobs::url_fetch::content::{sniff, Found};
/// assert_eq!(sniff(b"<?xml version=\"1.0\"?><datafile>"), Some(Found::Xml));
/// assert_eq!(sniff(b"PK\x03\x04"), Some(Found::Zip));
/// assert_eq!(sniff(b"d8:announce"), Some(Found::Torrent));
/// assert_eq!(sniff(b"<!DOCTYPE html><html>"), None);
/// ```
#[must_use]
pub fn sniff(head: &[u8]) -> Option<Found> {
    if head.starts_with(b"PK\x03\x04") {
        return Some(Found::Zip);
    }
    if let Some(rest) = head.strip_prefix(b"d") {
        let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
        if (1..=3).contains(&digits) && rest.get(digits) == Some(&b':') {
            return Some(Found::Torrent);
        }
        return None;
    }
    let text = head.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(head);
    let start = text.iter().position(|b| !b.is_ascii_whitespace())?;
    let lower = text[start..].to_ascii_lowercase();
    let xml = [
        &b"<?xml"[..],
        b"<!doctype datafile",
        b"<datafile",
        b"<header",
        b"<!--",
    ];
    xml.iter()
        .any(|p| lower.starts_with(p))
        .then_some(Found::Xml)
}

/// Bytes one member of a fetched DAT pack may decompress to; a zip bomb stops here.
pub const MAX_MEMBER_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Whether the first bytes are gzip's, which a server may send despite being asked not to.
///
/// ```
/// assert!(mistarr_server::jobs::url_fetch::content::is_gzip(b"\x1f\x8b\x08"));
/// ```
#[must_use]
pub fn is_gzip(head: &[u8]) -> bool {
    head.starts_with(b"\x1f\x8b")
}

/// A fetched file that passed its check.
#[derive(Debug)]
#[non_exhaustive]
pub enum Checked {
    /// A DAT, or a pack of DATs rebuilt from its checked members alone.
    Dat,
    /// A `.torrent` that parses, with its bytes and infohash.
    Torrent {
        /// The whole file.
        bytes: Vec<u8>,
        /// Its infohash.
        infohash: [u8; 20],
    },
}

/// Why a fetched file was not accepted.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Refused {
    /// Not what its first bytes said, or not a DAT, DAT pack or torrent at all; the
    /// text says why, for the debug log.
    #[error("{0}")]
    NotAccepted(String),
    /// A zip that holds a file other than a `.dat` or `.xml` DAT.
    #[error("the zip holds files other than DATs")]
    OtherFiles,
    /// The check was asked to stop.
    #[error("stopped")]
    Stopped,
    /// The file could not be read, or the rebuilt pack written.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Checks the file at `path` fully as `found`: every game of a DAT to the end of the
/// file, or the whole torrent. A zip must hold only `.dat` or `.xml` DATs, each of which
/// is checked while it is written into a new zip at `rebuilt`, so the pack placed holds
/// only what was checked; its writes rest for what `pace` returns. `stop` is asked
/// between games.
///
/// # Errors
///
/// [`Refused`] saying why; a partial `rebuilt` is removed.
pub fn check(
    found: Found,
    path: &Path,
    rebuilt: &Path,
    pace: &Pace,
    stop: &dyn Fn() -> bool,
) -> Result<Checked, Refused> {
    match found {
        Found::Xml => {
            let file = std::fs::File::open(path)?;
            dat(BufReader::with_capacity(64 * 1024, file), stop)?;
            Ok(Checked::Dat)
        }
        Found::Zip => {
            let packed = pack(path, rebuilt, pace, stop);
            if packed.is_err() {
                let _ = std::fs::remove_file(rebuilt);
            }
            packed.map(|()| Checked::Dat)
        }
        Found::Torrent => {
            let file = std::fs::File::open(path)?;
            let mut bytes = Vec::new();
            file.take(MAX_SOURCE_BYTES + 1).read_to_end(&mut bytes)?;
            if bytes.len() as u64 > MAX_SOURCE_BYTES {
                return Err(Refused::NotAccepted("larger than a source may be".into()));
            }
            let meta = mistarr_sources::torrent::parse_torrent(&bytes)
                .map_err(|e| Refused::NotAccepted(format!("not a torrent: {e}")))?;
            Ok(Checked::Torrent {
                infohash: meta.infohash,
                bytes,
            })
        }
    }
}

/// Parses every game of one DAT, one at a time, and what follows its root.
fn dat<R: std::io::BufRead>(reader: R, stop: &dyn Fn() -> bool) -> Result<(), Refused> {
    let not_dat = |e: mistarr_core::dat::DatError| Refused::NotAccepted(format!("not a DAT: {e}"));
    let stream = DatStream::new(reader).map_err(not_dat)?;
    for (i, game) in stream.enumerate() {
        game.map_err(not_dat)?;
        if i % CHECK_EVERY == 0 && stop() {
            return Err(Refused::Stopped);
        }
    }
    Ok(())
}

fn is_dat_member(name: &str) -> bool {
    Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .is_some_and(|e| e == "dat" || e == "xml")
}

/// A reader that copies what it hands out to `out` and fails past [`MAX_MEMBER_BYTES`].
struct Tee<'a, R, W> {
    inner: R,
    out: &'a mut W,
    seen: &'a std::cell::Cell<u64>,
}

impl<R: Read, W: Write> Read for Tee<'_, R, W> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        let seen = self.seen.get() + n as u64;
        self.seen.set(seen);
        if seen > MAX_MEMBER_BYTES {
            return Err(std::io::Error::other("a member is too large"));
        }
        self.out.write_all(&buf[..n])?;
        Ok(n)
    }
}

/// Checks that the zip at `path` holds at least one DAT and nothing else, parsing each
/// member while writing its decompressed bytes into a fresh zip at `rebuilt`.
fn pack(path: &Path, rebuilt: &Path, pace: &Pace, stop: &dyn Fn() -> bool) -> Result<(), Refused> {
    let not_zip = |e: zip::result::ZipError| Refused::NotAccepted(format!("not a DAT pack: {e}"));
    let file = std::fs::File::open(path)?;
    let mut archive = zip::ZipArchive::new(BufReader::new(file)).map_err(not_zip)?;
    let mut members = Vec::new();
    for i in 0..archive.len() {
        let Some(name) = archive.name_for_index(i) else {
            continue;
        };
        if name.ends_with('/') {
            continue;
        }
        if !is_dat_member(name) {
            return Err(Refused::OtherFiles);
        }
        members.push(i);
    }
    if members.is_empty() {
        return Err(Refused::NotAccepted("the zip holds no DAT".into()));
    }
    let out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(rebuilt)?;
    let out = Paced::new(out, std::sync::Arc::clone(pace));
    let mut writer = zip::ZipWriter::new(std::io::BufWriter::with_capacity(CHUNK, out));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .compression_level(Some(1));
    for i in members {
        let member = archive.by_index(i).map_err(not_zip)?;
        let name = member.name().to_owned();
        writer.start_file(name, options).map_err(not_zip)?;
        let seen = std::cell::Cell::new(0);
        let tee = Tee {
            inner: member,
            out: &mut writer,
            seen: &seen,
        };
        let checked = dat(BufReader::with_capacity(64 * 1024, tee), stop);
        if seen.get() > MAX_MEMBER_BYTES {
            return Err(Refused::NotAccepted("a member is too large".into()));
        }
        checked?;
    }
    let mut out = writer.finish().map_err(not_zip)?;
    out.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    const DAT: &[u8] = br#"<?xml version="1.0"?>
<datafile><header><name>Example System</name></header>
<game name="Example Quest (World)"><rom name="q.bin" size="4" crc="0a0b0c0d"/></game>
</datafile>"#;

    fn zip_of(members: &[(&str, &[u8])]) -> Vec<u8> {
        let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, body) in members {
            z.start_file(*name, zip::write::SimpleFileOptions::default())
                .expect("start");
            z.write_all(body).expect("write");
        }
        z.finish().expect("finish").into_inner()
    }

    fn no_rest() -> Pace {
        std::sync::Arc::new(|_| std::time::Duration::ZERO)
    }

    fn checked(found: Found, bytes: &[u8]) -> Result<Checked, Refused> {
        rebuilt(found, bytes).0
    }

    /// The check's outcome and the bytes of the rebuilt pack, if one was left.
    fn rebuilt(found: Found, bytes: &[u8]) -> (Result<Checked, Refused>, Option<Vec<u8>>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("f");
        std::fs::write(&path, bytes).expect("write");
        let out = dir.path().join("f.zip");
        let r = check(found, &path, &out, &no_rest(), &|| false);
        (r, std::fs::read(&out).ok())
    }

    #[test]
    fn first_bytes_name_the_type() {
        assert_eq!(sniff(b"\xEF\xBB\xBF\n  <datafile>"), Some(Found::Xml));
        assert_eq!(sniff(b"<!DOCTYPE datafile PUBLIC"), Some(Found::Xml));
        assert_eq!(sniff(b"<header><version>1"), Some(Found::Xml));
        assert_eq!(sniff(b"d4:infod"), Some(Found::Torrent));
        assert_eq!(sniff(b"d13:announce-list"), Some(Found::Torrent));
        for other in [
            &b""[..],
            b"<html>",
            b"\x7fELF",
            b"NES\x1a",
            b"dx",
            b"d1234:",
            b"PK\x05\x06",
        ] {
            assert_eq!(sniff(other), None, "{other:?}");
        }
    }

    #[test]
    fn dats_and_packs_of_dats_pass() {
        assert!(matches!(checked(Found::Xml, DAT), Ok(Checked::Dat)));
        let pack = zip_of(&[("a.dat", DAT), ("sub/", b""), ("b.XML", DAT)]);
        assert!(matches!(checked(Found::Zip, &pack), Ok(Checked::Dat)));
    }

    #[test]
    fn anything_else_is_refused() {
        let refused = |r: Result<Checked, Refused>| matches!(r, Err(Refused::NotAccepted(_)));
        assert!(refused(checked(
            Found::Xml,
            b"<?xml version=\"1.0\"?><html></html>"
        )));
        assert!(refused(checked(Found::Xml, &DAT[..DAT.len() - 12])));
        let other = |r: Result<Checked, Refused>| matches!(r, Err(Refused::OtherFiles));
        assert!(other(checked(
            Found::Zip,
            &zip_of(&[("a.dat", DAT), ("g.bin", b"rom")])
        )));
        assert!(other(checked(
            Found::Zip,
            &zip_of(&[("readme.txt", b"hi")])
        )));
        let (r, left) = rebuilt(Found::Zip, &zip_of(&[("a.dat", DAT), ("b.dat", b"<x/>")]));
        assert!(refused(r));
        assert!(left.is_none(), "a failed rebuild leaves nothing");
        let mut appended = DAT.to_vec();
        appended.extend_from_slice(b"NES\x1a rom bytes");
        assert!(refused(checked(Found::Xml, &appended)));
        assert!(refused(checked(
            Found::Zip,
            &zip_of(&[("a.dat", &appended)])
        )));
        assert!(refused(checked(
            Found::Zip,
            &zip_of(&[("a.dat", b"not xml")])
        )));
        assert!(refused(checked(Found::Zip, b"PK\x03\x04 broken")));
        assert!(refused(checked(Found::Torrent, b"d4:infoi1ee")));
    }

    #[test]
    fn a_torrent_passes_with_its_hash() {
        let torrent = b"d4:infod6:lengthi3e4:name5:a.bin12:piece lengthi16384e6:pieces20:aaaaaaaaaaaaaaaaaaaaee";
        let Ok(Checked::Torrent { bytes, infohash }) = checked(Found::Torrent, torrent) else {
            panic!("a torrent");
        };
        assert_eq!(bytes, torrent);
        assert_ne!(infohash, [0; 20]);
    }

    #[test]
    fn a_check_stops_when_asked() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("f");
        std::fs::write(&path, DAT).expect("write");
        let out = dir.path().join("out.zip");
        assert!(matches!(
            check(Found::Xml, &path, &out, &no_rest(), &|| true),
            Err(Refused::Stopped)
        ));
        assert!(matches!(
            check(
                Found::Xml,
                &dir.path().join("none"),
                &out,
                &no_rest(),
                &|| false
            ),
            Err(Refused::Io(_))
        ));
    }

    proptest::proptest! {
        #[test]
        fn sniffing_any_bytes_never_panics(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..96)) {
            let found = sniff(&bytes);
            if bytes.starts_with(b"PK\x03\x04") {
                proptest::prop_assert_eq!(found, Some(Found::Zip));
            }
        }
    }

    #[test]
    fn a_pack_is_rebuilt_from_its_checked_members_alone() {
        let hidden = b"NES\x1a hidden bytes the directory does not list";
        let mut padded = hidden.to_vec();
        let pack = zip_of(&[("a.dat", DAT), ("b.xml", DAT)]);
        padded.extend_from_slice(&pack);
        padded.extend_from_slice(hidden);
        let (r, out) = rebuilt(Found::Zip, &padded);
        assert!(matches!(r, Ok(Checked::Dat)), "{r:?}");
        let out = out.expect("a rebuilt pack");
        assert!(!out.windows(hidden.len()).any(|w| w == hidden));
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(out)).expect("a zip");
        assert_eq!(archive.len(), 2);
        let mut text = String::new();
        archive
            .by_name("b.xml")
            .expect("member")
            .read_to_string(&mut text)
            .expect("read");
        assert_eq!(text.as_bytes(), DAT);
        assert!(is_gzip(b"\x1f\x8b") && !is_gzip(DAT));
    }

    #[test]
    fn targets_follow_the_type() {
        assert_eq!(Found::Torrent.target(), "sources");
        assert_eq!(Found::Zip.target(), "dats");
        assert_eq!(Found::Xml.cap(), MAX_DAT_BYTES);
    }
}
