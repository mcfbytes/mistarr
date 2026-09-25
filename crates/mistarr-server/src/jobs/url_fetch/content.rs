//! What a fetched file is, from its first bytes and then its whole content.

use std::io::{BufReader, Read};
use std::path::Path;

use mistarr_core::dat::DatStream;

use crate::jobs::dat_import::MAX_DAT_BYTES;
use crate::jobs::source_import::MAX_SOURCE_BYTES;

/// Bytes read before the type is decided, unless the file is shorter.
pub const SNIFF_BYTES: usize = 64;

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

/// A fetched file that passed its check.
#[derive(Debug)]
#[non_exhaustive]
pub enum Checked {
    /// A DAT or a pack of DATs, each of which parses.
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
    /// The check was asked to stop.
    #[error("stopped")]
    Stopped,
    /// The file could not be read.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Checks the file at `path` fully as `found`: every game of a DAT, every member of a
/// zip, which must all be `.dat` or `.xml` DATs, or the whole torrent. `stop` is asked
/// between games.
///
/// # Errors
///
/// [`Refused`] saying why.
pub fn check(found: Found, path: &Path, stop: &dyn Fn() -> bool) -> Result<Checked, Refused> {
    match found {
        Found::Xml => {
            let file = std::fs::File::open(path)?;
            dat(BufReader::with_capacity(64 * 1024, file), stop)?;
            Ok(Checked::Dat)
        }
        Found::Zip => pack(path, stop).map(|()| Checked::Dat),
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

/// Parses every game of one DAT, one at a time.
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

/// Checks that every file in the zip at `path` is a `.dat` or `.xml` DAT, and that there is one.
fn pack(path: &Path, stop: &dyn Fn() -> bool) -> Result<(), Refused> {
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
            return Err(Refused::NotAccepted(
                "the zip holds a file that is not a DAT".into(),
            ));
        }
        members.push(i);
    }
    if members.is_empty() {
        return Err(Refused::NotAccepted("the zip holds no DAT".into()));
    }
    for i in members {
        let member = archive.by_index(i).map_err(not_zip)?;
        dat(BufReader::with_capacity(64 * 1024, member), stop)?;
    }
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

    fn checked(found: Found, bytes: &[u8]) -> Result<Checked, Refused> {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("f");
        std::fs::write(&path, bytes).expect("write");
        check(found, &path, &|| false)
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
        assert!(refused(checked(
            Found::Zip,
            &zip_of(&[("a.dat", DAT), ("g.bin", b"rom")])
        )));
        assert!(refused(checked(
            Found::Zip,
            &zip_of(&[("readme.txt", b"hi")])
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
        assert!(matches!(
            check(Found::Xml, &path, &|| true),
            Err(Refused::Stopped)
        ));
        assert!(matches!(
            check(Found::Xml, &dir.path().join("none"), &|| false),
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
    fn targets_follow_the_type() {
        assert_eq!(Found::Torrent.target(), "sources");
        assert_eq!(Found::Zip.target(), "dats");
        assert_eq!(Found::Xml.cap(), MAX_DAT_BYTES);
    }
}
