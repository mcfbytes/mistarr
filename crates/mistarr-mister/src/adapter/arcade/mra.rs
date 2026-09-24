//! MRA parsing: which MAME zips an arcade core definition needs and how its roms are built.

use std::io::{self, BufRead, BufReader, Read, Seek as _, SeekFrom};
use std::path::Path;
use std::sync::Arc;

use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

use crate::{Error, Result};

/// What an MRA file asks for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Mra {
    /// Display name from `<name>`.
    pub name: Option<String>,
    /// MAME set name from `<setname>`.
    pub setname: Option<String>,
    /// Core name from `<rbf>`.
    pub rbf: Option<String>,
    /// Zip file names from every `zip` attribute, `|`-separated lists split, first-seen order.
    pub zips: Vec<String>,
    /// Lowercase `md5` attributes of `<rom>` elements that carry a 32-digit hex value.
    pub md5: Vec<String>,
    /// Every `<rom>` element in document order.
    pub roms: Vec<MraRom>,
}

impl Mra {
    /// Where each zip of [`Mra::zips`] lives under `games/`, first-seen order, duplicates
    /// and names that leave `games/` dropped.
    ///
    /// ```
    /// let mra = mistarr_mister::adapter::arcade::mra::Mra {
    ///     zips: vec!["exblast.zip".into(), "/hbmame/exhb.zip".into()],
    ///     ..Default::default()
    /// };
    /// let paths: Vec<String> = mra.zip_paths().iter().map(|z| z.rel_path()).collect();
    /// assert_eq!(paths, ["mame/exblast.zip", "hbmame/exhb.zip"]);
    /// ```
    #[must_use]
    pub fn zip_paths(&self) -> Vec<ZipPath> {
        let mut out: Vec<ZipPath> = Vec::new();
        for zip in &self.zips {
            if let Some(p) = zip_location(zip) {
                if !out.contains(&p) {
                    out.push(p);
                }
            }
        }
        out
    }
}

/// One `<rom>` element: where its parts come from and how they are laid out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MraRom {
    /// The `index` attribute; 0 when absent or not a number.
    pub index: u32,
    /// Zip names from the `zip` attribute, tried in order for each named part.
    pub zips: Vec<String>,
    /// Lowercase expected MD5; `None` when absent, `none` or not 32 hex digits.
    pub md5: Option<String>,
    /// Content in document order.
    pub items: Vec<RomItem>,
}

/// One piece of a `<rom>`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RomItem {
    /// A `<part>` outside any interleave.
    Part(Part),
    /// An `<interleave>` and the parts inside it.
    Interleave(Interleave),
    /// A `<patch>` applied to the assembled bytes.
    Patch(Patch),
    /// Something the assembler does not implement, with the reason.
    Unsupported(String),
}

/// A `<part>`: bytes from a zip member, or inline hex bytes when it has no name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    /// Member name inside the zip; `None` for inline data.
    pub name: Option<String>,
    /// Zip names from the part's own `zip` attribute, overriding the rom's when not empty.
    pub zips: Vec<String>,
    /// Expected CRC32 of the member, used to find it when no member has the name.
    pub crc: Option<u32>,
    /// Bytes skipped at the start of the member.
    pub offset: u64,
    /// Bytes taken after the offset; `None` takes the rest of the member.
    pub length: Option<u64>,
    /// How many times the bytes are emitted.
    pub repeat: u64,
    /// The `map` attribute as written: hex digits, one per output byte.
    pub map: Option<String>,
    /// Inline bytes of an unnamed part parsed from memory.
    pub data: Vec<u8>,
    /// Where an unnamed part's hex sits in the file it was read from, in place of `data`.
    pub inline: Option<Inline>,
}

/// The hex text of an inline part left in its MRA file, read again by [`open_inline`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inline {
    /// The MRA file.
    pub file: Arc<Path>,
    /// Byte offset just after the `<part>` start tag.
    pub start: u64,
    /// Byte offset of the tag that ends the part.
    pub end: u64,
    /// Number of bytes the hex decodes to.
    pub len: u64,
}

impl Default for Part {
    fn default() -> Self {
        Self {
            name: None,
            zips: Vec::new(),
            crc: None,
            offset: 0,
            length: None,
            repeat: 1,
            map: None,
            data: Vec::new(),
            inline: None,
        }
    }
}

/// An `<interleave>`: parts whose bytes are spread over output words.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Interleave {
    /// The `input` attribute in bits; 8 when absent.
    pub input: u32,
    /// The `output` attribute in bits.
    pub output: u32,
    /// Parts in document order.
    pub parts: Vec<Part>,
}

/// A `<patch>`: bytes written over, or combined by exclusive or into, the assembled rom.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Patch {
    /// Position in the assembled rom.
    pub offset: u64,
    /// `operation="xor"`; otherwise the bytes replace what is there.
    pub xor: bool,
    /// The patch bytes.
    pub data: Vec<u8>,
}

/// A zip's place under `games/`: MiSTer reads a plain name from `mame/` and a name
/// starting with `/` from the games root; `..` components are resolved.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ZipPath {
    /// Directory relative to `games/`, `/`-separated, e.g. `mame` or `hbmame`.
    pub dir: String,
    /// File name of the zip.
    pub file: String,
}

impl ZipPath {
    /// `dir/file`, relative to `games/`.
    ///
    /// ```
    /// let z = mistarr_mister::adapter::arcade::mra::zip_location("exblast.zip").unwrap();
    /// assert_eq!(z.rel_path(), "mame/exblast.zip");
    /// ```
    #[must_use]
    pub fn rel_path(&self) -> String {
        if self.dir.is_empty() {
            self.file.clone()
        } else {
            format!("{}/{}", self.dir, self.file)
        }
    }
}

/// Resolves an MRA `zip` name to its place under `games/`, or `None` when it is
/// empty, uses a backslash or leaves `games/`.
///
/// ```
/// use mistarr_mister::adapter::arcade::mra::zip_location;
/// assert_eq!(zip_location("../hbmame/exhb.zip").unwrap().dir, "hbmame");
/// assert_eq!(zip_location("sub/exblast.zip").unwrap().dir, "mame/sub");
/// assert!(zip_location("/../exblast.zip").is_none());
/// ```
#[must_use]
pub fn zip_location(zip: &str) -> Option<ZipPath> {
    let zip = zip.trim();
    if zip.contains('\\') {
        return None;
    }
    let (mut parts, rest): (Vec<(&str, bool)>, &str) = match zip.strip_prefix('/') {
        Some(rest) => (Vec::new(), rest),
        None => (vec![("mame", false)], zip),
    };
    if rest.ends_with('/') {
        return None;
    }
    for c in rest.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            c => parts.push((c, true)),
        }
    }
    let (file, named) = parts.pop()?;
    if !named {
        return None;
    }
    let file = file.to_owned();
    let parts: Vec<&str> = parts.into_iter().map(|(c, _)| c).collect();
    Some(ZipPath {
        dir: parts.join("/"),
        file,
    })
}

/// Parses MRA XML.
///
/// Text of `<name>`, `<setname>` and `<rbf>` includes CDATA, resolved entities
/// and the text of any nested child elements, so `<name>A<b>x</b>B</name>` reads `AxB`.
/// Rom content the assembler does not implement is kept as [`RomItem::Unsupported`].
/// Element and attribute names compare case-insensitively, as MiSTer's loader reads them:
/// an end tag closes the innermost open element of its name, a stray end tag is ignored,
/// and an unknown entity stays as written.
///
/// # Errors
///
/// Returns [`Error::Mra`] when the document is not well-formed or ends with open elements.
///
/// ```
/// let mra = mistarr_mister::adapter::arcade::mra::parse(
///     br#"<misterromdescription><SetName>exblast</setname>
///         <ROM index="0" ZIP="exblast.zip|exparent.zip"/></misterromdescription>"#,
/// ).unwrap();
/// assert_eq!(mra.setname.as_deref(), Some("exblast"));
/// assert_eq!(mra.zips, ["exblast.zip", "exparent.zip"]);
/// assert_eq!(mra.roms[0].zips, mra.zips);
/// ```
pub fn parse(xml: &[u8]) -> Result<Mra> {
    parse_from(xml, None)
}

/// Parses MRA markup from `input`; inline part bytes are kept in [`Part::data`], or with
/// `file` set, left in that file as [`Part::inline`] so no payload is held.
fn parse_from<R: BufRead>(input: R, file: Option<&Arc<Path>>) -> Result<Mra> {
    let mut reader = Reader::from_reader(input);
    reader.config_mut().check_end_names = false;
    let mut buf = Vec::new();
    let mut mra = Mra::default();
    let mut open: Vec<(String, Option<Field>)> = Vec::new();
    let mut rom: Option<RomBuilder> = None;
    let keep = file.is_none();
    // Bytes of inline hex read here rather than by the XML reader, which does not count them.
    let mut taken = 0;
    loop {
        if let Some(RomBuilder {
            open: Some(Open::Part(part, hex, None, _)),
            skip: 0,
            ..
        }) = &mut rom
        {
            if part.name.is_none() {
                taken += take_text(reader.get_mut(), hex, keep.then_some(&mut part.data))?;
            }
        }
        let before = reader.buffer_position() + taken;
        buf.clear();
        let event = reader.read_event_into(&mut buf).map_err(xml_err)?;
        let at = Pos {
            before,
            after: reader.buffer_position() + taken,
            file,
        };
        let field = open.last().and_then(|(_, f)| *f);
        match event {
            Event::Start(e) => {
                let name = tag(e.local_name().as_ref());
                // A `<rom>` inside an unclosed field never feeds that field.
                let f = Field::of(&name).or(field.filter(|_| name != "rom"));
                open.push((name, f));
                read_attributes(&e, &mut mra)?;
                start(&e, &mut rom, at.after)?;
            }
            Event::Empty(e) => {
                read_attributes(&e, &mut mra)?;
                start(&e, &mut rom, at.after)?;
                end(
                    &tag(e.local_name().as_ref()),
                    &mut rom,
                    &mut mra,
                    at.after,
                    &at,
                );
            }
            Event::Text(t) => {
                let t = t.decode().map_err(xml_err)?;
                text(&t, field, &mut mra, &mut rom, keep);
            }
            Event::CData(c) => {
                let c = c.decode().map_err(xml_err)?;
                text(&c, field, &mut mra, &mut rom, keep);
            }
            Event::GeneralRef(r) => text(&resolve_ref(&r)?, field, &mut mra, &mut rom, keep),
            Event::End(e) => {
                let name = tag(e.local_name().as_ref());
                if let Some(i) = open.iter().rposition(|(n, _)| *n == name) {
                    for (closed, _) in open.drain(i..).rev() {
                        end(&closed, &mut rom, &mut mra, at.before, &at);
                    }
                }
            }
            Event::Eof if open.is_empty() => break,
            Event::Eof => return Err(Error::Mra("document ends inside an element".into())),
            _ => {}
        }
    }
    for s in [&mut mra.name, &mut mra.setname, &mut mra.rbf]
        .into_iter()
        .flatten()
    {
        *s = s.trim().to_owned();
    }
    Ok(mra)
}

/// Decodes plain text up to the next `<` or `&` straight from `input`, a buffer at a time,
/// so a large inline part is never held as one text event. Returns the bytes consumed.
fn take_text<R: BufRead>(
    input: &mut R,
    hex: &mut Hex,
    mut out: Option<&mut Vec<u8>>,
) -> io::Result<u64> {
    let mut consumed = 0;
    loop {
        let buf = input.fill_buf()?;
        let stop = buf.iter().position(|&b| b == b'<' || b == b'&');
        let n = stop.unwrap_or(buf.len());
        if n == 0 {
            return Ok(consumed);
        }
        hex.feed(&buf[..n], out.as_deref_mut());
        input.consume(n);
        consumed += n as u64;
        if stop.is_some() {
            return Ok(consumed);
        }
    }
}

/// Where the event being handled sits in the input.
struct Pos<'a> {
    before: u64,
    after: u64,
    file: Option<&'a Arc<Path>>,
}

/// A character or predefined entity reference as text; an unknown entity stays as written.
fn resolve_ref(r: &quick_xml::events::BytesRef<'_>) -> Result<String> {
    if let Some(c) = r.resolve_char_ref().map_err(xml_err)? {
        return Ok(c.to_string());
    }
    let name = r.decode().map_err(xml_err)?;
    Ok(resolve_predefined_entity(&name).map_or_else(|| format!("&{name};"), str::to_owned))
}

fn xml_err(e: impl std::fmt::Display) -> Error {
    Error::Mra(e.to_string())
}

/// Largest MRA file read, a sanity bound: inline part data makes some a few MiB.
pub const MAX_MRA_BYTES: u64 = 16 * 1024 * 1024;

/// Longest `<name>`, `<setname>` or `<rbf>` text kept, in bytes; the rest is dropped.
pub const MAX_FIELD_BYTES: usize = 256;

/// Version of what [`parse`] reads from an MRA; it changes whenever a file could parse differently.
pub const PARSER_VERSION: u32 = 2;

/// Reads and parses an MRA file of at most [`MAX_MRA_BYTES`], streaming it; inline part
/// data is checked and left in the file as [`Part::inline`].
///
/// # Errors
///
/// Returns [`Error::Io`] when the file cannot be read and [`Error::Mra`] when it is
/// too large or not well-formed.
///
/// ```
/// let path = std::env::temp_dir().join("mistarr-doc-example.mra");
/// std::fs::write(&path, r#"<misterromdescription><rom zip="exblast.zip"/></misterromdescription>"#).unwrap();
/// assert_eq!(mistarr_mister::adapter::arcade::mra::read(&path).unwrap().zips, ["exblast.zip"]);
/// ```
pub fn read(path: &Path) -> Result<Mra> {
    let file = std::fs::File::open(path)?;
    if file.metadata()?.len() > MAX_MRA_BYTES {
        return Err(too_big());
    }
    let mut limited = BufReader::new(file.take(MAX_MRA_BYTES + 1));
    let mra = parse_from(&mut limited, Some(&Arc::from(path)))?;
    if limited.into_inner().limit() == 0 {
        return Err(too_big());
    }
    Ok(mra)
}

fn too_big() -> Error {
    Error::Mra(format!("file is larger than {MAX_MRA_BYTES} bytes"))
}

/// Streams the bytes of an inline part read by [`read`], decoding its hex again from the file.
///
/// # Errors
///
/// An I/O error when the file cannot be read; reading fails with `InvalidData` when the
/// text does not decode to [`Inline::len`] bytes, as when the file changed after [`read`].
///
/// ```
/// use std::io::Read as _;
/// use mistarr_mister::adapter::arcade::mra;
/// let path = std::env::temp_dir().join("mistarr-doc-inline.mra");
/// std::fs::write(&path, "<m><rom><part>61 62 63</part></rom></m>").unwrap();
/// let parsed = mra::read(&path).unwrap();
/// let mra::RomItem::Part(part) = &parsed.roms[0].items[0] else { panic!() };
/// let mut bytes = Vec::new();
/// mra::open_inline(part.inline.as_ref().unwrap()).unwrap().read_to_end(&mut bytes).unwrap();
/// assert_eq!(bytes, b"abc");
/// ```
pub fn open_inline(inline: &Inline) -> io::Result<InlineReader> {
    let mut file = std::fs::File::open(&inline.file)?;
    file.seek(SeekFrom::Start(inline.start))?;
    let span = inline.end.saturating_sub(inline.start);
    let mut reader = Reader::from_reader(BufReader::new(file.take(span)));
    reader.config_mut().check_end_names = false;
    Ok(InlineReader {
        reader,
        buf: Vec::new(),
        out: Vec::new(),
        at: 0,
        hex: Hex::default(),
        expected: inline.len,
        done: false,
    })
}

/// Decoded bytes of one inline part, produced a file buffer at a time.
pub struct InlineReader {
    reader: Reader<BufReader<io::Take<std::fs::File>>>,
    buf: Vec<u8>,
    out: Vec<u8>,
    at: usize,
    hex: Hex,
    expected: u64,
    done: bool,
}

impl InlineReader {
    /// Decodes the next text event into `out`; false once the part's text is exhausted.
    fn refill(&mut self) -> io::Result<bool> {
        let bad =
            |e: &dyn std::fmt::Display| io::Error::new(io::ErrorKind::InvalidData, e.to_string());
        self.out.clear();
        self.at = 0;
        while self.out.is_empty() {
            if self.done {
                return Ok(false);
            }
            let raw = self.reader.get_mut();
            let buf = raw.fill_buf()?;
            let n = buf
                .iter()
                .position(|&b| b == b'<' || b == b'&')
                .unwrap_or(buf.len());
            if n > 0 {
                self.hex.feed(&buf[..n], Some(&mut self.out));
                raw.consume(n);
                continue;
            }
            self.buf.clear();
            let event = self
                .reader
                .read_event_into(&mut self.buf)
                .map_err(|e| bad(&e))?;
            let chunk: std::borrow::Cow<'_, str> = match event {
                Event::Text(t) => t.decode().map_err(|e| bad(&e))?,
                Event::CData(c) => c.decode().map_err(|e| bad(&e))?,
                Event::GeneralRef(r) => resolve_ref(&r).map_err(|e| bad(&e))?.into(),
                Event::Eof => {
                    self.done = true;
                    self.hex.finish(Some(&mut self.out));
                    if self.hex.bad || self.hex.len != self.expected {
                        return Err(bad(&"inline part data changed since the MRA was read"));
                    }
                    continue;
                }
                _ => continue,
            };
            self.hex.feed(chunk.as_bytes(), Some(&mut self.out));
        }
        Ok(true)
    }
}

impl Read for InlineReader {
    fn read(&mut self, into: &mut [u8]) -> io::Result<usize> {
        if self.at == self.out.len() && !self.refill()? {
            return Ok(0);
        }
        let n = into.len().min(self.out.len() - self.at);
        into[..n].copy_from_slice(&self.out[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

/// Incremental form of [`hex_bytes`]: the same result however the text is split.
#[derive(Debug, Default)]
struct Hex {
    high: Option<u8>,
    len: u64,
    bad: bool,
}

impl Hex {
    /// Decodes `text`, appending bytes to `out` when given and counting them either way;
    /// any byte outside ASCII hex digits and separators marks the text bad.
    fn feed(&mut self, text: &[u8], mut out: Option<&mut Vec<u8>>) {
        let digit = |c: u8| {
            char::from(c)
                .to_digit(16)
                .and_then(|d| u8::try_from(d).ok())
        };
        for &c in text {
            if self.bad {
                return;
            }
            match (self.high, digit(c)) {
                (None, _) if matches!(c, b' ' | b',' | b'\t' | b'\n' | b'\r') => {}
                (None, Some(d)) => self.high = Some(d),
                (Some(h), Some(d)) => {
                    self.high = None;
                    self.len += 1;
                    if let Some(out) = out.as_deref_mut() {
                        out.push(h << 4 | d);
                    }
                }
                _ => self.bad = true,
            }
        }
    }

    /// Emits a lone final digit as one byte, as MiSTer does.
    fn finish(&mut self, out: Option<&mut Vec<u8>>) {
        if let Some(h) = self.high.take() {
            self.len += 1;
            if let Some(out) = out {
                out.push(h);
            }
        }
    }
}

/// Zips from `mra` that are not present in `mame_dir`, compared case-insensitively.
///
/// ```
/// use mistarr_mister::adapter::arcade::mra::{missing_zips, Mra};
/// let mra = Mra { zips: vec!["exblast.zip".into()], ..Mra::default() };
/// assert_eq!(missing_zips(&mra, std::path::Path::new("/nonexistent")), ["exblast.zip"]);
/// ```
#[must_use]
pub fn missing_zips(mra: &Mra, mame_dir: &Path) -> Vec<String> {
    let present: Vec<String> = std::fs::read_dir(mame_dir)
        .map(|rd| {
            rd.filter_map(std::result::Result::ok)
                .filter_map(|e| e.file_name().to_str().map(str::to_ascii_lowercase))
                .collect()
        })
        .unwrap_or_default();
    mra.zips
        .iter()
        .filter(|z| !present.contains(&z.to_ascii_lowercase()))
        .cloned()
        .collect()
}

/// Decodes inline hex the way MiSTer does: adjacent digit pairs, optionally separated
/// by spaces, commas, tabs and newlines; a lone digit at the very end is one byte.
/// `None` on any other text, including a digit pair split by a separator.
///
/// ```
/// use mistarr_mister::adapter::arcade::mra::hex_bytes;
/// assert_eq!(hex_bytes("00 ff,1A\n2"), Some(vec![0x00, 0xff, 0x1a, 0x02]));
/// assert_eq!(hex_bytes("zz"), None);
/// assert_eq!(hex_bytes("0 1"), None);
/// ```
#[must_use]
pub fn hex_bytes(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 2);
    let mut hex = Hex::default();
    hex.feed(text.as_bytes(), Some(&mut out));
    hex.finish(Some(&mut out));
    (!hex.bad).then_some(out)
}

/// Reads a number the way `strtoul(value, NULL, 0)` does: `0x` hex, leading-zero octal, else decimal.
fn number(value: &str) -> Option<u64> {
    let v = value.trim();
    if let Some(hex) = v.strip_prefix("0x").or_else(|| v.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()
    } else if v.len() > 1 && v.starts_with('0') {
        u64::from_str_radix(&v[1..], 8).ok()
    } else {
        v.parse().ok()
    }
}

fn split_zips(value: &str) -> Vec<String> {
    value
        .split('|')
        .map(str::trim)
        .filter(|z| !z.is_empty())
        .map(str::to_owned)
        .collect()
}

fn valid_md5(value: &str) -> Option<String> {
    let md5 = value.trim().to_ascii_lowercase();
    (md5.len() == 32 && md5.bytes().all(|b| b.is_ascii_hexdigit())).then_some(md5)
}

/// A `<rom>` being read, with the element open inside it.
struct RomBuilder {
    rom: MraRom,
    interleave: Option<Interleave>,
    open: Option<Open>,
    /// Depth inside an element the assembler does not implement.
    skip: usize,
}

/// A `<part>` or `<patch>` whose text is still arriving.
enum Open {
    /// A part, its hex so far, why it is refused, and where its content starts.
    Part(Part, Hex, Option<String>, u64),
    Patch(Patch, String, Option<String>),
}

fn attrs(e: &BytesStart<'_>) -> Result<Vec<(String, String)>> {
    e.attributes()
        .map(|a| {
            let a = a.map_err(xml_err)?;
            let key = String::from_utf8_lossy(a.key.local_name().as_ref()).to_ascii_lowercase();
            let value = a
                .normalized_value(XmlVersion::Implicit1_0)
                .map_err(xml_err)?;
            Ok((key, value.into_owned()))
        })
        .collect()
}

fn tag(name: &[u8]) -> String {
    String::from_utf8_lossy(name).to_ascii_lowercase()
}

fn start(e: &BytesStart<'_>, rom: &mut Option<RomBuilder>, after: u64) -> Result<()> {
    let name = tag(e.local_name().as_ref());
    let Some(b) = rom else {
        if name == "rom" {
            let mut r = MraRom::default();
            for (k, v) in attrs(e)? {
                match k.as_str() {
                    "zip" => r.zips = split_zips(&v),
                    "md5" => r.md5 = valid_md5(&v),
                    "index" => r.index = v.trim().parse().unwrap_or(0),
                    _ => {}
                }
            }
            *rom = Some(RomBuilder {
                rom: r,
                interleave: None,
                open: None,
                skip: 0,
            });
        }
        return Ok(());
    };
    if b.skip > 0 {
        b.skip += 1;
        return Ok(());
    }
    match (name.as_str(), &b.open) {
        ("part", None) => {
            let (part, err) = part_from(&attrs(e)?);
            b.open = Some(Open::Part(part, Hex::default(), err, after));
        }
        ("patch", None) if b.interleave.is_none() => {
            let mut patch = Patch::default();
            let mut err = None;
            for (k, v) in attrs(e)? {
                match k.as_str() {
                    "offset" => match number(&v) {
                        Some(n) => patch.offset = n,
                        None => err = Some(format!("patch offset {v:?} is not a number")),
                    },
                    "operation" => patch.xor = v.trim().eq_ignore_ascii_case("xor"),
                    _ => {}
                }
            }
            b.open = Some(Open::Patch(patch, String::new(), err));
        }
        ("interleave", None) if b.interleave.is_none() => {
            let mut il = Interleave {
                input: 8,
                ..Interleave::default()
            };
            for (k, v) in attrs(e)? {
                let n = u32::try_from(number(&v).unwrap_or(0)).unwrap_or(0);
                match k.as_str() {
                    "input" => il.input = n,
                    "output" => il.output = n,
                    _ => {}
                }
            }
            b.interleave = Some(il);
        }
        _ => {
            b.rom
                .items
                .push(RomItem::Unsupported(format!("<{name}> inside <rom>")));
            b.skip = 1;
        }
    }
    Ok(())
}

fn part_from(attrs: &[(String, String)]) -> (Part, Option<String>) {
    let mut part = Part::default();
    let mut err = None;
    for (k, v) in attrs {
        let mut num = |slot: &mut u64| match number(v) {
            Some(n) => *slot = n,
            None => err = Some(format!("part {k} {v:?} is not a number")),
        };
        match k.as_str() {
            "name" => part.name = Some(v.clone()).filter(|n| !n.is_empty()),
            "zip" => part.zips = split_zips(v),
            "crc" => part.crc = u32::from_str_radix(v.trim(), 16).ok(),
            "offset" => num(&mut part.offset),
            "repeat" => num(&mut part.repeat),
            "length" => {
                let mut n = 0;
                num(&mut n);
                part.length = (n > 0).then_some(n);
            }
            "map" => part.map = Some(v.trim().to_owned()),
            _ => {}
        }
    }
    (part, err)
}

/// Adds text to the open field and to the open part or patch; `keep` stores inline bytes.
fn text(t: &str, field: Option<Field>, mra: &mut Mra, rom: &mut Option<RomBuilder>, keep: bool) {
    if let Some(f) = field {
        f.append(mra, t);
    }
    match rom {
        Some(RomBuilder {
            open: Some(Open::Part(part, hex, None, _)),
            skip: 0,
            ..
        }) if part.name.is_none() => hex.feed(t.as_bytes(), keep.then_some(&mut part.data)),
        Some(RomBuilder {
            open: Some(Open::Patch(_, buf, _)),
            skip: 0,
            ..
        }) => buf.push_str(t),
        _ => {}
    }
}

/// Closes element `name`; `pos` is where its content ends in the input.
fn end(name: &str, rom: &mut Option<RomBuilder>, mra: &mut Mra, pos: u64, at: &Pos<'_>) {
    let Some(b) = rom else {
        return;
    };
    if b.skip > 0 {
        b.skip -= 1;
        return;
    }
    match name {
        "part" => {
            let Some(Open::Part(mut part, mut hex, err, start)) = b.open.take() else {
                return;
            };
            hex.finish(Some(&mut part.data));
            let item = match (err, &part.name) {
                (Some(e), _) => Err(e),
                (None, Some(_)) => Ok(part),
                (None, None) if hex.bad => Err("inline part data is not hex".to_owned()),
                (None, None) => {
                    if let Some(file) = at.file.filter(|_| hex.len > 0) {
                        part.data = Vec::new();
                        part.inline = Some(Inline {
                            file: Arc::clone(file),
                            start,
                            end: pos,
                            len: hex.len,
                        });
                    }
                    Ok(part)
                }
            };
            match (item, &mut b.interleave) {
                (Ok(p), Some(il)) => il.parts.push(p),
                (Ok(p), None) => b.rom.items.push(RomItem::Part(p)),
                (Err(e), _) => b.rom.items.push(RomItem::Unsupported(e)),
            }
        }
        "patch" => {
            let Some(Open::Patch(mut patch, buf, err)) = b.open.take() else {
                return;
            };
            let item = match (err, hex_bytes(&buf)) {
                (Some(e), _) => RomItem::Unsupported(e),
                (None, None) => RomItem::Unsupported("patch data is not hex".to_owned()),
                (None, Some(data)) => {
                    patch.data = data;
                    RomItem::Patch(patch)
                }
            };
            b.rom.items.push(item);
        }
        "interleave" => {
            if let Some(il) = b.interleave.take() {
                b.rom.items.push(RomItem::Interleave(il));
            }
        }
        "rom" => {
            if let Some(b) = rom.take() {
                mra.roms.push(b.rom);
            }
        }
        _ => {}
    }
}

#[derive(Debug, Clone, Copy)]
enum Field {
    Name,
    Setname,
    Rbf,
}

impl Field {
    /// The field a lowercase tag name fills.
    fn of(tag: &str) -> Option<Self> {
        match tag {
            "name" => Some(Self::Name),
            "setname" => Some(Self::Setname),
            "rbf" => Some(Self::Rbf),
            _ => None,
        }
    }

    /// Appends `text` up to [`MAX_FIELD_BYTES`], so an unclosed field cannot take in the document.
    fn append(self, mra: &mut Mra, text: &str) {
        let slot = match self {
            Self::Name => &mut mra.name,
            Self::Setname => &mut mra.setname,
            Self::Rbf => &mut mra.rbf,
        }
        .get_or_insert_with(String::new);
        let room = MAX_FIELD_BYTES.saturating_sub(slot.len());
        let mut end = room.min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        slot.push_str(&text[..end]);
    }
}

fn read_attributes(e: &BytesStart<'_>, mra: &mut Mra) -> Result<()> {
    let is_rom = e.local_name().as_ref().eq_ignore_ascii_case(b"rom");
    for (key, value) in attrs(e)? {
        match key.as_str() {
            "zip" => {
                for zip in split_zips(&value) {
                    if !mra.zips.contains(&zip) {
                        mra.zips.push(zip);
                    }
                }
            }
            "md5" if is_rom => mra.md5.extend(valid_md5(&value)),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
