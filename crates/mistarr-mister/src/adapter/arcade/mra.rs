//! MRA parsing: which MAME zips an arcade core definition needs.

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
}

/// Parses MRA XML.
///
/// Text of `<name>`, `<setname>` and `<rbf>` includes CDATA, resolved entities
/// and the text of any nested child elements, so `<name>A<b>x</b>B</name>` reads `AxB`.
///
/// # Errors
///
/// Returns [`Error::Mra`] when the document is not well-formed or ends with open elements.
///
/// ```
/// let mra = mistarr_mister::adapter::arcade::mra::parse(
///     br#"<misterromdescription><setname>exblast</setname>
///         <rom index="0" zip="exblast.zip|exparent.zip"/></misterromdescription>"#,
/// ).unwrap();
/// assert_eq!(mra.zips, ["exblast.zip", "exparent.zip"]);
/// ```
pub fn parse(xml: &[u8]) -> Result<Mra> {
    let mut reader = Reader::from_reader(xml);
    let mut mra = Mra::default();
    let mut open: Vec<Option<Field>> = Vec::new();
    loop {
        let event = reader.read_event().map_err(xml_err)?;
        let field = open.last().copied().flatten();
        match event {
            Event::Start(e) => {
                open.push(Field::of(e.local_name().as_ref()).or(field));
                read_attributes(&e, &mut mra)?;
            }
            Event::Empty(e) => read_attributes(&e, &mut mra)?,
            Event::Text(t) => {
                if let Some(f) = field {
                    f.append(&mut mra, &t.decode().map_err(xml_err)?);
                }
            }
            Event::CData(c) => {
                if let Some(f) = field {
                    f.append(&mut mra, &c.decode().map_err(xml_err)?);
                }
            }
            Event::GeneralRef(r) => {
                let resolved = if let Some(c) = r.resolve_char_ref().map_err(xml_err)? {
                    c.to_string()
                } else {
                    let name = r.decode().map_err(xml_err)?;
                    resolve_predefined_entity(&name)
                        .ok_or_else(|| Error::Mra(format!("unknown entity `{name}`")))?
                        .to_owned()
                };
                if let Some(f) = field {
                    f.append(&mut mra, &resolved);
                }
            }
            Event::End(_) => {
                open.pop();
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

/// Reads and parses an MRA file.
///
/// # Errors
///
/// Returns [`Error::Io`] when the file cannot be read and [`Error::Mra`] when it is not well-formed.
///
/// ```
/// let path = std::env::temp_dir().join("mistarr-doc-example.mra");
/// std::fs::write(&path, r#"<misterromdescription><rom zip="exblast.zip"/></misterromdescription>"#).unwrap();
/// assert_eq!(mistarr_mister::adapter::arcade::mra::read(&path).unwrap().zips, ["exblast.zip"]);
/// ```
pub fn read(path: &Path) -> Result<Mra> {
    parse(&std::fs::read(path)?)
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

#[derive(Debug, Clone, Copy)]
enum Field {
    Name,
    Setname,
    Rbf,
}

impl Field {
    fn of(tag: &[u8]) -> Option<Self> {
        match tag {
            b"name" => Some(Self::Name),
            b"setname" => Some(Self::Setname),
            b"rbf" => Some(Self::Rbf),
            _ => None,
        }
    }

    fn append(self, mra: &mut Mra, text: &str) {
        let slot = match self {
            Self::Name => &mut mra.name,
            Self::Setname => &mut mra.setname,
            Self::Rbf => &mut mra.rbf,
        };
        slot.get_or_insert_with(String::new).push_str(text);
    }
}

fn read_attributes(e: &BytesStart<'_>, mra: &mut Mra) -> Result<()> {
    let is_rom = e.local_name().as_ref() == b"rom";
    for attr in e.attributes() {
        let attr = attr.map_err(|e| Error::Mra(e.to_string()))?;
        let key = attr.key.local_name();
        let value = || {
            attr.normalized_value(XmlVersion::Implicit1_0)
                .map_err(|e| Error::Mra(e.to_string()))
        };
        match key.as_ref() {
            b"zip" => {
                for zip in value()?.split('|').map(str::trim).filter(|z| !z.is_empty()) {
                    if !mra.zips.iter().any(|z2| z2 == zip) {
                        mra.zips.push(zip.to_owned());
                    }
                }
            }
            b"md5" if is_rom => {
                let md5 = value()?.trim().to_ascii_lowercase();
                if md5.len() == 32 && md5.bytes().all(|b| b.is_ascii_hexdigit()) {
                    mra.md5.push(md5);
                }
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<misterromdescription>
  <name>Example Blaster &amp; Friends</name>
  <setname>exblast</setname>
  <rbf>examplecore</rbf>
  <rom index="0" zip="exblast.zip|exparent.zip" md5="0123456789ABCDEF0123456789abcdef" type="merged">
    <part zip="exsound.zip" name="snd.bin" crc="00000000"/>
    <part name="cpu.bin" crc="11111111"/>
    <part zip="exblast.zip" name="gfx.bin"/>
  </rom>
  <rom index="1" md5="None"><part>00 FF</part></rom>
  <rom index="2" zip=" " md5="short"/>
</misterromdescription>"#;

    #[test]
    fn parses_synthetic_mra() {
        let mra = parse(SAMPLE.as_bytes()).expect("parse");
        assert_eq!(mra.name.as_deref(), Some("Example Blaster & Friends"));
        assert_eq!(mra.setname.as_deref(), Some("exblast"));
        assert_eq!(mra.rbf.as_deref(), Some("examplecore"));
        assert_eq!(mra.zips, ["exblast.zip", "exparent.zip", "exsound.zip"]);
        assert_eq!(mra.md5, ["0123456789abcdef0123456789abcdef"]);
    }

    #[test]
    fn malformed_xml_is_an_error() {
        assert!(matches!(
            parse(b"<misterromdescription><rom zip=\"a.zip\"></oops>"),
            Err(Error::Mra(_))
        ));
    }

    #[test]
    fn cdata_and_character_references_are_kept() {
        let mra = parse(
            b"<misterromdescription><name>Example &#38; Co &#x26; Blaster</name>\
              <setname><![CDATA[ex<blast>]]></setname></misterromdescription>",
        )
        .expect("parse");
        assert_eq!(mra.name.as_deref(), Some("Example & Co & Blaster"));
        assert_eq!(mra.setname.as_deref(), Some("ex<blast>"));
    }

    #[test]
    fn nested_child_text_stays_in_the_enclosing_field() {
        let mra = parse(
            b"<misterromdescription><name>A<b>x</b>B</name><rbf>c</rbf></misterromdescription>",
        )
        .expect("parse");
        assert_eq!(mra.name.as_deref(), Some("AxB"));
        assert_eq!(mra.rbf.as_deref(), Some("c"));
    }

    #[test]
    fn truncated_document_is_an_error() {
        let err = parse(b"<misterromdescription><rom zip=\"exblast.zip\"><part name=\"a\"/>");
        assert!(matches!(err, Err(Error::Mra(_))));
    }

    #[test]
    fn unknown_entity_is_an_error() {
        assert!(matches!(
            parse(b"<m><name>&bogus;</name></m>"),
            Err(Error::Mra(_))
        ));
    }

    #[test]
    fn empty_document_has_no_zips() {
        assert_eq!(
            parse(b"<misterromdescription/>").expect("parse"),
            Mra::default()
        );
    }

    #[test]
    fn read_and_missing_zips() {
        let dir = crate::adapter::testutil::scratch("mra");
        let path = dir.join("Example Blaster.mra");
        std::fs::write(&path, SAMPLE).expect("write");
        let mra = read(&path).expect("read");
        std::fs::write(dir.join("EXBLAST.ZIP"), b"").expect("write");
        assert_eq!(missing_zips(&mra, &dir), ["exparent.zip", "exsound.zip"]);
        assert!(matches!(read(&dir.join("absent.mra")), Err(Error::Io(_))));
    }
}
