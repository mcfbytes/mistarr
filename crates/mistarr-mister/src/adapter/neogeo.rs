//! Neo Geo adapter: the DAT game is the unit, placed whole as a zip or directory,
//! and the core's `romsets.xml` read for the romsets and BIOS files it names.

use std::path::{Path, PathBuf};

use mistarr_core::xml::{check_utf8, lossy, EscapeInvalid};
use quick_xml::events::Event;
use quick_xml::{Reader, XmlVersion};

use super::{
    basename, extension, row_methods, safe_name, staged_name, CoreAdapter, PlacementPlan, Step,
};
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

/// Parses a `romsets.xml`.
///
/// # Errors
///
/// [`Error::Romsets`] when the document is not well-formed.
///
/// ```
/// let r = mistarr_mister::adapter::neogeo::parse_romsets(
///     b"<!-- needs:\n  exbios.rom\n--><romsets><romset name=\"examplequest\"/></romsets>",
/// ).unwrap();
/// assert_eq!(r.sets, ["examplequest"]);
/// assert_eq!(r.bios, ["exbios.rom"]);
/// ```
pub fn parse_romsets(xml: &[u8]) -> Result<Romsets> {
    let err = |e: &dyn std::fmt::Display| Error::Romsets(e.to_string());
    let mut reader = Reader::from_reader(EscapeInvalid::new(xml));
    let mut buf = Vec::new();
    let mut out = Romsets::default();
    loop {
        buf.clear();
        match reader.read_event_into(&mut buf).map_err(|e| err(&e))? {
            Event::Start(e) | Event::Empty(e)
                if e.local_name().as_ref().eq_ignore_ascii_case("romset") =>
            {
                for a in e.attributes() {
                    let a = a.map_err(|e| err(&e))?;
                    if a.key.local_name().as_ref() == "name" {
                        let v = a
                            .normalized_value(XmlVersion::Implicit1_0)
                            .map_err(|e| err(&e))?;
                        check_utf8(&v).map_err(|e| err(&quick_xml::Error::from(e)))?;
                        let v = v.trim().to_owned();
                        if !v.is_empty() && !out.sets.contains(&v) {
                            out.sets.push(v);
                        }
                    }
                }
            }
            Event::Comment(c) => {
                let text = lossy(&c);
                for line in text.lines().map(str::trim) {
                    if is_file_name(line) && !out.bios.iter().any(|b| b == line) {
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

/// A single `name.ext` token: letters, digits, `-` and `_`, with a short extension.
fn is_file_name(s: &str) -> bool {
    let Some((stem, ext)) = s.rsplit_once('.') else {
        return false;
    };
    let ok = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.');
    !stem.is_empty()
        && (1..=5).contains(&ext.len())
        && ext.chars().all(|c| c.is_ascii_alphanumeric())
        && stem.chars().all(ok)
}

/// Reads and parses `romsets.xml` from `neogeo_dir`, or `None` when the file is absent.
///
/// # Errors
///
/// [`Error::Io`] when the file exists but cannot be read, [`Error::Romsets`] when it is not well-formed.
///
/// ```
/// let dir = std::env::temp_dir().join("mistarr-doc-romsets-none");
/// assert!(mistarr_mister::adapter::neogeo::read_romsets(&dir).unwrap().is_none());
/// ```
pub fn read_romsets(neogeo_dir: &Path) -> Result<Option<Romsets>> {
    match std::fs::read(neogeo_dir.join(ROMSETS_FILE)) {
        Ok(bytes) => parse_romsets(&bytes).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Whether romset `name` is in `neogeo_dir` as a directory or a `.zip`.
///
/// ```
/// let dir = std::env::temp_dir();
/// assert!(!mistarr_mister::adapter::neogeo::romset_on_disk(&dir, "mistarr-absent-set"));
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
    row_methods!();

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
        Ok(PlacementPlan {
            steps: vec![Step::Rename {
                from: staged_name(staged)?,
                to: final_rel_path.clone(),
            }],
            final_rel_path,
        })
    }

    fn accepts(&self, path: &Path) -> bool {
        extension(path).as_deref() == Some("zip") || path.is_dir()
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;
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

    #[test]
    fn accepts_zip_and_directory() {
        let a = adapter("neogeo");
        assert!(a.accepts(Path::new("examplequest.zip")));
        assert!(a.accepts(&scratch("neogeo-dir")));
        assert!(!a.accepts(Path::new("examplequest.p1")));
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
            parse_romsets(b"<romsets><romset name=\"a\"></oops>"),
            Err(Error::Romsets(_))
        ));
        let latin1 = parse_romsets(
            b"<!-- Caf\xe9\n  exbios.rom\n--><romsets><romset name=\"a\"/></romsets>",
        )
        .expect("parse");
        assert_eq!(
            (latin1.sets, latin1.bios),
            (vec!["a".to_owned()], vec!["exbios.rom".to_owned()])
        );
        assert!(matches!(
            parse_romsets(b"<romsets><romset name=\"\xe9\"/></romsets>"),
            Err(Error::Romsets(_))
        ));
    }

    #[test]
    fn file_name_tokens() {
        assert!(is_file_name("ex-s2.sp1"));
        assert!(!is_file_name("Place this file."));
        assert!(!is_file_name("nodot"));
        assert!(!is_file_name(".hidden"));
        assert!(!is_file_name("a.toolongext"));
    }

    #[test]
    fn romsets_file_and_presence_on_disk() {
        let dir = scratch("neogeo-romsets");
        assert!(read_romsets(&dir).expect("read").is_none());
        std::fs::write(dir.join(ROMSETS_FILE), ROMSETS).expect("write");
        let r = read_romsets(&dir).expect("read").expect("present");
        assert_eq!(r.sets.len(), 2);
        std::fs::create_dir_all(dir.join("examplequest")).expect("mkdir");
        std::fs::write(dir.join("exblast.zip"), b"").expect("write");
        assert!(romset_on_disk(&dir, "examplequest"));
        assert!(romset_on_disk(&dir, "exblast"));
        assert!(!romset_on_disk(&dir, "exmissing"));
        assert!(!romset_on_disk(&dir, "../neogeo-romsets"));
    }
}
