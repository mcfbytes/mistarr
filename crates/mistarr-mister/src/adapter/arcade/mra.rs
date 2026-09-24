//! MRA parsing: which MAME zips an arcade core definition needs and how its roms are built.

use std::io::Read as _;
use std::path::Path;

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
    /// Inline bytes of an unnamed part.
    pub data: Vec<u8>,
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
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().check_end_names = false;
    let mut mra = Mra::default();
    let mut open: Vec<(String, Option<Field>)> = Vec::new();
    let mut rom: Option<RomBuilder> = None;
    loop {
        let event = reader.read_event().map_err(xml_err)?;
        let field = open.last().and_then(|(_, f)| *f);
        match event {
            Event::Start(e) => {
                let name = tag(e.local_name().as_ref());
                // A `<rom>` inside an unclosed field never feeds that field.
                let f = Field::of(&name).or(field.filter(|_| name != "rom"));
                open.push((name, f));
                read_attributes(&e, &mut mra)?;
                start(&e, &mut rom)?;
            }
            Event::Empty(e) => {
                read_attributes(&e, &mut mra)?;
                start(&e, &mut rom)?;
                end(&tag(e.local_name().as_ref()), &mut rom, &mut mra);
            }
            Event::Text(t) => text(&t.decode().map_err(xml_err)?, field, &mut mra, &mut rom),
            Event::CData(c) => text(&c.decode().map_err(xml_err)?, field, &mut mra, &mut rom),
            Event::GeneralRef(r) => {
                let resolved = if let Some(c) = r.resolve_char_ref().map_err(xml_err)? {
                    c.to_string()
                } else {
                    let name = r.decode().map_err(xml_err)?;
                    resolve_predefined_entity(&name)
                        .map_or_else(|| format!("&{name};"), str::to_owned)
                };
                text(&resolved, field, &mut mra, &mut rom);
            }
            Event::End(e) => {
                let name = tag(e.local_name().as_ref());
                if let Some(at) = open.iter().rposition(|(n, _)| *n == name) {
                    for (closed, _) in open.drain(at..).rev() {
                        end(&closed, &mut rom, &mut mra);
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

fn xml_err(e: impl std::fmt::Display) -> Error {
    Error::Mra(e.to_string())
}

/// Largest MRA file read; real ones are a few KiB, so a bigger one is refused.
pub const MAX_MRA_BYTES: u64 = 1024 * 1024;

/// Longest `<name>`, `<setname>` or `<rbf>` text kept, in bytes; the rest is dropped.
pub const MAX_FIELD_BYTES: usize = 256;

/// Version of what [`parse`] reads from an MRA; it changes whenever a file could parse differently.
pub const PARSER_VERSION: u32 = 1;

/// Reads and parses an MRA file of at most [`MAX_MRA_BYTES`].
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
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_MRA_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_MRA_BYTES {
        return Err(Error::Mra(format!(
            "file is larger than {MAX_MRA_BYTES} bytes"
        )));
    }
    parse(&bytes)
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
    let digit = |c: char| c.to_digit(16).and_then(|d| u8::try_from(d).ok());
    let mut out = Vec::with_capacity(text.len() / 2);
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if matches!(c, ' ' | ',' | '\t' | '\n' | '\r') {
            continue;
        }
        let high = digit(c)?;
        match chars.next() {
            None => out.push(high),
            Some(c) => out.push(high << 4 | digit(c)?),
        }
    }
    Some(out)
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
    Part(Part, String, Option<String>),
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

fn start(e: &BytesStart<'_>, rom: &mut Option<RomBuilder>) -> Result<()> {
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
            b.open = Some(Open::Part(part, String::new(), err));
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

fn text(t: &str, field: Option<Field>, mra: &mut Mra, rom: &mut Option<RomBuilder>) {
    if let Some(f) = field {
        f.append(mra, t);
    }
    if let Some(RomBuilder {
        open: Some(Open::Part(_, buf, _) | Open::Patch(_, buf, _)),
        skip: 0,
        ..
    }) = rom
    {
        buf.push_str(t);
    }
}

fn end(name: &str, rom: &mut Option<RomBuilder>, mra: &mut Mra) {
    let Some(b) = rom else {
        return;
    };
    if b.skip > 0 {
        b.skip -= 1;
        return;
    }
    match name {
        "part" => {
            let Some(Open::Part(mut part, buf, err)) = b.open.take() else {
                return;
            };
            let item = match (err, &part.name) {
                (Some(e), _) => Err(e),
                (None, Some(_)) => Ok(part),
                (None, None) => match hex_bytes(&buf) {
                    Some(data) => {
                        part.data = data;
                        Ok(part)
                    }
                    None => Err("inline part data is not hex".to_owned()),
                },
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
