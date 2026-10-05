//! Neo Geo adapter: the DAT game is the unit, placed whole as a zip or directory,
//! and the core's `romsets.xml` read for the romsets and BIOS files it names.

use std::collections::HashSet;
use std::io::{self, BufRead, BufReader, Read as _};
use std::path::{Path, PathBuf};

use mistarr_core::dat::{MAX_DEPTH, MAX_EVENT_BYTES};
use mistarr_core::xml::{attr_value, lossy, CappedReader};
use quick_xml::events::Event;

use super::xml_caps;
use super::{basename, safe_name, staged_name, CoreAdapter, PlacementPlan};
use crate::input::{DatEntry, StagedFile, StagedKind};
use crate::platforms::Platform;
use crate::{Error, Result};

/// The file the Neo Geo core reads its romset list from, in `games/NeoGeo`.
pub const ROMSETS_FILE: &str = "romsets.xml";

/// What a `romsets.xml` names.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Romsets {
    /// The `name` of every `<romset>`, first-seen order: the directory or zip each is loaded from.
    pub sets: Vec<String>,
    /// File names listed on their own line inside XML comments, which is where the core's
    /// file names the BIOS files it expects. Reported as present or missing, never handled.
    pub bios: Vec<String>,
}

/// Largest `romsets.xml` read, a sanity bound.
pub const MAX_ROMSETS_BYTES: u64 = 16 * 1024 * 1024;

/// Most distinct `<romset name>` values collected; further ones are refused.
pub const MAX_SETS: usize = 4096;

/// Most distinct BIOS file names collected from comments; further ones are refused.
pub const MAX_BIOS_NAMES: usize = 4096;

/// Longest romset or BIOS name kept, in bytes; a real name is a short file or
/// directory name, so a longer one is dropped rather than stored twice over.
pub const MAX_NAME_BYTES: usize = 256;

fn romsets_err(e: quick_xml::Error, position: u64) -> Error {
    Error::Romsets {
        position,
        source: e,
    }
}

fn too_big() -> Error {
    Error::FileTooLarge {
        limit: MAX_ROMSETS_BYTES,
    }
}

/// Parses a `romsets.xml`, streamed through a [`BufRead`] with size, event and depth caps.
///
/// # Errors
///
/// [`Error::Romsets`] when the document is not well-formed XML,
/// [`Error::XmlTooDeep`] past [`MAX_DEPTH`] levels of nesting,
/// [`Error::XmlEventTooLarge`] past one capped event, and [`Error::XmlOutputTooLarge`]
/// past [`MAX_SETS`] or [`MAX_BIOS_NAMES`].
///
/// ```
/// let r = mistarr_mister::adapter::neogeo::parse_romsets(
///     b"<!-- needs:\n  exbios.rom\n--><romsets><romset name=\"examplequest\"/></romsets>".as_slice(),
/// ).unwrap();
/// assert_eq!(r.sets, ["examplequest"]);
/// assert_eq!(r.bios, ["exbios.rom"]);
/// ```
pub fn parse_romsets<R: BufRead>(xml: R) -> Result<Romsets> {
    let mut reader = CappedReader::new(xml, MAX_EVENT_BYTES, MAX_DEPTH);
    let mut out = Romsets::default();
    let mut sets_seen: HashSet<String> = HashSet::new();
    let mut bios_seen: HashSet<String> = HashSet::new();
    loop {
        let position = reader.position();
        let event = reader
            .read_event()
            .map_err(|e| xml_caps::read_error(e, 0, romsets_err))?;
        match event {
            Event::Start(e) | Event::Empty(e)
                if e.local_name().as_ref().eq_ignore_ascii_case("romset") =>
            {
                let name = attr_value(&e, "name").map_err(|e| romsets_err(e, position))?;
                let v = name.as_deref().map_or("", str::trim);
                if !v.is_empty() && v.len() <= MAX_NAME_BYTES && sets_seen.insert(v.to_owned()) {
                    if out.sets.len() >= MAX_SETS {
                        return Err(xml_caps::output_too_large("romsets", MAX_SETS, position));
                    }
                    out.sets.push(v.to_owned());
                }
            }
            Event::Comment(c) => {
                let text = lossy(&c);
                for line in text.lines().map(str::trim) {
                    if is_file_name(line) && bios_seen.insert(line.to_owned()) {
                        if out.bios.len() >= MAX_BIOS_NAMES {
                            return Err(xml_caps::output_too_large(
                                "bios names",
                                MAX_BIOS_NAMES,
                                position,
                            ));
                        }
                        out.bios.push(line.to_owned());
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

/// A single `name.ext` token within [`MAX_NAME_BYTES`]: letters, digits, `-` and `_`,
/// with a short extension.
fn is_file_name(s: &str) -> bool {
    if s.len() > MAX_NAME_BYTES {
        return false;
    }
    let Some((stem, ext)) = s.rsplit_once('.') else {
        return false;
    };
    let ok = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.');
    !stem.is_empty()
        && (1..=5).contains(&ext.len())
        && ext.chars().all(|c| c.is_ascii_alphanumeric())
        && stem.chars().all(ok)
}

/// Reads and parses `romsets.xml` from `neogeo_dir`, streaming it through a [`BufReader`]
/// rather than reading the whole file into memory first; `None` when the file is absent.
///
/// # Errors
///
/// [`Error::Io`] when the file exists but cannot be read, [`Error::FileTooLarge`] when
/// it exceeds [`MAX_ROMSETS_BYTES`], and the errors [`parse_romsets`] returns when it is
/// not well-formed or its output outgrows a cap.
///
/// ```
/// let dir = tempfile::tempdir().unwrap();
/// assert!(mistarr_mister::adapter::neogeo::read_romsets(dir.path()).unwrap().is_none());
/// ```
pub fn read_romsets(neogeo_dir: &Path) -> Result<Option<Romsets>> {
    let path = neogeo_dir.join(ROMSETS_FILE);
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::io_at(&path)(e)),
    };
    if file.metadata().map_err(Error::io_at(&path))?.len() > MAX_ROMSETS_BYTES {
        return Err(too_big());
    }
    let mut limited = BufReader::new(file.take(MAX_ROMSETS_BYTES + 1));
    let romsets = parse_romsets(&mut limited)?;
    if limited.into_inner().limit() == 0 {
        return Err(too_big());
    }
    Ok(Some(romsets))
}

/// Whether romset `name` is in `neogeo_dir` as a directory or a `.zip`.
///
/// ```
/// let dir = tempfile::tempdir().unwrap();
/// std::fs::write(dir.path().join("exblast.zip"), b"").unwrap();
/// assert!(mistarr_mister::adapter::neogeo::romset_on_disk(dir.path(), "exblast"));
/// assert!(!mistarr_mister::adapter::neogeo::romset_on_disk(dir.path(), "exmissing"));
/// ```
#[must_use]
pub fn romset_on_disk(neogeo_dir: &Path, name: &str) -> bool {
    if name.is_empty() || name.contains(['/', '\\']) || name == ".." {
        return false;
    }
    neogeo_dir.join(name).is_dir() || neogeo_dir.join(format!("{name}.zip")).is_file()
}

/// Romset placement; members are verified against the DAT, not the zip's own hash.
pub(super) struct NeoGeo(pub &'static Platform);

impl CoreAdapter for NeoGeo {
    fn plan_placement(&self, entry: &DatEntry, staged: &StagedFile) -> Result<PlacementPlan> {
        let name = safe_name(&entry.name)?;
        let final_rel_path = match staged.kind {
            StagedKind::Zip => PathBuf::from(self.0.core_dir).join(format!("{name}.zip")),
            StagedKind::Dir => PathBuf::from(self.0.core_dir).join(name),
            StagedKind::File => {
                return Err(Error::Unplaceable(
                    "Neo Geo games are placed as a zip or directory",
                ))
            }
        };
        for rom in &entry.roms {
            let present = staged
                .members
                .iter()
                .any(|m| basename(&m.name).eq_ignore_ascii_case(basename(&rom.name)));
            if !present {
                return Err(Error::MissingRom(rom.name.clone()));
            }
        }
        Ok(PlacementPlan::rename(staged_name(staged)?, final_rel_path))
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use proptest::prelude::*;

    use super::super::testutil::*;
    use super::super::Step;
    use super::*;

    fn game() -> DatEntry {
        entry("examplequest", &[("001-p1.p1", 1024), ("001-s1.s1", 512)])
    }

    #[test]
    fn zip_is_placed_whole() {
        let z = container(
            StagedKind::Zip,
            "dl.zip",
            &[("001-p1.p1", 1024, b""), ("001-s1.s1", 512, b"")],
        );
        let plan = adapter("neogeo").plan_placement(&game(), &z).expect("plan");
        assert_eq!(plan.final_rel_path, Path::new("NeoGeo/examplequest.zip"));
        assert_eq!(
            plan.steps,
            [Step::Rename {
                from: "dl.zip".into(),
                to: "NeoGeo/examplequest.zip".into()
            }]
        );
    }

    #[test]
    fn directory_is_placed_whole() {
        let d = container(
            StagedKind::Dir,
            "examplequest",
            &[("001-p1.p1", 1024, b""), ("001-s1.s1", 512, b"")],
        );
        let plan = adapter("neogeo").plan_placement(&game(), &d).expect("plan");
        assert_eq!(plan.final_rel_path, Path::new("NeoGeo/examplequest"));
    }

    #[test]
    fn incomplete_set_and_loose_file_are_refused() {
        let z = container(StagedKind::Zip, "dl.zip", &[("001-p1.p1", 1024, b"")]);
        assert!(matches!(
            adapter("neogeo").plan_placement(&game(), &z),
            Err(Error::MissingRom(n)) if n == "001-s1.s1"
        ));
        assert!(adapter("neogeo")
            .plan_placement(&game(), &file("x.p1", 1, &[]))
            .is_err());
    }

    const ROMSETS: &str = r#"<!--
Place this file in the NeoGeo directory.
Files that must be present:

   exbios.rom
   ex-lo.lo
   exfix.fix

-->
<romsets>
  <!-- first group -->
  <romset name="examplequest" altname="Example Quest" year="1990"/>
  <romset name="exblast"><file name="ex-p1.p1" type="P" index="0"/></romset>
  <romset name="examplequest"/>
  <romset altname="no name"/>
</romsets>"#;

    #[test]
    fn romsets_lists_sets_and_commented_bios_names() {
        let r = parse_romsets(ROMSETS.as_bytes()).expect("parse");
        assert_eq!(r.sets, ["examplequest", "exblast"]);
        assert_eq!(r.bios, ["exbios.rom", "ex-lo.lo", "exfix.fix"]);
        assert!(matches!(
            parse_romsets(b"<romsets><romset name=\"a\"></oops>".as_slice()),
            Err(Error::Romsets { .. })
        ));
        let latin1 = parse_romsets(
            b"<!-- Caf\xe9\n  exbios.rom\n--><romsets><romset name=\"a\"/></romsets>".as_slice(),
        )
        .expect("parse");
        assert_eq!(
            (latin1.sets, latin1.bios),
            (vec!["a".to_owned()], vec!["exbios.rom".to_owned()])
        );
        assert!(matches!(
            parse_romsets(b"<romsets><romset name=\"\xe9\"/></romsets>".as_slice()),
            Err(Error::Romsets { .. })
        ));
    }

    /// `n` levels of `<a>` nested inside `<romsets>`, well-formed either way.
    fn nested(n: usize) -> String {
        format!("<romsets>{}{}</romsets>", "<a>".repeat(n), "</a>".repeat(n))
    }

    #[test]
    fn depth_cap_refuses_deep_nesting_without_panicking() {
        assert!(parse_romsets(nested(MAX_DEPTH - 4).as_bytes()).is_ok());
        assert!(matches!(
            parse_romsets(nested(MAX_DEPTH * 4).as_bytes()),
            Err(Error::XmlTooDeep { .. })
        ));
    }

    #[test]
    fn event_size_cap_refuses_an_oversized_attribute() {
        let huge = "x".repeat(2 * 1024 * 1024);
        let xml = format!("<romsets><romset name=\"{huge}\"/></romsets>");
        assert!(matches!(
            parse_romsets(xml.as_bytes()),
            Err(Error::XmlEventTooLarge { .. })
        ));
    }

    /// `n` distinct `<romset name="sI">` tags.
    fn romset_tags(n: usize) -> String {
        (0..n).fold(String::new(), |mut acc, i| {
            write!(acc, "<romset name=\"s{i}\"/>").expect("write");
            acc
        })
    }

    #[test]
    fn romset_count_cap_refuses_growth_past_the_limit() {
        let over = format!("<romsets>{}</romsets>", romset_tags(MAX_SETS + 1));
        assert!(matches!(
            parse_romsets(over.as_bytes()),
            Err(Error::XmlOutputTooLarge { limit, .. }) if limit == MAX_SETS
        ));
        let under = format!("<romsets>{}</romsets>", romset_tags(MAX_SETS));
        let sets = parse_romsets(under.as_bytes()).expect("parse").sets;
        assert_eq!(sets.len(), MAX_SETS);
    }

    #[test]
    fn bios_name_count_cap_refuses_growth_past_the_limit() {
        let lines = (0..=MAX_BIOS_NAMES).fold(String::new(), |mut acc, i| {
            writeln!(acc, "bios{i}.rom").expect("write");
            acc
        });
        let xml = format!("<!--{lines}--><romsets/>");
        assert!(matches!(
            parse_romsets(xml.as_bytes()),
            Err(Error::XmlOutputTooLarge { limit, .. }) if limit == MAX_BIOS_NAMES
        ));
    }

    #[test]
    fn read_romsets_refuses_an_oversized_file() {
        let tmp = crate::adapter::testutil::scratch();
        let dir = tmp.path();
        let pad = " ".repeat(usize::try_from(MAX_ROMSETS_BYTES).expect("fits"));
        std::fs::write(dir.join(ROMSETS_FILE), format!("<romsets>{pad}</romsets>")).expect("write");
        assert!(matches!(
            read_romsets(dir),
            Err(Error::FileTooLarge { limit }) if limit == MAX_ROMSETS_BYTES
        ));
    }

    #[test]
    fn file_name_tokens() {
        assert!(is_file_name("ex-s2.sp1"));
        assert!(!is_file_name("Place this file."));
        assert!(!is_file_name("nodot"));
        assert!(!is_file_name(".hidden"));
        assert!(!is_file_name("a.toolongext"));
        let long = format!("{}.rom", "a".repeat(MAX_NAME_BYTES));
        assert!(!is_file_name(&long));
    }

    #[test]
    fn overlong_names_are_dropped_not_stored() {
        let long_set = "s".repeat(MAX_NAME_BYTES + 1);
        let xml = format!("<romsets><romset name=\"{long_set}\"/></romsets>");
        assert_eq!(
            parse_romsets(xml.as_bytes()).expect("parse").sets,
            Vec::<String>::new()
        );
        let long_bios = format!("{}.rom", "b".repeat(MAX_NAME_BYTES));
        let xml = format!("<!--{long_bios}--><romsets/>");
        assert_eq!(
            parse_romsets(xml.as_bytes()).expect("parse").bios,
            Vec::<String>::new()
        );
    }

    #[test]
    fn romsets_file_and_presence_on_disk() {
        let tmp = scratch();
        let dir = tmp.path();
        assert!(read_romsets(dir).expect("read").is_none());
        std::fs::write(dir.join(ROMSETS_FILE), ROMSETS).expect("write");
        let r = read_romsets(dir).expect("read").expect("present");
        assert_eq!(r.sets.len(), 2);
        std::fs::create_dir_all(dir.join("examplequest")).expect("mkdir");
        std::fs::write(dir.join("exblast.zip"), b"").expect("write");
        assert!(romset_on_disk(dir, "examplequest"));
        assert!(romset_on_disk(dir, "exblast"));
        assert!(!romset_on_disk(dir, "exmissing"));
        assert!(!romset_on_disk(dir, "../neogeo-romsets"));
    }

    proptest! {
        #[test]
        fn parse_romsets_never_panics(s in ".{0,200}") {
            let _ = parse_romsets(s.as_bytes());
        }

        #[test]
        fn depth_cap_never_panics(n in 0usize..300) {
            let _ = parse_romsets(nested(n).as_bytes());
        }
    }
}
