//! No-Intro DB export games turned into rom entries; the rules are `docs/VERIFICATION.md` "DB export".

use std::collections::HashMap;
use std::path::Path;

use super::{DatRom, ExportOptions, RomStatus};
use crate::hash::HeaderRule;

/// A game's `<archive>`: its number, parent reference, region, languages and release status.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct Archive {
    pub(super) number: Option<String>,
    pub(super) clone: Option<String>,
    pub(super) region: Option<String>,
    pub(super) languages: Option<String>,
    pub(super) status: Option<String>,
}

impl Archive {
    /// The parent's game name: `clone` holds the parent's archive number, `P` or nothing on a parent.
    pub(super) fn parent(&self, parents: &HashMap<String, String>, own: &str) -> Option<String> {
        if self.is_parent() {
            return None;
        }
        let clone = self.clone.as_deref()?.trim();
        parents.get(clone).filter(|name| *name != own).cloned()
    }

    /// Whether the game is a parent: `clone` is `P`, empty or absent.
    pub(super) fn is_parent(&self) -> bool {
        self.clone
            .as_deref()
            .map(str::trim)
            .is_none_or(|c| c.is_empty() || c.eq_ignore_ascii_case("p"))
    }
}

/// One `<source>`: a dump of the game and the files it describes.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct Source {
    pub(super) files: Vec<File>,
}

/// One `<file>` of a source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct File {
    pub(super) extension: String,
    pub(super) format: String,
    pub(super) size: u64,
    pub(super) crc32: Option<String>,
    pub(super) md5: Option<String>,
    pub(super) sha1: Option<String>,
    pub(super) header: Option<String>,
    /// `item`: the file is an extra, such as save data, not the game image.
    pub(super) item: Option<String>,
    /// `forcename`: the file name the rom takes instead of `<game>.<ext>`.
    pub(super) forcename: Option<String>,
    /// `bad="1"`: a known bad dump.
    pub(super) bad: bool,
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
    /// Bytes of its text fields, counted against a game's budget.
    pub(super) fn bytes(&self) -> usize {
        let len = |s: &Option<String>| s.as_deref().map_or(0, str::len);
        self.extension.len()
            + self.format.len()
            + len(&self.crc32)
            + len(&self.md5)
            + len(&self.sha1)
            + len(&self.header)
            + len(&self.item)
            + len(&self.forcename)
    }

    fn kind(&self) -> Kind {
        let format = self.format.trim().to_ascii_lowercase();
        match format.as_str() {
            "headerless" => Kind::Headerless,
            _ if self.headerless_extension() => Kind::Headerless,
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

    /// Whether the extension names a headerless image: `unh`, or `lyx` for Lynx.
    fn headerless_extension(&self) -> bool {
        let own = self.own_extension();
        own.eq_ignore_ascii_case("unh") || own.eq_ignore_ascii_case("lyx")
    }

    /// Whether the file is the game image: no `item`, and no extension, a headerless
    /// extension, or one of `accepted`; with nothing accepted every file without `item` counts.
    fn is_image(&self, accepted: &[&str]) -> bool {
        if self.item.is_some() {
            return false;
        }
        let own = self.own_extension();
        accepted.is_empty()
            || own.is_empty()
            || self.headerless_extension()
            || accepted.iter().any(|e| own.eq_ignore_ascii_case(e))
    }

    /// Whether the file is the headerless form of an image in `source`: its size plus the
    /// image's `header` bytes is the image's size.
    fn strips(&self, source: &Source, accepted: &[&str]) -> bool {
        self.item.is_none()
            && self.kind() == Kind::Headerless
            && source.files.iter().any(|h| {
                let header = h.header.as_deref().unwrap_or("");
                let len = header.chars().filter(char::is_ascii_hexdigit).count() as u64 / 2;
                h.kind() == Kind::Headered
                    && h.is_image(accepted)
                    && len > 0
                    && self.size.checked_add(len) == Some(h.size)
            })
    }

    fn status(&self) -> RomStatus {
        if self.bad {
            RomStatus::BadDump
        } else {
            RomStatus::Good
        }
    }
}

/// Order of preference between dumps of one file: good, then bad, then none.
fn status_rank(status: RomStatus) -> u8 {
    match status {
        RomStatus::Good | RomStatus::Verified => 0,
        RomStatus::BadDump => 1,
        RomStatus::NoDump => 2,
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

/// The rom entries of a game: its image files of the preferred kind across every source,
/// good dumps first, once per dump and per name. The images are the files
/// [`File::is_image`] accepts under the platform's extensions; when there are none, a
/// game's only distinct file without `item` is its image.
pub(super) fn roms(game: &str, sources: &[Source], options: &ExportOptions) -> Vec<DatRom> {
    let rule = options.header_rule;
    let all = || {
        sources
            .iter()
            .flat_map(|s| s.files.iter().map(move |f| (s, f)))
    };
    let headered = all()
        .map(|(_, f)| f)
        .find(|f| f.item.is_none() && f.kind() == Kind::Headered && !f.own_extension().is_empty())
        .map(File::own_extension);
    let mut accepted: Vec<&str> = options
        .load_extensions
        .iter()
        .map(|e| trim_dot(e))
        .collect();
    accepted.extend(options.extension.as_deref().map(trim_dot));
    if accepted.is_empty() {
        accepted.extend(headered);
    }
    let mut images: Vec<(&Source, &File)> = all()
        .filter(|(s, f)| f.is_image(&accepted) || f.strips(s, &accepted))
        .collect();
    let fallback = images.is_empty();
    if fallback {
        images = all().filter(|(_, f)| f.item.is_none()).collect();
        let first = images.first().map(|(_, f)| f.key());
        if images.iter().any(|(_, f)| Some(f.key()) != first) {
            return Vec::new();
        }
    }
    let Some(best) = images.iter().map(|(_, f)| rank(rule, f.kind())).min() else {
        return Vec::new();
    };
    images.retain(|(_, f)| rank(rule, f.kind()) == best);
    images.sort_by_key(|(_, f)| status_rank(f.status()));
    let sibling = headered.or_else(|| accepted.first().copied());
    let mut out: Vec<DatRom> = Vec::new();
    let mut keys: Vec<String> = Vec::new();
    for (source, file) in images {
        let key = file.key();
        if keys.contains(&key) {
            continue;
        }
        let name = rom_name(game, file, options, sibling, fallback);
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
            status: file.status(),
            header,
        });
    }
    out
}

fn trim_dot(ext: &str) -> &str {
    ext.trim().trim_start_matches('.')
}

/// The file's `forcename`, else `<game>.<ext>`. A headerless, extensionless or `rename`d
/// file takes the platform's written extension, else `sibling`.
fn rom_name(
    game: &str,
    file: &File,
    options: &ExportOptions,
    sibling: Option<&str>,
    rename: bool,
) -> String {
    if let Some(name) = file
        .forcename
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
    {
        return name.to_owned();
    }
    let own = file.own_extension();
    let ext = if rename || own.is_empty() || file.kind() == Kind::Headerless {
        options
            .extension
            .as_deref()
            .map(trim_dot)
            .or(sibling)
            .unwrap_or(own)
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
