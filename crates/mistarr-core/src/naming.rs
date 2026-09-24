//! No-Intro and Redump name parsing; the contract is `docs/VERIFICATION.md` "Name parsing".

use std::fmt;

use unicode_normalization::UnicodeNormalization;

#[cfg(test)]
mod tests;

/// A release region as written in a No-Intro or Redump region tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
// Each variant is the region its name spells; `REGIONS` gives the tag text.
#[allow(missing_docs)]
pub enum Region {
    World,
    Usa,
    Europe,
    Japan,
    Asia,
    Argentina,
    Australia,
    Austria,
    Belgium,
    Brazil,
    Canada,
    China,
    Croatia,
    Denmark,
    Finland,
    France,
    Germany,
    Greece,
    HongKong,
    India,
    Ireland,
    Israel,
    Italy,
    Korea,
    LatinAmerica,
    Mexico,
    Netherlands,
    NewZealand,
    Norway,
    Poland,
    Portugal,
    Russia,
    Scandinavia,
    Singapore,
    SouthAfrica,
    Spain,
    Sweden,
    Switzerland,
    Taiwan,
    Turkey,
    Uk,
    Unknown,
}

/// Region tag spellings, matched case-insensitively. The first spelling is canonical.
const REGIONS: &[(&str, Region)] = &[
    ("World", Region::World),
    ("USA", Region::Usa),
    ("Europe", Region::Europe),
    ("Japan", Region::Japan),
    ("Asia", Region::Asia),
    ("Argentina", Region::Argentina),
    ("Australia", Region::Australia),
    ("Austria", Region::Austria),
    ("Belgium", Region::Belgium),
    ("Brazil", Region::Brazil),
    ("Canada", Region::Canada),
    ("China", Region::China),
    ("Croatia", Region::Croatia),
    ("Denmark", Region::Denmark),
    ("Finland", Region::Finland),
    ("France", Region::France),
    ("Germany", Region::Germany),
    ("Greece", Region::Greece),
    ("Hong Kong", Region::HongKong),
    ("India", Region::India),
    ("Ireland", Region::Ireland),
    ("Israel", Region::Israel),
    ("Italy", Region::Italy),
    ("Korea", Region::Korea),
    ("Latin America", Region::LatinAmerica),
    ("Mexico", Region::Mexico),
    ("Netherlands", Region::Netherlands),
    ("New Zealand", Region::NewZealand),
    ("Norway", Region::Norway),
    ("Poland", Region::Poland),
    ("Portugal", Region::Portugal),
    ("Russia", Region::Russia),
    ("Scandinavia", Region::Scandinavia),
    ("Singapore", Region::Singapore),
    ("South Africa", Region::SouthAfrica),
    ("Spain", Region::Spain),
    ("Sweden", Region::Sweden),
    ("Switzerland", Region::Switzerland),
    ("Taiwan", Region::Taiwan),
    ("Turkey", Region::Turkey),
    ("UK", Region::Uk),
    ("United Kingdom", Region::Uk),
    ("Unknown", Region::Unknown),
];

impl Region {
    /// Looks up a region by its tag spelling, ignoring ASCII case.
    ///
    /// ```
    /// use mistarr_core::naming::Region;
    /// assert_eq!(Region::from_name("usa"), Some(Region::Usa));
    /// assert_eq!(Region::from_name("Atlantis"), None);
    /// ```
    #[must_use]
    pub fn from_name(name: &str) -> Option<Region> {
        let name = name.trim();
        REGIONS
            .iter()
            .find(|(label, _)| label.eq_ignore_ascii_case(name))
            .map(|&(_, region)| region)
    }

    /// The canonical tag spelling, e.g. `USA` or `Hong Kong`.
    ///
    /// ```
    /// use mistarr_core::naming::Region;
    /// assert_eq!(Region::HongKong.name(), "Hong Kong");
    /// ```
    #[must_use]
    pub fn name(self) -> &'static str {
        REGIONS
            .iter()
            .find(|&&(_, region)| region == self)
            .map_or("Unknown", |&(label, _)| label)
    }
}

impl fmt::Display for Region {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A classification tag on a name. `Display` gives the stored label, e.g. `unl` or `other:Kiosk`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Flag {
    /// `(BIOS)` or a `[BIOS]` prefix.
    Bios,
    /// `(Beta)` or `(Beta N)`.
    Beta,
    /// `(Proto)` or `(Proto N)`.
    Proto,
    /// `(Demo)`, `(Demo N)` or `(Kiosk)`.
    Demo,
    /// `(Sample)`.
    Sample,
    /// `(Unl)`.
    Unlicensed,
    /// `(Pirate)`.
    Pirate,
    /// `(Program)`.
    Program,
    /// `[b]` or `[bN]`.
    BadDump,
    /// Any other tag. Parenthesised tags keep their inner text, bracket tags keep the brackets.
    Other(String),
}

impl fmt::Display for Flag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Flag::Bios => f.write_str("bios"),
            Flag::Beta => f.write_str("beta"),
            Flag::Proto => f.write_str("proto"),
            Flag::Demo => f.write_str("demo"),
            Flag::Sample => f.write_str("sample"),
            Flag::Unlicensed => f.write_str("unl"),
            Flag::Pirate => f.write_str("pirate"),
            Flag::Program => f.write_str("program"),
            Flag::BadDump => f.write_str("baddump"),
            Flag::Other(tag) => write!(f, "other:{tag}"),
        }
    }
}

/// Development stage of a revision; later stages rank higher.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Stage {
    /// `(Proto N)`.
    Proto,
    /// `(Beta N)`.
    Beta,
    /// A released build.
    Release,
}

/// Sortable revision key: version first, then stage, then the proto or beta
/// number, then the alt number. An untagged name ranks as [`RevisionRank::RELEASE`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RevisionRank {
    /// Version components; `vX.Y` is `[X, Y]`, `Rev N` is `[1, N]`, untagged is `[1]`.
    pub version: [u32; 4],
    /// Development stage.
    pub stage: Stage,
    /// The N of `Beta N` or `Proto N`, 0 when absent.
    pub pre: u32,
    /// The N of `Alt N`; `(Alt)` is 1.
    pub alt: u32,
}

impl RevisionRank {
    /// Rank of a first release with no revision tag.
    pub const RELEASE: RevisionRank = RevisionRank {
        version: [1, 0, 0, 0],
        stage: Stage::Release,
        pre: 0,
        alt: 0,
    };
}

impl Default for RevisionRank {
    fn default() -> Self {
        Self::RELEASE
    }
}

/// The revision-bearing tags of a name, with their text and rank.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Revision {
    /// The tags as written, joined with `, `, e.g. `Rev 1, Alt`.
    pub label: String,
    /// Sort key.
    pub rank: RevisionRank,
}

impl fmt::Display for Revision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label)
    }
}

/// A game name split into the fields listed in `docs/VERIFICATION.md`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedName {
    /// Text before the first tag, trimmed; a leading `[BIOS]` is not part of it.
    pub base_name: String,
    /// From the first tag whose tokens are all known regions.
    pub regions: Vec<Region>,
    /// From the first tag whose tokens are all language codes such as `En` or `Zh-Hant`.
    pub languages: Vec<String>,
    /// Combined `Rev`, version, `Beta`, `Proto` and `Alt` tags.
    pub revision: Option<Revision>,
    /// Classification tags in order of appearance, without duplicates.
    pub flags: Vec<Flag>,
    /// The N of `(Disc N)`.
    pub disc: Option<u32>,
}

impl ParsedName {
    /// The revision rank, or [`RevisionRank::RELEASE`] when there is no revision tag.
    ///
    /// ```
    /// use mistarr_core::naming::{parse_name, RevisionRank};
    /// let plain = parse_name("Example Quest (USA)");
    /// let rev = parse_name("Example Quest (USA) (Rev 1)");
    /// assert_eq!(plain.revision_rank(), RevisionRank::RELEASE);
    /// assert!(rev.revision_rank() > plain.revision_rank());
    /// ```
    #[must_use]
    pub fn revision_rank(&self) -> RevisionRank {
        self.revision
            .as_ref()
            .map_or(RevisionRank::RELEASE, |rev| rev.rank)
    }

    /// Whether `flag` is present.
    ///
    /// ```
    /// use mistarr_core::naming::{parse_name, Flag};
    /// assert!(parse_name("Example Quest (USA) (Beta)").has_flag(&Flag::Beta));
    /// ```
    #[must_use]
    pub fn has_flag(&self, flag: &Flag) -> bool {
        self.flags.contains(flag)
    }

    /// Flags as stored in `titles.flags`, with `disc:N` appended when a disc tag was present.
    ///
    /// ```
    /// use mistarr_core::naming::parse_name;
    /// let parsed = parse_name("Example Saga (Europe) (Disc 2) (Unl)");
    /// assert_eq!(parsed.flag_labels(), vec!["unl", "disc:2"]);
    /// ```
    #[must_use]
    pub fn flag_labels(&self) -> Vec<String> {
        let mut labels: Vec<String> = self.flags.iter().map(ToString::to_string).collect();
        if let Some(disc) = self.disc {
            labels.push(format!("disc:{disc}"));
        }
        labels
    }
}

/// Parses a No-Intro or Redump name. Never fails; unrecognised tags become [`Flag::Other`].
///
/// ```
/// use mistarr_core::naming::{parse_name, Flag, Region};
/// let parsed = parse_name("Example Quest (USA, Europe) (En,Fr) (Rev 1) [b]");
/// assert_eq!(parsed.base_name, "Example Quest");
/// assert_eq!(parsed.regions, vec![Region::Usa, Region::Europe]);
/// assert_eq!(parsed.languages, vec!["En", "Fr"]);
/// assert_eq!(parsed.revision.map(|r| r.label).as_deref(), Some("Rev 1"));
/// assert_eq!(parsed.flags, vec![Flag::BadDump]);
/// ```
#[must_use]
pub fn parse_name(name: &str) -> ParsedName {
    let mut parsed = ParsedName::default();
    let mut rev = RevisionBuilder::default();
    let mut rest = name.trim_start();

    while let Some((tag, after)) = split_tag(rest, '[', ']') {
        apply_bracket(&mut parsed, tag);
        rest = after.trim_start();
    }

    let base_end = first_tag_start(rest).unwrap_or(rest.len());
    rest[..base_end].trim().clone_into(&mut parsed.base_name);
    rest = &rest[base_end..];

    while let Some(start) = first_tag_start(rest) {
        rest = &rest[start..];
        let (open, close) = if rest.starts_with('(') {
            ('(', ')')
        } else {
            ('[', ']')
        };
        let Some((tag, after)) = split_tag(rest, open, close) else {
            break;
        };
        if open == '(' {
            apply_paren(&mut parsed, &mut rev, tag);
        } else {
            apply_bracket(&mut parsed, tag);
        }
        rest = after;
    }

    parsed.revision = rev.build();
    parsed
}

/// Byte offset of the first `(` or `[` that opens a closed tag.
fn first_tag_start(s: &str) -> Option<usize> {
    s.char_indices().find_map(|(i, c)| {
        let close = match c {
            '(' => ')',
            '[' => ']',
            _ => return None,
        };
        split_tag(&s[i..], c, close).map(|_| i)
    })
}

/// For `s` starting with `open`, returns the inner text up to the matching `close` and the rest.
fn split_tag(s: &str, open: char, close: char) -> Option<(&str, &str)> {
    let inner = s.strip_prefix(open)?;
    let mut depth = 0usize;
    for (i, c) in inner.char_indices() {
        if c == open {
            depth += 1;
        } else if c == close {
            if depth == 0 {
                return Some((&inner[..i], &inner[i + close.len_utf8()..]));
            }
            depth -= 1;
        }
    }
    None
}

fn push_flag(parsed: &mut ParsedName, flag: Flag) {
    if !parsed.flags.contains(&flag) {
        parsed.flags.push(flag);
    }
}

fn apply_bracket(parsed: &mut ParsedName, tag: &str) {
    let trimmed = tag.trim();
    let flag = if is_bad_dump(trimmed) {
        Flag::BadDump
    } else if trimmed.eq_ignore_ascii_case("bios") {
        Flag::Bios
    } else {
        Flag::Other(format!("[{trimmed}]"))
    };
    push_flag(parsed, flag);
}

fn is_bad_dump(tag: &str) -> bool {
    tag.strip_prefix('b')
        .is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit()))
}

fn apply_paren(parsed: &mut ParsedName, rev: &mut RevisionBuilder, tag: &str) {
    let tag = tag.trim();
    if tag.is_empty() {
        return;
    }
    if parsed.regions.is_empty() {
        if let Some(regions) = parse_regions(tag) {
            parsed.regions = regions;
            return;
        }
    }
    if parsed.languages.is_empty() {
        if let Some(languages) = parse_languages(tag) {
            parsed.languages = languages;
            return;
        }
    }
    let (word, arg) = match tag.split_once(char::is_whitespace) {
        Some((word, arg)) => (word, Some(arg.trim())),
        None => (tag, None),
    };
    let word = word.to_ascii_lowercase();
    let simple = |flag: Flag| arg.is_none().then_some(flag);
    let flag = match word.as_str() {
        "rev" => arg.and_then(parse_rev).map(|version| {
            rev.version(tag, version);
            None
        }),
        "version" => arg.and_then(parse_version).map(|version| {
            rev.version(tag, version);
            None
        }),
        "beta" => opt_number(arg).map(|n| {
            rev.pre(tag, Stage::Beta, n);
            Some(Flag::Beta)
        }),
        "proto" | "prototype" => opt_number(arg).map(|n| {
            rev.pre(tag, Stage::Proto, n);
            Some(Flag::Proto)
        }),
        "alt" => opt_number(arg).map(|n| {
            rev.alt(tag, n.unwrap_or(1));
            None
        }),
        "disc" => arg.and_then(parse_number).map(|n| {
            parsed.disc = Some(n);
            None
        }),
        "demo" => opt_number(arg).map(|_| Some(Flag::Demo)),
        "kiosk" => Some(Some(Flag::Demo)),
        "sample" => simple(Flag::Sample).map(Some),
        "unl" | "unlicensed" => simple(Flag::Unlicensed).map(Some),
        "pirate" => simple(Flag::Pirate).map(Some),
        "program" => simple(Flag::Program).map(Some),
        "bios" => simple(Flag::Bios).map(Some),
        _ => parse_version_word(&word).map(|version| {
            if arg.is_none() {
                rev.version(tag, version);
                None
            } else {
                Some(Flag::Other(tag.to_owned()))
            }
        }),
    };
    match flag {
        Some(Some(flag)) => push_flag(parsed, flag),
        Some(None) => {}
        None => push_flag(parsed, Flag::Other(tag.to_owned())),
    }
}

fn parse_regions(tag: &str) -> Option<Vec<Region>> {
    let mut regions = Vec::new();
    for token in tag.split(',') {
        let region = Region::from_name(token)?;
        if !regions.contains(&region) {
            regions.push(region);
        }
    }
    Some(regions)
}

fn parse_languages(tag: &str) -> Option<Vec<String>> {
    tag.split(',')
        .map(|token| {
            let token = token.trim();
            is_language_code(token).then(|| token.to_owned())
        })
        .collect()
}

/// `Xx`, optionally followed by a subtag of two to four ASCII letters, e.g. `Zh-Hant`.
fn is_language_code(token: &str) -> bool {
    let (primary, subtag) = match token.split_once('-') {
        Some((primary, subtag)) => (primary, Some(subtag)),
        None => (token, None),
    };
    let b = primary.as_bytes();
    let primary_ok = b.len() == 2 && b[0].is_ascii_uppercase() && b[1].is_ascii_lowercase();
    let subtag_ok = subtag.map_or(true, |s| {
        (2..=4).contains(&s.len()) && s.bytes().all(|c| c.is_ascii_alphabetic())
    });
    primary_ok && subtag_ok
}

fn parse_number(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// `None` for a malformed argument, `Some(None)` for no argument, `Some(Some(n))` for a number.
// The outer option is validity, the inner one presence.
#[allow(clippy::option_option)]
fn opt_number(arg: Option<&str>) -> Option<Option<u32>> {
    match arg {
        None => Some(None),
        Some(s) => parse_number(s).map(Some),
    }
}

/// `Rev 1` is `[1, 1]`, `Rev B` is `[1, 2]`, `Rev 1.2` is `[1, 1, 2]`.
fn parse_rev(arg: &str) -> Option<[u32; 4]> {
    if !arg.is_empty() && arg.bytes().all(|b| b.is_ascii_alphabetic()) {
        let n = arg.bytes().fold(0u32, |acc, b| {
            acc.saturating_mul(26)
                .saturating_add(u32::from(b.to_ascii_uppercase() - b'A' + 1))
        });
        return Some([1, n, 0, 0]);
    }
    let (dotted, _) = parse_dotted(arg)?;
    Some([1, dotted[0], dotted[1], dotted[2]])
}

/// `v1.1`, `v2.0.1` or `v1.1a`, lowercase `v` already applied to `word`.
fn parse_version_word(word: &str) -> Option<[u32; 4]> {
    parse_version(word.strip_prefix('v')?)
}

/// Dotted digits with an optional trailing letter that becomes one more component.
fn parse_version(arg: &str) -> Option<[u32; 4]> {
    let (digits, letter) = match arg.as_bytes().last() {
        Some(&b) if b.is_ascii_alphabetic() => (&arg[..arg.len() - 1], Some(b)),
        _ => (arg, None),
    };
    let (mut version, count) = parse_dotted(digits)?;
    if let (Some(letter), Some(slot)) = (letter, version.get_mut(count)) {
        *slot = u32::from(letter.to_ascii_lowercase() - b'a' + 1);
    }
    Some(version)
}

/// Up to four dotted numbers and how many were given.
fn parse_dotted(s: &str) -> Option<([u32; 4], usize)> {
    let mut out = [0u32; 4];
    let mut count = 0;
    for part in s.split('.') {
        let n = parse_number(part)?;
        if let Some(slot) = out.get_mut(count) {
            *slot = n;
        }
        count += 1;
    }
    Some((out, count))
}

#[derive(Default)]
struct RevisionBuilder {
    labels: Vec<String>,
    version: Option<[u32; 4]>,
    stage: Option<(Stage, u32)>,
    alt: Option<u32>,
}

impl RevisionBuilder {
    fn version(&mut self, label: &str, version: [u32; 4]) {
        self.labels.push(label.to_owned());
        self.version = Some(version);
    }

    fn pre(&mut self, label: &str, stage: Stage, n: Option<u32>) {
        self.labels.push(label.to_owned());
        self.stage = Some((stage, n.unwrap_or(0)));
    }

    fn alt(&mut self, label: &str, n: u32) {
        self.labels.push(label.to_owned());
        self.alt = Some(n);
    }

    fn build(self) -> Option<Revision> {
        if self.labels.is_empty() {
            return None;
        }
        let (stage, pre) = self.stage.unwrap_or((Stage::Release, 0));
        Some(Revision {
            label: self.labels.join(", "),
            rank: RevisionRank {
                version: self.version.unwrap_or(RevisionRank::RELEASE.version),
                stage,
                pre,
                alt: self.alt.unwrap_or(0),
            },
        })
    }
}

/// Characters replaced with `_`, as libretro does for thumbnail and playlist names.
const LIBRETRO_SUBSTITUTIONS: &[char] = &['&', '*', '/', ':', '`', '<', '>', '?', '\\', '|', '"'];

/// Normalises a name for pre-download matching: NFKC, lowercase, libretro
/// substitutions to `_`, whitespace runs collapsed to one space and trimmed. Idempotent.
///
/// ```
/// use mistarr_core::naming::normalize_for_match;
/// assert_eq!(normalize_for_match("  Example:  Quest ＆ Co "), "example_ quest _ co");
/// ```
#[must_use]
pub fn normalize_for_match(name: &str) -> String {
    let folded: String = name.nfkc().flat_map(char::to_lowercase).collect();
    let folded: String = folded.nfkc().flat_map(char::to_lowercase).collect();
    let mut out = String::with_capacity(folded.len());
    for word in folded.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.extend(word.chars().map(|c| {
            if LIBRETRO_SUBSTITUTIONS.contains(&c) {
                '_'
            } else {
                c
            }
        }));
    }
    out
}

/// Clone-group key for a title whose DAT gives no `cloneof`: the normalised base name.
/// Callers pair it with the platform id, as `docs/VERIFICATION.md` "Clone grouping" says.
///
/// ```
/// use mistarr_core::naming::{group_key, parse_name};
/// let a = group_key(&parse_name("Example Quest (USA) (Rev 1)"));
/// let b = group_key(&parse_name("Example Quest (Japan) (Disc 2)"));
/// assert_eq!(a, b);
/// assert_eq!(a, "example quest");
/// ```
#[must_use]
pub fn group_key(parsed: &ParsedName) -> String {
    normalize_for_match(&parsed.base_name)
}
