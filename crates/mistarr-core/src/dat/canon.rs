//! mistarr's own rewrite of a DAT from what the parser reads; see `docs/ARCHITECTURE.md` "Fetching a URL".

use std::io::{BufRead, Write};

use super::export::{Archive, File, Source};
use super::{
    DatError, DatFormat, DatGame, DatHeader, DatRom, DatStream, ExportOptions, Mode, RomStatus,
};

/// What [`rewrite`] wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rewritten {
    /// The form of the input, kept in the output.
    pub format: DatFormat,
    /// Games written.
    pub games: usize,
}

/// Why [`rewrite`] failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RewriteError {
    /// The input is not a DAT the parser accepts.
    #[error(transparent)]
    Dat(#[from] DatError),
    /// The output could not be written.
    #[error("the rewritten DAT cannot be written: {0}")]
    Write(#[from] std::io::Error),
}

/// Parses the DAT `reader` holds and writes to `out` only what the parser read, in its
/// own form: the header's fields, and each game with the attributes, releases and roms
/// the parser keeps, or for a DB export its archive and every file of its sources.
/// Comments, processing instructions, doctypes, CDATA sections, unknown elements and
/// stray text are not carried over. One game is held at a time.
///
/// ```
/// let xml = br#"<!-- note --><datafile><header><name>Example System</name></header>
///   <game name="Example Quest (World)"><payload>anything</payload>
///   <rom name="q.bin" size="4" crc="0A0B0C0D"/></game></datafile>"#;
/// let mut out = Vec::new();
/// let done = mistarr_core::dat::rewrite(&xml[..], &mut out)?;
/// assert_eq!(done.games, 1);
/// let text = String::from_utf8(out)?;
/// assert!(!text.contains("note") && !text.contains("payload"));
/// assert_eq!(mistarr_core::dat::parse_dat(text.as_bytes())?, mistarr_core::dat::parse_dat(xml)?);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// # Errors
/// [`RewriteError::Dat`] for anything [`DatStream`] refuses, [`RewriteError::Write`] when
/// `out` fails; `out` then holds a partial document.
pub fn rewrite<R: BufRead, W: Write>(reader: R, out: W) -> Result<Rewritten, RewriteError> {
    let mut stream = DatStream::open(reader, ExportOptions::default(), Mode::Raw)?;
    let format = stream.format();
    let first = stream.header().clone();
    let mut w = Out { out };
    w.put("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n")?;
    match format {
        DatFormat::DbExport => {
            write_header(&mut w, &first, false)?;
            w.put("<datafile>\n")?;
        }
        DatFormat::Logiqx => {
            w.put("<datafile>\n")?;
            write_header(&mut w, &first, false)?;
        }
    }
    let mut games = 0usize;
    while let Some((game, _)) = stream.pull()? {
        match stream.raw.take() {
            Some((archive, sources)) => write_export_game(&mut w, &game.name, &archive, &sources)?,
            None => write_game(&mut w, &game)?,
        }
        games += 1;
    }
    if games == 0 {
        return Err(DatError::NoGames.into());
    }
    if stream.header() != &first {
        let reset =
            first.clrmamepro_header.is_some() && stream.header().clrmamepro_header.is_none();
        write_header(&mut w, stream.header(), reset)?;
    }
    w.put("</datafile>\n")?;
    w.out.flush()?;
    Ok(Rewritten { format, games })
}

struct Out<W> {
    out: W,
}

impl<W: Write> Out<W> {
    fn put(&mut self, s: &str) -> std::io::Result<()> {
        self.out.write_all(s.as_bytes())
    }

    /// Writes `value` as element text or, with `attr`, as a quoted attribute value.
    fn escaped(&mut self, value: &str, attr: bool) -> std::io::Result<()> {
        let mut rest = value;
        while let Some(at) = rest.find(|c: char| needs_ref(c, attr)) {
            self.put(&rest[..at])?;
            let c = rest[at..].chars().next().unwrap_or(' ');
            match c {
                '&' => self.put("&amp;")?,
                '<' => self.put("&lt;")?,
                '>' => self.put("&gt;")?,
                '"' => self.put("&quot;")?,
                other => self.put(&format!("&#{};", u32::from(other)))?,
            }
            rest = &rest[at + c.len_utf8()..];
        }
        self.put(rest)
    }

    fn attr(&mut self, key: &str, value: Option<&str>) -> std::io::Result<()> {
        if let Some(v) = value {
            self.put(" ")?;
            self.put(key)?;
            self.put("=\"")?;
            self.escaped(v, true)?;
            self.put("\"")?;
        }
        Ok(())
    }

    fn element(&mut self, indent: &str, name: &str, text: Option<&str>) -> std::io::Result<()> {
        if let Some(t) = text {
            self.put(indent)?;
            self.put(&format!("<{name}>"))?;
            self.escaped(t, false)?;
            self.put(&format!("</{name}>\n"))?;
        }
        Ok(())
    }
}

/// Whether `c` is written as a reference: markup, and the line breaks and tabs the
/// parser would otherwise normalise, in attributes all three and in text `\r`.
fn needs_ref(c: char, attr: bool) -> bool {
    match c {
        '&' | '<' | '>' | '\r' => true,
        '"' | '\t' | '\n' => attr,
        _ => false,
    }
}

/// Writes every header field; `reset` adds an empty `<clrmamepro/>` that clears a rule an
/// earlier header set.
fn write_header<W: Write>(w: &mut Out<W>, h: &DatHeader, reset: bool) -> std::io::Result<()> {
    w.put("<header>\n")?;
    w.element("  ", "name", Some(&h.name))?;
    w.element("  ", "description", Some(&h.description))?;
    w.element("  ", "version", Some(&h.version))?;
    w.element("  ", "date", h.date.as_deref())?;
    w.element("  ", "author", h.author.as_deref())?;
    w.element("  ", "homepage", h.homepage.as_deref())?;
    w.element("  ", "url", h.url.as_deref())?;
    w.element("  ", "comment", h.comment.as_deref())?;
    if h.clrmamepro_header.is_some() || reset {
        w.put("  <clrmamepro")?;
        w.attr("header", h.clrmamepro_header.as_deref())?;
        w.put("/>\n")?;
    }
    w.put("</header>\n")
}

fn write_game<W: Write>(w: &mut Out<W>, g: &DatGame) -> std::io::Result<()> {
    w.put("<game")?;
    w.attr("name", Some(&g.name))?;
    w.attr("cloneof", g.clone_of.as_deref())?;
    w.attr("romof", g.rom_of.as_deref())?;
    w.put(">\n")?;
    w.element("  ", "description", g.description.as_deref())?;
    w.element("  ", "category", g.category.as_deref())?;
    if !g.regions.is_empty() || !g.languages.is_empty() {
        let regions = g.regions.join(",");
        let languages = g.languages.join(",");
        w.put("  <release")?;
        w.attr(
            "region",
            Some(&regions).filter(|r| !r.is_empty()).map(String::as_str),
        )?;
        w.attr(
            "language",
            Some(&languages)
                .filter(|l| !l.is_empty())
                .map(String::as_str),
        )?;
        w.put("/>\n")?;
    }
    for rom in &g.roms {
        write_rom(w, rom)?;
    }
    w.put("</game>\n")
}

fn write_rom<W: Write>(w: &mut Out<W>, r: &DatRom) -> std::io::Result<()> {
    w.put("  <rom")?;
    w.attr("name", Some(&r.name))?;
    w.attr("size", Some(&r.size.to_string()))?;
    w.attr("crc", r.crc32.as_deref())?;
    w.attr("md5", r.md5.as_deref())?;
    w.attr("sha1", r.sha1.as_deref())?;
    if r.status != RomStatus::Good {
        w.attr("status", Some(r.status.as_str()))?;
    }
    w.attr("header", r.header.as_deref())?;
    w.put("/>\n")
}

fn write_export_game<W: Write>(
    w: &mut Out<W>,
    name: &str,
    archive: &Archive,
    sources: &[Source],
) -> std::io::Result<()> {
    w.put("<game")?;
    w.attr("name", Some(name))?;
    w.put(">\n")?;
    if archive != &Archive::default() {
        w.put("  <archive")?;
        w.attr("number", archive.number.as_deref())?;
        w.attr("clone", archive.clone.as_deref())?;
        w.attr("region", archive.region.as_deref())?;
        w.attr("languages", archive.languages.as_deref())?;
        w.attr("status", archive.status.as_deref())?;
        w.put("/>\n")?;
    }
    for source in sources {
        w.put("  <source>\n")?;
        for file in &source.files {
            write_file(w, file)?;
        }
        w.put("  </source>\n")?;
    }
    w.put("</game>\n")
}

fn nonempty(s: &str) -> Option<&str> {
    (!s.is_empty()).then_some(s)
}

fn write_file<W: Write>(w: &mut Out<W>, f: &File) -> std::io::Result<()> {
    w.put("    <file")?;
    w.attr("extension", nonempty(&f.extension))?;
    w.attr("format", nonempty(&f.format))?;
    w.attr("size", Some(&f.size.to_string()))?;
    w.attr("crc32", f.crc32.as_deref())?;
    w.attr("md5", f.md5.as_deref())?;
    w.attr("sha1", f.sha1.as_deref())?;
    w.attr("header", f.header.as_deref())?;
    w.attr("item", f.item.as_deref())?;
    w.attr("forcename", f.forcename.as_deref())?;
    if f.bad {
        w.attr("bad", Some("1"))?;
    }
    w.put("/>\n")
}

#[cfg(test)]
mod tests;
