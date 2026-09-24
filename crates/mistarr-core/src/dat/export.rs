//! No-Intro DB export games turned into rom entries; the rules are `docs/VERIFICATION.md` "DB export".

use std::collections::HashMap;
use std::path::Path;

use super::{DatRom, ExportOptions, RomStatus};
use crate::hash::HeaderRule;

/// A game's `<archive>`: its number, parent reference, region and languages.
#[derive(Debug, Default)]
pub(super) struct Archive {
    pub(super) number: Option<String>,
    pub(super) clone: Option<String>,
    pub(super) region: Option<String>,
    pub(super) languages: Option<String>,
}

impl Archive {
    /// The parent's game name: `clone` holds the parent's archive number, `P` or nothing on a parent.
    pub(super) fn parent(&self, parents: &HashMap<String, String>, own: &str) -> Option<String> {
        let clone = self.clone.as_deref()?.trim();
        if clone.is_empty() || clone.eq_ignore_ascii_case("p") {
            return None;
        }
        parents.get(clone).filter(|name| *name != own).cloned()
    }
}

/// One `<source>`: a dump of the game and the files it describes.
#[derive(Debug, Default)]
pub(super) struct Source {
    /// The `details` section names a bad dump.
    pub(super) bad: bool,
    pub(super) files: Vec<File>,
}

/// One `<file>` of a source.
#[derive(Debug)]
pub(super) struct File {
    pub(super) extension: String,
    pub(super) format: String,
    pub(super) size: u64,
    pub(super) crc32: Option<String>,
    pub(super) md5: Option<String>,
    pub(super) sha1: Option<String>,
    pub(super) header: Option<String>,
}

/// How a file is stored, from its `format` attribute and extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Headerless,
    Headered,
    BigEndian,
    Plain,
    Other,
}

impl File {
    fn kind(&self) -> Kind {
        let format = self.format.trim().to_ascii_lowercase();
        match format.as_str() {
            "headerless" => Kind::Headerless,
            _ if self.extension.trim().eq_ignore_ascii_case("unh") => Kind::Headerless,
            "headered" => Kind::Headered,
            "bigendian" => Kind::BigEndian,
            "" | "default" => Kind::Plain,
            _ => Kind::Other,
        }
    }

    /// What identifies the dump: its SHA1, else its size and other hashes.
    fn key(&self) -> String {
        match &self.sha1 {
            Some(sha1) => sha1.clone(),
            None => format!(
                "{}:{}:{}",
                self.size,
                self.crc32.as_deref().unwrap_or(""),
                self.md5.as_deref().unwrap_or("")
            ),
        }
    }

    fn own_extension(&self) -> &str {
        self.extension.trim().trim_start_matches('.')
    }
}

/// Preference of a storage kind under a header rule; the lowest present is taken.
fn rank(rule: HeaderRule, kind: Kind) -> u8 {
    match kind {
        Kind::Headerless if rule.strips_header() => 0,
        Kind::BigEndian if rule == HeaderRule::N64 => 0,
        Kind::Plain => 1,
        Kind::BigEndian => 2,
        Kind::Other => 3,
        Kind::Headered => 4,
        Kind::Headerless => 5,
    }
}

/// The rom entries of a game: files of the preferred kind across every source, once per
/// dump and per name, each named `<game>.<extension>`.
pub(super) fn roms(game: &str, sources: &[Source], options: &ExportOptions) -> Vec<DatRom> {
    let rule = options.header_rule;
    let files = || {
        sources
            .iter()
            .flat_map(|s| s.files.iter().map(move |f| (s, f)))
    };
    let Some(best) = files().map(|(_, f)| rank(rule, f.kind())).min() else {
        return Vec::new();
    };
    let sibling_ext = files()
        .find(|(_, f)| f.kind() == Kind::Headered && !f.own_extension().is_empty())
        .map(|(_, f)| f.own_extension().to_owned());
    let mut out: Vec<DatRom> = Vec::new();
    let mut keys: Vec<String> = Vec::new();
    for (source, file) in files().filter(|(_, f)| rank(rule, f.kind()) == best) {
        let key = file.key();
        if let Some(at) = keys.iter().position(|k| *k == key) {
            if !source.bad {
                out[at].status = RomStatus::Good;
            }
            continue;
        }
        let name = rom_name(game, file, options, sibling_ext.as_deref());
        if out.iter().any(|r| r.name == name) {
            continue;
        }
        let header = if file.kind() == Kind::Headerless {
            source
                .files
                .iter()
                .find(|f| f.kind() == Kind::Headered && f.header.is_some())
                .and_then(|f| f.header.clone())
        } else {
            None
        };
        keys.push(key);
        out.push(DatRom {
            name,
            size: file.size,
            crc32: file.crc32.clone(),
            md5: file.md5.clone(),
            sha1: file.sha1.clone(),
            status: if source.bad {
                RomStatus::BadDump
            } else {
                RomStatus::Good
            },
            header,
        });
    }
    out
}

/// `<game>.<ext>`, where a `.unh` or missing extension becomes the platform's, else the headered file's.
fn rom_name(game: &str, file: &File, options: &ExportOptions, sibling: Option<&str>) -> String {
    let own = file.own_extension();
    let ext = if own.is_empty() || own.eq_ignore_ascii_case("unh") {
        options
            .extension
            .as_deref()
            .or(sibling)
            .unwrap_or(own)
            .trim_start_matches('.')
    } else {
        own
    };
    if ext.is_empty() {
        game.to_owned()
    } else {
        format!("{game}.{ext}")
    }
}

/// Comma-separated values, trimmed, empty ones dropped.
pub(super) fn split_list(value: Option<&str>) -> Vec<String> {
    value
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Appends each of `more` not already in `list`.
pub(super) fn extend_unique(list: &mut Vec<String>, more: Vec<String>) {
    for v in more {
        if !list.contains(&v) {
            list.push(v);
        }
    }
}

/// The DAT name and version a DB export file name carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportName {
    /// `<System> (DB Export)`, stable across versions so a newer export supersedes an older one.
    pub dat_name: String,
    /// The parenthesised group after `(DB Export)`, empty when there is none.
    pub version: String,
}

/// Reads `<System> (DB Export) (<version>)` from a file name, with or without its
/// `.xml`, `.dat` or `.zip` extension; `None` when the name does not have that shape.
///
/// ```
/// use mistarr_core::dat::export_name;
/// let name = export_name("Example Vendor - Example System (DB Export) (20260101-000000).xml").unwrap();
/// assert_eq!(name.dat_name, "Example Vendor - Example System (DB Export)");
/// assert_eq!(name.version, "20260101-000000");
/// assert!(export_name("Example Vendor - Example System (20260101).dat").is_none());
/// ```
#[must_use]
pub fn export_name(file_name: &str) -> Option<ExportName> {
    const MARKER: &str = "(db export)";
    let path = Path::new(file_name);
    let known = path.extension().is_some_and(|e| {
        ["xml", "dat", "zip"]
            .iter()
            .any(|k| e.eq_ignore_ascii_case(k))
    });
    let stem = if known {
        path.file_stem()?.to_str()?
    } else {
        path.file_name()?.to_str()?
    };
    let at = stem.to_ascii_lowercase().find(MARKER)?;
    let system = stem[..at].trim();
    if system.is_empty() {
        return None;
    }
    let rest = stem[at + MARKER.len()..].trim_start();
    let version = rest
        .strip_prefix('(')
        .and_then(|r| r.split_once(')'))
        .map(|(v, _)| v.trim().to_owned())
        .unwrap_or_default();
    Some(ExportName {
        dat_name: format!("{system} (DB Export)"),
        version,
    })
}

#[cfg(test)]
mod tests;
