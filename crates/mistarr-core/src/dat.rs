//! Streaming DAT parsing, Logiqx and No-Intro DB export; the contract is `docs/VERIFICATION.md` "DAT parsing".

use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Seek};

use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::{BytesRef, BytesStart, Event};
use quick_xml::{Reader, XmlVersion};
use serde::{Deserialize, Serialize};

use crate::hash::HeaderRule;
use crate::xml::{check_utf8, lossy, Capped, EscapeInvalid};

mod canon;
mod export;
mod family;

pub use canon::{rewrite, RewriteError, Rewritten};
pub use export::{export_name, ExportName};
pub use family::{family_key, split_version, version_order, DatFamily, FORMAT_MARKERS};

#[cfg(test)]
mod tests;

/// Failure to read a DAT or a DAT pack.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DatError {
    /// The XML is malformed or could not be read.
    #[error("invalid XML at byte {position}: {source}")]
    Xml {
        /// Byte offset where the error was detected.
        position: u64,
        /// Underlying parser error.
        source: quick_xml::Error,
    },
    /// The document is neither a Logiqx DAT nor a DB export.
    #[error(
        "root element is <{root}>; expected a Logiqx DAT (<datafile>) \
         or a No-Intro DB export (<header> followed by <datafile>)"
    )]
    NotDatafile {
        /// Name of the root element found.
        root: String,
    },
    /// The document ended before `</datafile>`.
    #[error("file ends before </datafile>")]
    Truncated,
    /// The DAT has no `game` entries.
    #[error("DAT contains no games")]
    NoGames,
    /// A required attribute is missing.
    #[error("<{element}> in game {game:?} has no {attribute} attribute")]
    MissingAttribute {
        /// Element that lacks the attribute.
        element: &'static str,
        /// Attribute name.
        attribute: &'static str,
        /// Enclosing game name, empty when the game itself has no name.
        game: String,
    },
    /// An attribute value is not in the expected form.
    #[error("game {game:?} has invalid {attribute} value {value:?}")]
    InvalidAttribute {
        /// Enclosing game name.
        game: String,
        /// Attribute name.
        attribute: &'static str,
        /// The rejected value.
        value: String,
    },
    /// One element, text run or comment is larger than [`MAX_EVENT_BYTES`].
    #[error("an XML element at byte {position} is larger than {} KiB", MAX_EVENT_BYTES >> 10)]
    EventTooLarge {
        /// Byte offset where the element starts.
        position: u64,
    },
    /// Something other than whitespace, comments or processing instructions follows
    /// the root element, such as another file appended to the DAT.
    #[error("data follows the end of the DAT at byte {position}")]
    TrailingData {
        /// Byte offset of the first such data.
        position: u64,
    },
    /// Elements nest deeper than [`MAX_DEPTH`].
    #[error("elements nest deeper than {MAX_DEPTH} levels at byte {position}")]
    TooDeep {
        /// Byte offset of the element that went too deep.
        position: u64,
    },
    /// One field, an attribute or an element's text, is longer than its cap:
    /// [`MAX_NAME_BYTES`] for names, [`MAX_FIELD_BYTES`] for the rest.
    #[error("the {field} at byte {position} is longer than {limit} bytes")]
    FieldTooLarge {
        /// The attribute or element.
        field: &'static str,
        /// Its cap.
        limit: usize,
        /// Byte offset near the field.
        position: u64,
    },
    /// A game has more than [`MAX_GAME_ENTRIES`] roms, releases and files together.
    #[error("game {game:?} has more than {MAX_GAME_ENTRIES} roms, releases or files")]
    TooManyEntries {
        /// The game.
        game: String,
    },
    /// A game's fields add up to more than [`MAX_GAME_BYTES`].
    #[error("game {game:?} holds more than {} MiB of fields", MAX_GAME_BYTES >> 20)]
    GameTooLarge {
        /// The game.
        game: String,
    },
    /// A DB export's parent index would exceed [`MAX_INDEX_BYTES`].
    #[error("the DB export's parent index is larger than {} MiB", MAX_INDEX_BYTES >> 20)]
    IndexTooLarge,
    /// The zip container could not be read.
    #[error("invalid zip archive: {0}")]
    Zip(#[from] zip::result::ZipError),
    /// A zip pack has no `.dat` or `.xml` members.
    #[error("zip archive contains no .dat or .xml files")]
    EmptyPack,
    /// A member of a zip pack failed to parse.
    #[error("{member}: {source}")]
    Member {
        /// Member path inside the archive.
        member: String,
        /// Why it failed.
        source: Box<DatError>,
    },
}

/// Largest single XML event, a tag with its attributes, a text run or a comment, that a
/// DAT may hold; larger ones fail with [`DatError::EventTooLarge`] before being buffered.
pub const MAX_EVENT_BYTES: u64 = 1024 * 1024;

/// Deepest element nesting a DAT may use, the document root at depth 1.
pub const MAX_DEPTH: usize = 64;

/// Longest name: a game's, a rom's, a parent reference or an archive number, and the header's.
pub const MAX_NAME_BYTES: usize = 4 * 1024;

/// Longest other field: an attribute or the text of one element.
pub const MAX_FIELD_BYTES: usize = 64 * 1024;

/// Most roms, releases and DB export files one game may hold together.
pub const MAX_GAME_ENTRIES: usize = 100_000;

/// Most bytes of fields one game may hold, so one game never grows memory without bound.
pub const MAX_GAME_BYTES: usize = 16 * 1024 * 1024;

/// Most bytes of names and archive numbers a DB export's parent index may hold.
pub const MAX_INDEX_BYTES: usize = 32 * 1024 * 1024;

/// Attributes capped at [`MAX_NAME_BYTES`]; any other at [`MAX_FIELD_BYTES`].
const NAME_KEYS: [&str; 6] = ["name", "cloneof", "romof", "forcename", "number", "clone"];
/// Header fields of a DAT. `name` and `version` are kept verbatim.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatHeader {
    /// `<name>`, used for platform binding.
    pub name: String,
    /// `<description>`.
    pub description: String,
    /// `<version>`, compared as a string for supersession.
    pub version: String,
    /// `<date>`.
    pub date: Option<String>,
    /// `<author>`.
    pub author: Option<String>,
    /// `<homepage>`.
    pub homepage: Option<String>,
    /// `<url>`.
    pub url: Option<String>,
    /// `<comment>`.
    pub comment: Option<String>,
    /// The `header` attribute of `<clrmamepro>`, naming a header-skip rule.
    pub clrmamepro_header: Option<String>,
}

/// Dump status of a rom entry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum RomStatus {
    /// `good`, the default when the attribute is absent.
    #[default]
    Good,
    /// `baddump`.
    BadDump,
    /// `nodump`: no known dump, hashes are usually absent.
    NoDump,
    /// `verified`.
    Verified,
}

impl RomStatus {
    /// The attribute spelling, as stored in `roms.status`.
    ///
    /// ```
    /// use mistarr_core::dat::RomStatus;
    /// assert_eq!(RomStatus::BadDump.as_str(), "baddump");
    /// ```
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            RomStatus::Good => "good",
            RomStatus::BadDump => "baddump",
            RomStatus::NoDump => "nodump",
            RomStatus::Verified => "verified",
        }
    }

    /// Parses an attribute value, ignoring ASCII case.
    ///
    /// ```
    /// use mistarr_core::dat::RomStatus;
    /// assert_eq!(RomStatus::parse("nodump"), Some(RomStatus::NoDump));
    /// assert_eq!(RomStatus::parse("great"), None);
    /// ```
    #[must_use]
    pub fn parse(value: &str) -> Option<RomStatus> {
        [
            RomStatus::Good,
            RomStatus::BadDump,
            RomStatus::NoDump,
            RomStatus::Verified,
        ]
        .into_iter()
        .find(|status| status.as_str().eq_ignore_ascii_case(value.trim()))
    }
}

/// One `<rom>` of a game. Hash fields are lowercase hex of the documented length.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatRom {
    /// File name, possibly with a subdirectory for disc images.
    pub name: String,
    /// Size in bytes.
    pub size: u64,
    /// CRC32, 8 hex characters.
    pub crc32: Option<String>,
    /// MD5, 32 hex characters.
    pub md5: Option<String>,
    /// SHA1, 40 hex characters.
    pub sha1: Option<String>,
    /// Dump status.
    pub status: RomStatus,
    /// The `header` attribute, verbatim.
    pub header: Option<String>,
}

/// One `<game>` (or `<machine>`) entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatGame {
    /// Full game name.
    pub name: String,
    /// Parent game name from `cloneof`.
    pub clone_of: Option<String>,
    /// `romof`.
    pub rom_of: Option<String>,
    /// `<description>`.
    pub description: Option<String>,
    /// First `<category>`.
    pub category: Option<String>,
    /// Regions the DAT states outside the name: `<release region>`, or a DB export's `archive@region`.
    pub regions: Vec<String>,
    /// Languages the DAT states outside the name: `<release language>`, or `archive@languages`.
    pub languages: Vec<String>,
    /// A DB export's `archive@status`, verbatim, such as `Proto 2`; see [`DatGame::status_flags`].
    pub status: Option<String>,
    /// Rom entries in document order.
    pub roms: Vec<DatRom>,
}

impl DatGame {
    /// The name flags `status` stands for: `beta`, `proto` (also "Possible Proto"), `demo`
    /// and `sample`, matched as words in any case.
    ///
    /// ```
    /// use mistarr_core::dat::DatGame;
    /// use mistarr_core::naming::Flag;
    /// let game = DatGame { name: "Example Quest (World)".into(), clone_of: None, rom_of: None,
    ///     description: None, category: None, regions: vec![], languages: vec![],
    ///     status: Some("Possible Proto".into()), roms: vec![] };
    /// assert_eq!(game.status_flags(), [Flag::Proto]);
    /// ```
    #[must_use]
    pub fn status_flags(&self) -> Vec<crate::naming::Flag> {
        use crate::naming::Flag;
        let Some(status) = &self.status else {
            return Vec::new();
        };
        let mut flags = Vec::new();
        for word in status.split(|c: char| !c.is_ascii_alphabetic()) {
            let flag = match word.to_ascii_lowercase().as_str() {
                "beta" => Flag::Beta,
                "proto" | "prototype" => Flag::Proto,
                "demo" => Flag::Demo,
                "sample" => Flag::Sample,
                _ => continue,
            };
            if !flags.contains(&flag) {
                flags.push(flag);
            }
        }
        flags
    }
}

/// The two input forms a DAT file may take, told apart by its first element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DatFormat {
    /// A Logiqx `<datafile>` document.
    Logiqx,
    /// A No-Intro database export: a `<header>` element followed by a `<datafile>` element.
    DbExport,
}

/// How a DB export's files become rom entries; ignored for Logiqx DATs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExportOptions {
    /// Header rule of the platform the DAT binds to, which decides the file format taken.
    pub header_rule: HeaderRule,
    /// The extension the platform writes, without the dot, naming files stored as `.unh`,
    /// with none, headerless, or taken as the only file of a game.
    pub extension: Option<String>,
    /// Extensions the platform loads, without the dot; a file with one of them is an image.
    pub load_extensions: Vec<String>,
    /// Archive number to game name, from [`export_parents`], resolving clone references.
    pub parents: HashMap<String, String>,
}

/// A fully parsed DAT with at least one game.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dat {
    /// Header fields.
    pub header: DatHeader,
    /// Games in document order.
    pub games: Vec<DatGame>,
}

/// Parses a DAT held in memory. A DB export is read twice, first for its parent index.
///
/// ```
/// let xml = br#"<datafile><header><name>Example System</name></header>
///   <game name="Example Quest (USA)"><rom name="q.bin" size="4" crc="0A0B0C0D"/></game>
/// </datafile>"#;
/// let dat = mistarr_core::dat::parse_dat(xml)?;
/// assert_eq!(dat.header.name, "Example System");
/// assert_eq!(dat.games[0].roms[0].crc32.as_deref(), Some("0a0b0c0d"));
/// # Ok::<(), mistarr_core::dat::DatError>(())
/// ```
///
/// # Errors
/// Any [`DatError`] other than the zip variants.
pub fn parse_dat(bytes: &[u8]) -> Result<Dat, DatError> {
    parse_dat_with(bytes, ExportOptions::default())
}

/// Parses a DAT held in memory, turning a DB export's files into roms by `options`;
/// its `parents` are filled from the document.
///
/// ```
/// use mistarr_core::dat::{parse_dat_with, ExportOptions};
/// use mistarr_core::hash::HeaderRule;
/// let xml = br#"<header><version>1</version></header><datafile>
///   <game name="Example Quest (Japan)"><archive number="1" clone="P"/><source>
///     <file extension="nes" size="20" crc32="0a0b0c0d" format="Headered" header="4E 45 53 1A"/>
///     <file extension="unh" size="4" crc32="01020304" format="Headerless"/>
///   </source></game></datafile>"#;
/// let options = ExportOptions { header_rule: HeaderRule::Ines, extension: Some("nes".into()), ..Default::default() };
/// let dat = parse_dat_with(xml, options)?;
/// assert_eq!(dat.games[0].roms[0].name, "Example Quest (Japan).nes");
/// assert_eq!(dat.games[0].roms[0].size, 4);
/// # Ok::<(), mistarr_core::dat::DatError>(())
/// ```
///
/// # Errors
/// Any [`DatError`] other than the zip variants.
pub fn parse_dat_with(bytes: &[u8], mut options: ExportOptions) -> Result<Dat, DatError> {
    if let Some(parents) = export_parents(bytes)? {
        options.parents = parents;
    }
    collect(DatStream::with_options(bytes, options)?)
}

/// Parses a DAT from a buffered reader, one game at a time. A DB export read this way
/// has no parent index, so its clones are not linked; see [`export_parents`].
///
/// ```
/// use std::io::BufReader;
/// let xml = r#"<datafile><game name="Example Quest (Japan)"><rom name="q.bin" size="1"/></game></datafile>"#;
/// let dat = mistarr_core::dat::parse_dat_reader(BufReader::new(xml.as_bytes()))?;
/// assert_eq!(dat.games.len(), 1);
/// # Ok::<(), mistarr_core::dat::DatError>(())
/// ```
///
/// # Errors
/// Any [`DatError`] other than the zip variants.
pub fn parse_dat_reader<R: BufRead>(reader: R) -> Result<Dat, DatError> {
    collect(DatStream::new(reader)?)
}

fn collect<R: BufRead>(mut stream: DatStream<R>) -> Result<Dat, DatError> {
    let mut games = Vec::new();
    for game in &mut stream {
        games.push(game?);
    }
    Ok(Dat {
        header: stream.header,
        games,
    })
}

/// The first pass over a DB export: the archive number and game name of every parent, for
/// [`ExportOptions::parents`]. `None` for a Logiqx DAT, after reading only its first element.
///
/// ```
/// let xml = br#"<header/><datafile>
///   <game name="Example Quest (Japan)"><archive number="0001" clone="P"/></game>
///   <game name="Example Quest (USA)"><archive number="0002" clone="0001"/></game>
/// </datafile>"#;
/// let parents = mistarr_core::dat::export_parents(&xml[..])?.unwrap();
/// assert_eq!(parents["0001"], "Example Quest (Japan)");
/// assert!(mistarr_core::dat::export_parents(&b"<datafile/>"[..])?.is_none());
/// # Ok::<(), mistarr_core::dat::DatError>(())
/// ```
///
/// # Errors
/// As [`DatStream::new`], plus any error met in a game.
pub fn export_parents<R: BufRead>(reader: R) -> Result<Option<HashMap<String, String>>, DatError> {
    let mut stream = DatStream::open(reader, ExportOptions::default(), Mode::Index)?;
    if stream.format != DatFormat::DbExport {
        return Ok(None);
    }
    let mut parents = HashMap::new();
    let mut bytes = 0usize;
    while let Some((game, number)) = stream.pull()? {
        if let Some(n) = number {
            if let std::collections::hash_map::Entry::Vacant(slot) = parents.entry(n) {
                bytes += slot.key().len() + game.name.len();
                if bytes > MAX_INDEX_BYTES {
                    return Err(DatError::IndexTooLarge);
                }
                slot.insert(game.name);
            }
        }
    }
    Ok(Some(parents))
}

/// What a [`DatStream`] reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Every game in full.
    Full,
    /// Only names and archive numbers, for [`export_parents`]; the trailer is not read.
    Index,
    /// Every game in full, keeping each DB export game's archive and sources, for [`rewrite`].
    Raw,
}

/// Iterator over the games of a DAT, holding one game in memory at a time.
/// Yields [`DatError::NoGames`] once if the document ends without any game.
pub struct DatStream<R: BufRead> {
    reader: Reader<Capped<EscapeInvalid<R>>>,
    buf: Vec<u8>,
    header: DatHeader,
    format: DatFormat,
    options: ExportOptions,
    mode: Mode,
    raw: Option<(export::Archive, Vec<export::Source>)>,
    first: Option<(DatGame, Option<String>)>,
    count: usize,
    depth: usize,
    done: bool,
    fused: bool,
}

impl<R: BufRead> DatStream<R> {
    /// Reads up to the header and first game, checking the root element.
    ///
    /// ```
    /// use mistarr_core::dat::DatStream;
    /// let xml = r#"<datafile><header><name>Example System</name><version>2</version></header>
    ///   <game name="Example Quest (USA)"/><game name="Example Quest (Europe)"/></datafile>"#;
    /// let mut stream = DatStream::new(xml.as_bytes())?;
    /// assert_eq!(stream.header().version, "2");
    /// assert_eq!(stream.by_ref().count(), 2);
    /// # Ok::<(), mistarr_core::dat::DatError>(())
    /// ```
    ///
    /// # Errors
    /// [`DatError::NotDatafile`], [`DatError::Xml`], [`DatError::Truncated`] or a header error.
    pub fn new(reader: R) -> Result<Self, DatError> {
        Self::with_options(reader, ExportOptions::default())
    }

    /// As [`DatStream::new`], turning a DB export's files into roms by `options`.
    ///
    /// ```
    /// use mistarr_core::dat::{DatFormat, DatStream, ExportOptions};
    /// let xml = r#"<header><version>7</version></header><datafile><game name="Example Quest (World)"/></datafile>"#;
    /// let stream = DatStream::with_options(xml.as_bytes(), ExportOptions::default())?;
    /// assert_eq!(stream.format(), DatFormat::DbExport);
    /// assert_eq!(stream.header().version, "7");
    /// # Ok::<(), mistarr_core::dat::DatError>(())
    /// ```
    ///
    /// # Errors
    /// As [`DatStream::new`].
    pub fn with_options(reader: R, options: ExportOptions) -> Result<Self, DatError> {
        Self::open(reader, options, Mode::Full)
    }

    fn open(reader: R, options: ExportOptions, mode: Mode) -> Result<Self, DatError> {
        let mut stream = DatStream {
            reader: Reader::from_reader(Capped::new(EscapeInvalid::new(reader))),
            buf: Vec::new(),
            header: DatHeader::default(),
            format: DatFormat::Logiqx,
            options,
            mode,
            raw: None,
            first: None,
            count: 0,
            depth: 0,
            done: false,
            fused: false,
        };
        stream.read_root()?;
        if !(mode == Mode::Index && stream.format == DatFormat::Logiqx) {
            stream.first = stream.next_game()?;
        }
        Ok(stream)
    }

    /// The header, complete once `new` has returned when it precedes the games.
    ///
    /// ```
    /// use mistarr_core::dat::DatStream;
    /// let xml = r#"<datafile><header><name>Example System</name></header><game name="A"/></datafile>"#;
    /// assert_eq!(DatStream::new(xml.as_bytes())?.header().name, "Example System");
    /// # Ok::<(), mistarr_core::dat::DatError>(())
    /// ```
    #[must_use]
    pub fn header(&self) -> &DatHeader {
        &self.header
    }

    /// Which form the document takes.
    ///
    /// ```
    /// use mistarr_core::dat::{DatFormat, DatStream};
    /// let stream = DatStream::new(r#"<datafile><game name="A"/></datafile>"#.as_bytes())?;
    /// assert_eq!(stream.format(), DatFormat::Logiqx);
    /// # Ok::<(), mistarr_core::dat::DatError>(())
    /// ```
    #[must_use]
    pub fn format(&self) -> DatFormat {
        self.format
    }

    fn xml_error(&self, source: quick_xml::Error) -> DatError {
        DatError::Xml {
            position: self.offset(),
            source,
        }
    }

    fn offset(&self) -> u64 {
        self.reader.get_ref().get_ref().position()
    }

    /// Fails when `value` is longer than `limit`.
    fn cap(&self, field: &'static str, value: &str, limit: usize) -> Result<(), DatError> {
        if value.len() > limit {
            return Err(DatError::FieldTooLarge {
                field,
                limit,
                position: self.offset(),
            });
        }
        Ok(())
    }

    /// Fails on a value read from bytes that are not UTF-8.
    fn utf8(&self, value: &str) -> Result<(), DatError> {
        check_utf8(value).map_err(|e| self.xml_error(e.into()))
    }

    /// The next event, each capped at [`MAX_EVENT_BYTES`] and nested at most [`MAX_DEPTH`] deep.
    fn read_event(&mut self) -> Result<Event<'static>, DatError> {
        self.buf.clear();
        let start = self.offset();
        self.reader.get_mut().arm(MAX_EVENT_BYTES);
        let event = match self.reader.read_event_into(&mut self.buf) {
            Ok(event) => event.into_owned(),
            Err(_) if self.reader.get_ref().over() => {
                return Err(DatError::EventTooLarge { position: start })
            }
            Err(err) => return Err(self.xml_error(err)),
        };
        match event {
            Event::Start(_) => {
                self.depth += 1;
                if self.depth > MAX_DEPTH {
                    return Err(DatError::TooDeep { position: start });
                }
            }
            Event::End(_) => self.depth = self.depth.saturating_sub(1),
            _ => {}
        }
        Ok(event)
    }

    /// Reads to the end of input after the root element closes, refusing anything but
    /// whitespace, comments and processing instructions.
    fn read_trailer(&mut self) -> Result<(), DatError> {
        loop {
            let at = self.offset();
            match self.read_event()? {
                Event::Eof => return Ok(()),
                Event::Text(t) if t.as_ref().bytes().all(|b| b.is_ascii_whitespace()) => {}
                Event::Comment(_) | Event::PI(_) => {}
                _ => return Err(DatError::TrailingData { position: at }),
            }
        }
    }

    /// Reads to the `<datafile>` start: a Logiqx root, or the element after a DB export's `<header>`.
    fn read_root(&mut self) -> Result<(), DatError> {
        let mut after_header = false;
        loop {
            match self.read_event()? {
                Event::Start(e) if e.local_name().as_ref() == "datafile" => return Ok(()),
                Event::Empty(e) if e.local_name().as_ref() == "datafile" => {
                    self.done = true;
                    return self.read_trailer();
                }
                Event::Start(e) if !after_header && e.local_name().as_ref() == "header" => {
                    self.format = DatFormat::DbExport;
                    after_header = true;
                    self.read_header()?;
                }
                Event::Empty(e) if !after_header && e.local_name().as_ref() == "header" => {
                    self.format = DatFormat::DbExport;
                    after_header = true;
                }
                Event::Start(e) | Event::Empty(e) => {
                    return Err(DatError::NotDatafile {
                        root: lossy(e.name().as_ref()),
                    })
                }
                Event::Eof => return Err(DatError::Truncated),
                _ => {}
            }
        }
    }

    /// Next game at datafile level with its archive number when it is a parent, parsing a
    /// header met on the way.
    fn next_game(&mut self) -> Result<Option<(DatGame, Option<String>)>, DatError> {
        while !self.done {
            match self.read_event()? {
                Event::Start(e) => match e.local_name().as_ref() {
                    "game" | "machine" => return self.read_game(&e, true).map(Some),
                    "header" => self.read_header()?,
                    _ => self.skip(&e)?,
                },
                Event::Empty(e) => {
                    if matches!(e.local_name().as_ref(), "game" | "machine") {
                        return self.read_game(&e, false).map(Some);
                    }
                }
                Event::End(_) => {
                    self.done = true;
                    if self.mode != Mode::Index {
                        self.read_trailer()?;
                    }
                }
                Event::Eof => return Err(DatError::Truncated),
                _ => {}
            }
        }
        Ok(None)
    }

    /// The prefetched first game, then each following one.
    fn pull(&mut self) -> Result<Option<(DatGame, Option<String>)>, DatError> {
        match self.first.take() {
            Some(first) => Ok(Some(first)),
            None => self.next_game(),
        }
    }

    /// Reads past the element `_start` opened, one capped event at a time.
    fn skip(&mut self, _start: &BytesStart<'_>) -> Result<(), DatError> {
        let mut depth = 1usize;
        while depth > 0 {
            match self.read_event()? {
                Event::Start(_) => depth += 1,
                Event::End(_) => depth -= 1,
                Event::Eof => return Err(DatError::Truncated),
                _ => {}
            }
        }
        Ok(())
    }

    fn read_header(&mut self) -> Result<(), DatError> {
        loop {
            match self.read_event()? {
                Event::Start(e) => {
                    if e.local_name().as_ref() == "clrmamepro" {
                        self.header.clrmamepro_header = self.attr(&e, "header")?;
                    }
                    let text = if e.local_name().as_ref() == "name" {
                        self.read_text("header name", MAX_NAME_BYTES)?
                    } else {
                        self.read_text("header field", MAX_FIELD_BYTES)?
                    };
                    let h = &mut self.header;
                    match e.local_name().as_ref() {
                        "name" => h.name = text,
                        "description" => h.description = text,
                        "version" => h.version = text,
                        "date" => h.date = Some(text),
                        "author" => h.author = Some(text),
                        "homepage" => h.homepage = Some(text),
                        "url" => h.url = Some(text),
                        "comment" => h.comment = Some(text),
                        _ => {}
                    }
                }
                Event::Empty(e) => {
                    if e.local_name().as_ref() == "clrmamepro" {
                        self.header.clrmamepro_header = self.attr(&e, "header")?;
                    }
                }
                Event::End(_) => return Ok(()),
                Event::Eof => return Err(DatError::Truncated),
                _ => {}
            }
        }
    }

    /// Concatenated text up to the end of the current element, trimmed and at most
    /// `limit` bytes before trimming; nested elements are skipped.
    fn read_text(&mut self, field: &'static str, limit: usize) -> Result<String, DatError> {
        let mut text = String::new();
        loop {
            match self.read_event()? {
                Event::Text(t) => {
                    text.push_str(&t.xml10_content());
                    self.cap(field, &text, limit)?;
                }
                Event::CData(t) => {
                    text.push_str(&t);
                    self.cap(field, &text, limit)?;
                }
                Event::GeneralRef(r) => {
                    text.push_str(&self.resolve_ref(&r)?);
                    self.cap(field, &text, limit)?;
                }
                Event::Start(e) => self.skip(&e)?,
                Event::End(_) => {
                    self.utf8(&text)?;
                    return Ok(text.trim().to_owned());
                }
                Event::Eof => return Err(DatError::Truncated),
                _ => {}
            }
        }
    }

    /// Resolves a predefined or character reference; unknown entities are kept as written.
    fn resolve_ref(&self, r: &BytesRef<'_>) -> Result<String, DatError> {
        if let Some(c) = r.resolve_char_ref().map_err(|e| self.xml_error(e))? {
            return Ok(c.to_string());
        }
        let name: &str = r;
        self.utf8(name)?;
        Ok(resolve_predefined_entity(name).map_or_else(|| format!("&{name};"), str::to_owned))
    }

    /// Attribute `key`, capped at [`MAX_NAME_BYTES`] for names, else [`MAX_FIELD_BYTES`].
    fn attr(&self, e: &BytesStart<'_>, key: &'static str) -> Result<Option<String>, DatError> {
        for attr in e.attributes() {
            let attr = attr.map_err(|err| self.xml_error(err.into()))?;
            if attr.key.local_name().as_ref() == key {
                let value: Cow<'_, str> = attr
                    .normalized_value(XmlVersion::Implicit1_0)
                    .map_err(|err| self.xml_error(err))?;
                self.utf8(&value)?;
                let limit = if NAME_KEYS.contains(&key) {
                    MAX_NAME_BYTES
                } else {
                    MAX_FIELD_BYTES
                };
                self.cap(key, &value, limit)?;
                return Ok(Some(value.into_owned()));
            }
        }
        Ok(None)
    }

    fn read_game(
        &mut self,
        start: &BytesStart<'_>,
        has_body: bool,
    ) -> Result<(DatGame, Option<String>), DatError> {
        let name = self
            .attr(start, "name")?
            .ok_or_else(|| DatError::MissingAttribute {
                element: "game",
                attribute: "name",
                game: String::new(),
            })?;
        let export = self.format == DatFormat::DbExport;
        let mut game = DatGame {
            clone_of: None,
            rom_of: None,
            name,
            description: None,
            category: None,
            regions: Vec::new(),
            languages: Vec::new(),
            status: None,
            roms: Vec::new(),
        };
        if !export {
            game.clone_of = self.attr(start, "cloneof")?;
            game.rom_of = self.attr(start, "romof")?;
        }
        let mut archive = export::Archive::default();
        let mut sources: Vec<export::Source> = Vec::new();
        let mut budget = Budget::default();
        if has_body {
            loop {
                let (e, body) = match self.read_event()? {
                    Event::Start(e) => (e, true),
                    Event::Empty(e) => (e, false),
                    Event::End(_) => break,
                    Event::Eof => return Err(DatError::Truncated),
                    _ => continue,
                };
                match e.local_name().as_ref() {
                    "description" if !export && body => {
                        let text = self.read_text("description", MAX_FIELD_BYTES)?;
                        budget.add(&game.name, 0, text.len())?;
                        game.description = Some(text);
                        continue;
                    }
                    "category" if !export && body => {
                        let text = self.read_text("category", MAX_FIELD_BYTES)?;
                        budget.add(&game.name, 0, text.len())?;
                        game.category.get_or_insert(text);
                        continue;
                    }
                    "rom" if !export => {
                        let rom = self.read_rom(&e, &game.name)?;
                        budget.add(&game.name, 1, rom_bytes(&rom))?;
                        game.roms.push(rom);
                    }
                    "release" if !export => {
                        let added = self.read_release(&e, &mut game)?;
                        budget.add(&game.name, 1, added)?;
                    }
                    "archive" if export => archive = self.read_archive(&e)?,
                    "source" if export && self.mode != Mode::Index && body => {
                        let source = self.read_source(&game.name, &mut budget)?;
                        sources.push(source);
                        continue;
                    }
                    _ => {}
                }
                if body {
                    self.skip(&e)?;
                }
            }
        }
        if export {
            game.clone_of = archive.parent(&self.options.parents, &game.name);
            game.regions = export::split_list(archive.region.as_deref());
            game.languages = export::split_list(archive.languages.as_deref());
            game.status = archive.status.clone().filter(|s| !s.trim().is_empty());
            game.roms = export::roms(&game.name, &sources, &self.options);
        }
        self.count += 1;
        let number = archive.number.clone().filter(|_| archive.is_parent());
        if self.mode == Mode::Raw && export {
            self.raw = Some((archive, sources));
        }
        Ok((game, number))
    }

    /// Adds a `<release>`'s region and languages to the game's, each once; returns the
    /// bytes it read.
    fn read_release(&self, e: &BytesStart<'_>, game: &mut DatGame) -> Result<usize, DatError> {
        let region = self.attr(e, "region")?;
        let language = self.attr(e, "language")?;
        let read = region.as_deref().map_or(0, str::len) + language.as_deref().map_or(0, str::len);
        export::extend_unique(&mut game.regions, export::split_list(region.as_deref()));
        export::extend_unique(&mut game.languages, export::split_list(language.as_deref()));
        Ok(read)
    }

    fn read_archive(&self, e: &BytesStart<'_>) -> Result<export::Archive, DatError> {
        Ok(export::Archive {
            number: self.attr(e, "number")?.filter(|n| !n.trim().is_empty()),
            clone: self.attr(e, "clone")?,
            region: self.attr(e, "region")?,
            languages: self.attr(e, "languages")?,
            status: self.attr(e, "status")?,
        })
    }

    /// Reads one `<source>`: every `<file>` in it, counted against the game's `budget`.
    fn read_source(&mut self, game: &str, budget: &mut Budget) -> Result<export::Source, DatError> {
        let mut source = export::Source::default();
        loop {
            let (e, body) = match self.read_event()? {
                Event::Start(e) => (e, true),
                Event::Empty(e) => (e, false),
                Event::End(_) => return Ok(source),
                Event::Eof => return Err(DatError::Truncated),
                _ => continue,
            };
            if e.local_name().as_ref() == "file" {
                if let Some(file) = self.read_file(&e, game)? {
                    budget.add(game, 1, file.bytes())?;
                    source.files.push(file);
                }
            }
            if body {
                self.skip(&e)?;
            }
        }
    }

    /// Reads one `<file>`; an `item` extra that fails to parse is `None`, never an error.
    fn read_file(&self, e: &BytesStart<'_>, game: &str) -> Result<Option<export::File>, DatError> {
        let item = self.attr(e, "item").ok().flatten();
        match (self.read_file_attrs(e, game, item.clone()), item) {
            (Ok(file), _) => Ok(Some(file)),
            (Err(_), Some(_)) => Ok(None),
            (Err(err), None) => Err(err),
        }
    }

    fn read_file_attrs(
        &self,
        e: &BytesStart<'_>,
        game: &str,
        item: Option<String>,
    ) -> Result<export::File, DatError> {
        let size_text = self
            .attr(e, "size")?
            .ok_or_else(|| DatError::MissingAttribute {
                element: "file",
                attribute: "size",
                game: game.to_owned(),
            })?;
        Ok(export::File {
            extension: self.attr(e, "extension")?.unwrap_or_default(),
            format: self.attr(e, "format")?.unwrap_or_default(),
            size: parse_size(game, &size_text)?,
            crc32: self.hex(e, "crc32", 8, game)?,
            md5: self.hex(e, "md5", 32, game)?,
            sha1: self.hex(e, "sha1", 40, game)?,
            header: self.attr(e, "header")?.filter(|h| !h.trim().is_empty()),
            item,
            forcename: self.attr(e, "forcename")?,
            bad: self.attr(e, "bad")?.is_some_and(|v| v.trim() == "1"),
        })
    }

    /// A hash attribute as lowercase hex of `len` digits; empty counts as absent.
    fn hex(
        &self,
        e: &BytesStart<'_>,
        key: &'static str,
        len: usize,
        game: &str,
    ) -> Result<Option<String>, DatError> {
        match self.attr(e, key)? {
            None => Ok(None),
            Some(v) if v.trim().is_empty() => Ok(None),
            Some(v) => {
                let t = v.trim();
                if t.len() == len && t.bytes().all(|b| b.is_ascii_hexdigit()) {
                    Ok(Some(t.to_ascii_lowercase()))
                } else {
                    Err(DatError::InvalidAttribute {
                        game: game.to_owned(),
                        attribute: key,
                        value: v,
                    })
                }
            }
        }
    }

    fn read_rom(&self, e: &BytesStart<'_>, game: &str) -> Result<DatRom, DatError> {
        let missing = |attribute| DatError::MissingAttribute {
            element: "rom",
            attribute,
            game: game.to_owned(),
        };
        let name = self.attr(e, "name")?.ok_or_else(|| missing("name"))?;
        let size_text = self.attr(e, "size")?.ok_or_else(|| missing("size"))?;
        let size = parse_size(game, &size_text)?;
        let status = match self.attr(e, "status")? {
            None => RomStatus::Good,
            Some(v) => RomStatus::parse(&v).ok_or_else(|| DatError::InvalidAttribute {
                game: game.to_owned(),
                attribute: "status",
                value: v,
            })?,
        };
        let crc32 = self.hex(e, "crc", 8, game)?;
        let md5 = self.hex(e, "md5", 32, game)?;
        let sha1 = self.hex(e, "sha1", 40, game)?;
        Ok(DatRom {
            header: self.attr(e, "header")?,
            name,
            size,
            crc32,
            md5,
            sha1,
            status,
        })
    }
}

/// What one game has taken so far against [`MAX_GAME_ENTRIES`] and [`MAX_GAME_BYTES`].
#[derive(Default)]
struct Budget {
    entries: usize,
    bytes: usize,
}

impl Budget {
    fn add(&mut self, game: &str, entries: usize, bytes: usize) -> Result<(), DatError> {
        self.entries += entries;
        self.bytes += bytes;
        if self.entries > MAX_GAME_ENTRIES {
            return Err(DatError::TooManyEntries {
                game: game.to_owned(),
            });
        }
        if self.bytes > MAX_GAME_BYTES {
            return Err(DatError::GameTooLarge {
                game: game.to_owned(),
            });
        }
        Ok(())
    }
}

fn rom_bytes(rom: &DatRom) -> usize {
    let len = |s: &Option<String>| s.as_deref().map_or(0, str::len);
    rom.name.len() + len(&rom.crc32) + len(&rom.md5) + len(&rom.sha1) + len(&rom.header)
}

fn parse_size(game: &str, text: &str) -> Result<u64, DatError> {
    text.trim()
        .parse::<u64>()
        .map_err(|_| DatError::InvalidAttribute {
            game: game.to_owned(),
            attribute: "size",
            value: text.to_owned(),
        })
}

impl<R: BufRead> Iterator for DatStream<R> {
    type Item = Result<DatGame, DatError>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some((game, _)) = self.first.take() {
            return Some(Ok(game));
        }
        if self.fused {
            return None;
        }
        let item = match self.next_game() {
            Ok(Some((game, _))) => return Some(Ok(game)),
            Ok(None) if self.count > 0 => None,
            Ok(None) => Some(Err(DatError::NoGames)),
            Err(err) => Some(Err(err)),
        };
        self.fused = true;
        item
    }
}

/// One DAT from a zip pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackMember {
    /// Member path inside the archive.
    pub file_name: String,
    /// The parsed DAT.
    pub dat: Dat,
}

/// Iterator over the `.dat` and `.xml` members of a zip pack, parsing one member per step.
pub struct DatPack<R: Read + Seek> {
    archive: zip::ZipArchive<R>,
    members: Vec<usize>,
    next: usize,
}

/// Opens a zipped DAT pack. Members are yielded in archive order; other files are ignored.
///
/// ```
/// use std::io::{Cursor, Write};
/// let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
/// zip.start_file("Example System.dat", zip::write::SimpleFileOptions::default())?;
/// zip.write_all(br#"<datafile><game name="Example Quest (USA)"/></datafile>"#)?;
/// let bytes = zip.finish()?.into_inner();
///
/// let members: Vec<_> = mistarr_core::dat::parse_dat_pack(Cursor::new(bytes))?
///     .collect::<Result<_, _>>()?;
/// assert_eq!(members[0].file_name, "Example System.dat");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// # Errors
/// [`DatError::Zip`] if the central directory is unreadable, [`DatError::EmptyPack`]
/// if no member has a `.dat` or `.xml` extension.
pub fn parse_dat_pack<R: Read + Seek>(reader: R) -> Result<DatPack<R>, DatError> {
    let archive = zip::ZipArchive::new(reader)?;
    let members: Vec<usize> = (0..archive.len())
        .filter(|&i| archive.name_for_index(i).is_some_and(is_dat_member))
        .collect();
    if members.is_empty() {
        return Err(DatError::EmptyPack);
    }
    Ok(DatPack {
        archive,
        members,
        next: 0,
    })
}

fn is_dat_member(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    !lower.ends_with('/')
        && std::path::Path::new(&lower)
            .extension()
            .is_some_and(|ext| ext == "dat" || ext == "xml")
}

impl<R: Read + Seek> DatPack<R> {
    /// Number of DAT members in the archive.
    ///
    /// ```
    /// use std::io::{Cursor, Write};
    /// let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    /// zip.start_file("a.xml", zip::write::SimpleFileOptions::default())?;
    /// zip.write_all(br#"<datafile><game name="A"/></datafile>"#)?;
    /// zip.start_file("readme.txt", zip::write::SimpleFileOptions::default())?;
    /// let pack = mistarr_core::dat::parse_dat_pack(zip.finish()?)?;
    /// assert_eq!(pack.len(), 1);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn len(&self) -> usize {
        self.members.len()
    }

    /// Always false: a pack without DAT members is rejected when opened.
    ///
    /// ```
    /// use std::io::{Cursor, Write};
    /// let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    /// zip.start_file("a.dat", zip::write::SimpleFileOptions::default())?;
    /// zip.write_all(br#"<datafile><game name="A"/></datafile>"#)?;
    /// assert!(!mistarr_core::dat::parse_dat_pack(zip.finish()?)?.is_empty());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// Parses one member, reading a DB export twice so its clones are linked.
    fn read_member(&mut self, index: usize) -> Result<PackMember, DatError> {
        let file_name = self.archive.by_index(index)?.name().to_owned();
        let in_member = |source| DatError::Member {
            member: file_name.clone(),
            source: Box::new(source),
        };
        let parents = export_parents(BufReader::new(self.archive.by_index(index)?))
            .map_err(in_member)?
            .unwrap_or_default();
        let options = ExportOptions {
            parents,
            ..ExportOptions::default()
        };
        let stream =
            DatStream::with_options(BufReader::new(self.archive.by_index(index)?), options);
        let dat = stream.and_then(collect).map_err(in_member)?;
        Ok(PackMember { file_name, dat })
    }
}

impl<R: Read + Seek> Iterator for DatPack<R> {
    type Item = Result<PackMember, DatError>;

    fn next(&mut self) -> Option<Self::Item> {
        let index = *self.members.get(self.next)?;
        self.next += 1;
        Some(self.read_member(index))
    }
}
