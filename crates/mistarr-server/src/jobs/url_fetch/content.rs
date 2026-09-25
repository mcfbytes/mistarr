//! What a fetched file is, from its first bytes and then its whole content.

use std::cell::Cell;
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use mistarr_core::dat::{rewrite, RewriteError};

use crate::jobs::dat_import::MAX_DAT_BYTES;
use crate::jobs::source_import::MAX_SOURCE_BYTES;
use crate::jobs::url_fetch::spool::{Spill, Target, RECHECK_BYTES};

/// Bytes read before the type is decided, unless the file is shorter.
pub const SNIFF_BYTES: usize = 64;

/// Buffer of the rewrite's writes.
const CHUNK: usize = 1024 * 1024;

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

/// Bytes the members of a fetched DAT pack may decompress to, one or all together; a zip
/// bomb stops here.
pub const MAX_UNPACKED_BYTES: u64 = MAX_DAT_BYTES;

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
    /// A DAT or a pack of DATs, rewritten by mistarr into a new file; the fetched bytes
    /// are not kept.
    Dat {
        /// The rewrite.
        path: PathBuf,
        /// Whether it is in RAM.
        in_ram: bool,
    },
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
    /// A pack unpacks, or a rewrite grows, past [`MAX_UNPACKED_BYTES`].
    #[error("larger than a DAT may be")]
    TooLarge,
    /// The card has too little room for the rewrite.
    #[error("the card has too little free space")]
    NoRoom,
    /// The check was asked to stop.
    #[error("stopped")]
    Stopped,
    /// The file could not be read, or the rewrite written.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Checks the file at `path` fully as `found`. A DAT is parsed to the end of the file and
/// rewritten by [`rewrite`] into a file where `target` allows; each member of a zip, which
/// must all be `.dat` or `.xml` DATs, is rewritten likewise into a new zip. A torrent is
/// parsed whole. `stop` is asked every [`RECHECK_BYTES`] read.
///
/// # Errors
///
/// [`Refused`] saying why; the partial rewrite is removed at once.
pub fn check(
    found: Found,
    path: &Path,
    target: &Target,
    stop: &dyn Fn() -> bool,
) -> Result<Checked, Refused> {
    match found {
        Found::Xml => {
            let file = std::fs::File::open(path)?;
            let size = file.metadata()?.len();
            let budget = Budget::new(stop);
            let spill = Spill::create(target.clone(), size).map_err(write_error)?;
            let mut out = std::io::BufWriter::with_capacity(CHUNK, spill);
            let input = Counted {
                inner: file,
                budget: &budget,
            };
            let rewritten = rewrite(BufReader::with_capacity(64 * 1024, input), &mut out);
            budget.verdict(rewritten.map(|_| ()))?;
            let spill = out.into_inner().map_err(|e| write_error(e.into_error()))?;
            let (path, in_ram) = spill.finish().map_err(write_error)?;
            Ok(Checked::Dat { path, in_ram })
        }
        Found::Zip => pack(path, target, stop),
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

/// A write error as a refusal: a full card, a rewrite past its cap, or plain I/O.
fn write_error(e: std::io::Error) -> Refused {
    match e.kind() {
        std::io::ErrorKind::StorageFull => Refused::NoRoom,
        std::io::ErrorKind::FileTooLarge => Refused::TooLarge,
        _ => Refused::Io(e),
    }
}

/// What the input readers of one check have taken, and why they stopped.
struct Budget<'a> {
    read: Cell<u64>,
    next_stop_check: Cell<u64>,
    stop: &'a dyn Fn() -> bool,
    stopped: Cell<bool>,
    over: Cell<bool>,
}

impl<'a> Budget<'a> {
    fn new(stop: &'a dyn Fn() -> bool) -> Self {
        Self {
            read: Cell::new(0),
            next_stop_check: Cell::new(0),
            stop,
            stopped: Cell::new(false),
            over: Cell::new(false),
        }
    }

    /// Counts `n` more bytes read, failing past [`MAX_UNPACKED_BYTES`] or once `stop` says so.
    fn take(&self, n: usize) -> std::io::Result<()> {
        let read = self.read.get() + n as u64;
        self.read.set(read);
        if read > MAX_UNPACKED_BYTES {
            self.over.set(true);
            return Err(std::io::Error::other("larger than a DAT may be"));
        }
        if read >= self.next_stop_check.get() {
            self.next_stop_check.set(read + RECHECK_BYTES);
            if (self.stop)() {
                self.stopped.set(true);
                return Err(std::io::Error::other("stopped"));
            }
        }
        Ok(())
    }

    /// The refusal a rewrite's outcome stands for, the readers' own reasons first.
    fn verdict(&self, done: Result<(), RewriteError>) -> Result<(), Refused> {
        if self.stopped.get() {
            return Err(Refused::Stopped);
        }
        if self.over.get() {
            return Err(Refused::TooLarge);
        }
        match done {
            Ok(()) => Ok(()),
            Err(RewriteError::Write(e)) => Err(write_error(e)),
            Err(e) => Err(Refused::NotAccepted(format!("not a DAT: {e}"))),
        }
    }
}

/// A reader counting what it hands out against a [`Budget`].
struct Counted<'a, 'b, R> {
    inner: R,
    budget: &'a Budget<'b>,
}

impl<R: Read> Read for Counted<'_, '_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.budget.read.get() == 0 {
            self.budget.take(0)?;
        }
        let n = self.inner.read(buf)?;
        self.budget.take(n)?;
        Ok(n)
    }
}

fn is_dat_member(name: &str) -> bool {
    Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .is_some_and(|e| e == "dat" || e == "xml")
}

/// The rewrite of a pack: a [`Spill`] that becomes a sink once the pack is refused, so the
/// zip writer's closing writes on drop cost nothing before the file is removed. It keeps
/// the position and length, which the zip writer still seeks by as a sink.
struct PackOut {
    spill: Spill,
    abandoned: Rc<Cell<bool>>,
    pos: u64,
    len: u64,
}

impl Write for PackOut {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = if self.abandoned.get() {
            buf.len()
        } else {
            self.spill.write(buf)?
        };
        self.pos += n as u64;
        self.len = self.len.max(self.pos);
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if self.abandoned.get() {
            return Ok(());
        }
        self.spill.flush()
    }
}

impl Seek for PackOut {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        if !self.abandoned.get() {
            self.pos = self.spill.seek(pos)?;
            return Ok(self.pos);
        }
        let to = match pos {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
            SeekFrom::End(d) => self.len.checked_add_signed(d),
        };
        self.pos = to.ok_or_else(|| std::io::Error::other("seek out of range"))?;
        Ok(self.pos)
    }
}

/// Checks that the zip at `path` holds at least one DAT and nothing else, and that its
/// members unpack to at most [`MAX_UNPACKED_BYTES`] together, rewriting each member into
/// a new zip where `target` allows.
fn pack(path: &Path, target: &Target, stop: &dyn Fn() -> bool) -> Result<Checked, Refused> {
    let not_zip = |e: zip::result::ZipError| Refused::NotAccepted(format!("not a DAT pack: {e}"));
    let file = std::fs::File::open(path)?;
    let size = file.metadata()?.len();
    let mut archive = zip::ZipArchive::new(BufReader::new(file)).map_err(not_zip)?;
    let mut members = Vec::new();
    let mut declared = 0u64;
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
    for &i in &members {
        declared = declared.saturating_add(archive.by_index_raw(i).map_err(not_zip)?.size());
    }
    if declared > MAX_UNPACKED_BYTES {
        return Err(Refused::TooLarge);
    }
    let abandoned = Rc::new(Cell::new(false));
    let spill = Spill::create(target.clone(), size).map_err(write_error)?;
    let out = PackOut {
        spill,
        abandoned: Rc::clone(&abandoned),
        pos: 0,
        len: 0,
    };
    let mut writer = zip::ZipWriter::new(std::io::BufWriter::with_capacity(CHUNK, out));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .compression_level(Some(1));
    let budget = Budget::new(stop);
    let rewritten = (|| -> Result<(), Refused> {
        for &i in &members {
            let member = archive.by_index(i).map_err(not_zip)?;
            let name = member.name().to_owned();
            writer.start_file(name, options).map_err(|e| match e {
                zip::result::ZipError::Io(e) => write_error(e),
                e => not_zip(e),
            })?;
            let input = Counted {
                inner: member,
                budget: &budget,
            };
            let done = rewrite(BufReader::with_capacity(64 * 1024, input), &mut writer);
            budget.verdict(done.map(|_| ()))?;
        }
        Ok(())
    })();
    if let Err(e) = rewritten {
        abandoned.set(true);
        return Err(e);
    }
    let finished = writer.finish().map_err(|e| match e {
        zip::result::ZipError::Io(e) => write_error(e),
        e => not_zip(e),
    });
    let buffered = match finished {
        Ok(b) => b,
        Err(e) => {
            abandoned.set(true);
            return Err(e);
        }
    };
    let out = buffered
        .into_inner()
        .map_err(|e| write_error(e.into_error()))?;
    let (path, in_ram) = out.spill.finish().map_err(write_error)?;
    Ok(Checked::Dat { path, in_ram })
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::Arc;

    use super::*;
    use crate::jobs::url_fetch::spool::Places;

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

    fn target(dir: &Path) -> Target {
        Target {
            places: Places {
                ram: None,
                card: dir.join("card"),
                floor: 0,
            },
            name: "fetch-1-1-out.part".into(),
            ram: false,
            pace: Arc::new(|_| std::time::Duration::ZERO),
            limit: MAX_DAT_BYTES,
        }
    }

    fn checked(found: Found, bytes: &[u8]) -> Result<Checked, Refused> {
        rewritten(found, bytes).0
    }

    /// The check's outcome and the rewrite's bytes; asserts nothing else is left behind.
    fn rewritten(found: Found, bytes: &[u8]) -> (Result<Checked, Refused>, Option<Vec<u8>>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("f");
        std::fs::write(&path, bytes).expect("write");
        let r = check(found, &path, &target(dir.path()), &|| false);
        let out = match &r {
            Ok(Checked::Dat { path, .. }) => Some(std::fs::read(path).expect("read")),
            _ => None,
        };
        let left = std::fs::read_dir(dir.path().join("card")).map_or(0, Iterator::count);
        assert_eq!(
            left,
            usize::from(out.is_some()),
            "only a finished rewrite is left"
        );
        (r, out)
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
    fn a_dat_is_rewritten_without_what_the_parser_does_not_read() {
        let noisy = br#"<?xml version="1.0"?><!DOCTYPE datafile><!-- hidden-comment -->
<datafile><header><name>Example System</name></header>
<hidden-element><![CDATA[hidden-cdata]]></hidden-element>
<game name="Example Quest (World)"><rom name="q.bin" size="4" crc="0a0b0c0d"/><hidden-rom-sibling/></game>
</datafile>"#;
        let (r, out) = rewritten(Found::Xml, noisy);
        assert!(matches!(r, Ok(Checked::Dat { in_ram: false, .. })), "{r:?}");
        let out = String::from_utf8(out.expect("a rewrite")).expect("utf-8");
        assert!(!out.contains("hidden") && !out.contains("DOCTYPE"), "{out}");
        assert_eq!(
            mistarr_core::dat::parse_dat(out.as_bytes()).expect("parse"),
            mistarr_core::dat::parse_dat(noisy).expect("parse")
        );
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
        assert!(refused(checked(
            Found::Zip,
            &zip_of(&[("a.dat", DAT), ("b.dat", b"<x/>")])
        )));
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
    fn reading_past_the_unpacked_cap_or_after_a_stop_is_refused() {
        let budget = Budget::new(&|| false);
        let cap = usize::try_from(MAX_UNPACKED_BYTES).expect("fits");
        budget.take(cap).expect("within");
        assert!(budget.take(1).is_err());
        assert!(matches!(budget.verdict(Ok(())), Err(Refused::TooLarge)));
        let stopping = Budget::new(&|| true);
        assert!(stopping.take(0).is_err());
        assert!(matches!(stopping.verdict(Ok(())), Err(Refused::Stopped)));
        let asked = Cell::new(0);
        let counting = || {
            asked.set(asked.get() + 1);
            false
        };
        let every = Budget::new(&counting);
        let step = usize::try_from(RECHECK_BYTES).expect("fits") / 4;
        for _ in 0..8 {
            every.take(step).expect("within");
        }
        assert_eq!(asked.get(), 2, "asked once per RECHECK_BYTES read");
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
    fn a_check_stops_when_asked_and_leaves_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("f");
        std::fs::write(&path, DAT).expect("write");
        let t = target(dir.path());
        assert!(matches!(
            check(Found::Xml, &path, &t, &|| true),
            Err(Refused::Stopped)
        ));
        let pack = dir.path().join("p.zip");
        std::fs::write(&pack, zip_of(&[("a.dat", DAT)])).expect("write");
        assert!(matches!(
            check(Found::Zip, &pack, &t, &|| true),
            Err(Refused::Stopped)
        ));
        assert_eq!(
            std::fs::read_dir(dir.path().join("card")).map_or(0, Iterator::count),
            0
        );
        assert!(matches!(
            check(Found::Xml, &dir.path().join("none"), &t, &|| false),
            Err(Refused::Io(_))
        ));
    }

    #[test]
    fn a_rewrite_past_its_limit_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("f");
        std::fs::write(&path, DAT).expect("write");
        let mut t = target(dir.path());
        t.limit = 16;
        assert!(matches!(
            check(Found::Xml, &path, &t, &|| false),
            Err(Refused::TooLarge)
        ));
        assert_eq!(
            std::fs::read_dir(dir.path().join("card")).map_or(0, Iterator::count),
            0
        );
        assert!(matches!(
            write_error(std::io::ErrorKind::StorageFull.into()),
            Refused::NoRoom
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
    fn a_pack_is_rebuilt_from_rewritten_members_alone() {
        let hidden = b"NES\x1a hidden bytes the directory does not list";
        let mut padded = hidden.to_vec();
        let noisy = [DAT, b"<!-- hidden-member-comment -->"].concat();
        let pack = zip_of(&[("a.dat", DAT), ("b.xml", &noisy)]);
        padded.extend_from_slice(&pack);
        padded.extend_from_slice(hidden);
        let (r, out) = rewritten(Found::Zip, &padded);
        assert!(matches!(r, Ok(Checked::Dat { .. })), "{r:?}");
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
        assert!(!text.contains("hidden"));
        assert_eq!(
            mistarr_core::dat::parse_dat(text.as_bytes()).expect("parse"),
            mistarr_core::dat::parse_dat(DAT).expect("parse")
        );
        assert!(is_gzip(b"\x1f\x8b") && !is_gzip(DAT));
    }

    #[test]
    fn targets_follow_the_type() {
        assert_eq!(Found::Torrent.target(), "sources");
        assert_eq!(Found::Zip.target(), "dats");
        assert_eq!(Found::Xml.cap(), MAX_DAT_BYTES);
    }
}
