//! The fuzzy and size-only tiers of pre-download mapping, run on the files
//! the name tiers left unmatched; see `docs/VERIFICATION.md` "Pre-download matching".

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};

use crate::binding::{base_name, normalise_name, Confidence, RomRef};
use crate::torrent::TorrentFile;

/// Most title groups one file may name by the fuzzy tier; a file naming
/// more is ambiguous and gets no fuzzy candidate.
pub const MAX_FUZZY_GROUPS: usize = 4;

/// Most roms of one size the size-only tier accepts.
pub const MAX_SIZE_ONLY: usize = 4;

/// A rom a file of some size may be, as [`SizeIndex`] gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SizedRom {
    /// The rom.
    pub rom: RomRef,
    /// Its `match_base`: the normalised name before the first tag.
    pub base: String,
    /// Its title's clone group, so versions of one entry count once.
    pub group: i64,
}

/// The roms of the bound platform by size, for the fuzzy tiers.
pub trait SizeIndex {
    /// Live, non-BIOS roms a file of `size` bytes may be: those of that
    /// size, or of that size less a header the platform's hashing skips.
    fn roms_of_size(&self, size: u64) -> Vec<SizedRom>;
}

/// True for a version tag, which says nothing about which title a name is:
/// `v2`, `v1.0.6`, `1.0.6`, `rev`, `ver` or `version`.
///
/// ```
/// use mistarr_sources::fuzzy::is_version_tag;
/// assert!(is_version_tag("v10") && is_version_tag("1.0.6") && is_version_tag("rev"));
/// assert!(!is_version_tag("2") && !is_version_tag("v") && !is_version_tag("nova"));
/// ```
#[must_use]
pub fn is_version_tag(chunk: &str) -> bool {
    let dotted = |s: &str| {
        let parts: Vec<&str> = s.split('.').collect();
        parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    };
    matches!(chunk, "rev" | "ver" | "version")
        || chunk.strip_prefix('v').is_some_and(dotted)
        || (chunk.contains('.') && dotted(chunk))
}

/// True for a word that tells sequels apart: a number or a roman numeral
/// from `ii` to `xx`.
///
/// ```
/// use mistarr_sources::fuzzy::is_sequel;
/// assert!(is_sequel("2") && is_sequel("iii") && is_sequel("xiv"));
/// assert!(!is_sequel("quest") && !is_sequel("mix"));
/// ```
#[must_use]
pub fn is_sequel(word: &str) -> bool {
    const ROMAN: [&str; 19] = [
        "ii", "iii", "iv", "v", "vi", "vii", "viii", "ix", "x", "xi", "xii", "xiii", "xiv", "xv",
        "xvi", "xvii", "xviii", "xix", "xx",
    ];
    (!word.is_empty() && word.bytes().all(|b| b.is_ascii_digit())) || ROMAN.contains(&word)
}

/// The words of a name that say which title it is: the part before the
/// first `(` or `[` tag, lowercased, split on anything but letters and
/// digits, without version tags (a trailing dotted version is cut from a word).
///
/// ```
/// use mistarr_sources::fuzzy::words;
/// assert_eq!(words("example_quest_v2"), ["example", "quest"]);
/// assert_eq!(words("Quest 2 (USA)"), ["quest", "2"]);
/// assert_eq!(words("novathesquirrel1.0.6"), ["novathesquirrel"]);
/// ```
#[must_use]
pub fn words(name: &str) -> Vec<String> {
    let lowered = name.to_lowercase();
    let untagged = lowered.split(['(', '[']).next().unwrap_or_default();
    let mut out = Vec::new();
    let mut after_rev = false;
    for chunk in untagged.split(|c: char| c.is_whitespace() || matches!(c, '_' | '-' | '+')) {
        let chunk = chunk.trim_matches('.');
        let chunk = if is_version_tag(chunk) {
            chunk
        } else {
            cut_version(chunk)
        };
        if chunk.is_empty() || is_version_tag(chunk) {
            after_rev = chunk == "rev";
            continue;
        }
        let short = chunk.len() == 1 || chunk.bytes().all(|b| b.is_ascii_digit());
        if after_rev && short {
            after_rev = false;
            continue;
        }
        after_rev = false;
        for w in chunk.split(|c: char| !c.is_alphanumeric()) {
            if !w.is_empty() {
                out.push(w.to_owned());
            }
        }
    }
    out
}

/// `chunk` without a trailing version glued to it, as in `name1.0.6` or `name.v2`.
fn cut_version(chunk: &str) -> &str {
    for (i, c) in chunk.char_indices().skip(1) {
        let rest = &chunk[i..];
        let glued = c.is_ascii_digit() && rest.contains('.') && is_version_tag(rest);
        let dotted_v = rest.starts_with(".v") && is_version_tag(&rest[1..]);
        if glued || dotted_v {
            return chunk[..i].trim_end_matches('.');
        }
    }
    chunk
}

/// How two word lists relate, strongest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Signal {
    /// The same words, or the same letters once run together.
    Equal,
    /// The file's words lead the rom's, and the rom's next word is no sequel number.
    Prefix,
    /// No signal.
    None,
}

/// The signal between a file's words and a rom's base words: equal, or the
/// file's a leading prefix of the rom's (`nova` of "nova the squirrel")
/// that does not stop before a sequel number (`quest` of "quest 2").
///
/// ```
/// use mistarr_sources::fuzzy::{signal, words as w, Signal};
/// assert_eq!(signal(&w("nova"), &w("nova the squirrel")), Signal::Prefix);
/// assert_eq!(signal(&w("novathesquirrel"), &w("nova the squirrel")), Signal::Equal);
/// assert_eq!(signal(&w("quest 2"), &w("quest 3")), Signal::None);
/// assert_eq!(signal(&w("quest"), &w("quest 2")), Signal::None);
/// assert_eq!(signal(&w("quest of kings"), &w("quest")), Signal::None);
/// ```
#[must_use]
pub fn signal(file: &[String], rom: &[String]) -> Signal {
    if file.is_empty() || rom.is_empty() {
        return Signal::None;
    }
    if file == rom || file.concat() == rom.concat() {
        return Signal::Equal;
    }
    if file.len() < rom.len() && rom.starts_with(file) && !is_sequel(&rom[file.len()]) {
        return Signal::Prefix;
    }
    Signal::None
}

/// The fuzzy and size-only candidates of `unmatched`, the files of `files`
/// the name tiers left unmatched, among those whose extension is in `extensions`.
///
/// A file's words are its stem's ([`words`]) when no other considered file
/// has the same, else its parent directory's when no other considered file
/// has those; the directory's words are also tried when the stem's find
/// nothing. It names the roms of its size whose words are equal to them, or
/// else those they lead ([`signal`]), as [`Confidence::Fuzzy`], unless those
/// roms span more than [`MAX_FUZZY_GROUPS`] clone groups. When that finds
/// nothing and the file is the only one of `files` with a considered
/// extension, it names every rom of its size as [`Confidence::Size`] if
/// there are 1 to [`MAX_SIZE_ONLY`].
///
/// ```
/// use mistarr_sources::binding::{Confidence, RomRef};
/// use mistarr_sources::fuzzy::{candidates, SizeIndex, SizedRom};
/// use mistarr_sources::torrent::TorrentFile;
///
/// struct Nova;
/// impl SizeIndex for Nova {
///     fn roms_of_size(&self, size: u64) -> Vec<SizedRom> {
///         if size != 16 { return Vec::new(); }
///         let rom = |id, base: &str| SizedRom { rom: RomRef(id), base: base.into(), group: id };
///         vec![rom(1, "nova the squirrel"), rom(2, "other tale")]
///     }
/// }
/// let files = vec![TorrentFile { index: 3, path: "nova.nes".into(), size: 16 }];
/// let unmatched: Vec<&TorrentFile> = files.iter().collect();
/// let found = candidates(&files, &unmatched, &["nes"], &Nova);
/// assert_eq!(found, vec![(3, RomRef(1), Confidence::Fuzzy)]);
/// ```
#[must_use]
pub fn candidates(
    files: &[TorrentFile],
    unmatched: &[&TorrentFile],
    extensions: &[&str],
    index: &dyn SizeIndex,
) -> Vec<(u32, RomRef, Confidence)> {
    let considered = |f: &TorrentFile| f.size > 0 && has_extension(&f.path, extensions);
    let mut by_size: BTreeMap<u64, Vec<&TorrentFile>> = BTreeMap::new();
    let mut stems: HashMap<u64, u32> = HashMap::new();
    let mut dirs: HashMap<u64, u32> = HashMap::new();
    for file in unmatched.iter().copied().filter(|f| considered(f)) {
        by_size.entry(file.size).or_default().push(file);
        stems.insert(key(&stem_words(&file.path)), 0);
        dirs.insert(key(&dir_words(&file.path)), 0);
    }
    if by_size.is_empty() {
        return Vec::new();
    }
    // Only the words of unmatched files are counted, so a large torrent adds no map entry per file.
    let mut total = 0usize;
    for f in files.iter().filter(|f| considered(f)) {
        total += 1;
        if let Some(n) = stems.get_mut(&key(&stem_words(&f.path))) {
            *n += 1;
        }
        if let Some(n) = dirs.get_mut(&key(&dir_words(&f.path))) {
            *n += 1;
        }
    }
    let unique = |counts: &HashMap<u64, u32>, w: Vec<String>| {
        (!w.is_empty() && counts.get(&key(&w)) == Some(&1)).then_some(w)
    };
    let mut out = Vec::new();
    for (size, group_files) in by_size {
        let roms = index.roms_of_size(size);
        if roms.is_empty() {
            continue;
        }
        let group = Group::new(&roms);
        for file in group_files {
            let tries = [
                unique(&stems, stem_words(&file.path)),
                unique(&dirs, dir_words(&file.path)),
            ];
            let found = tries
                .into_iter()
                .flatten()
                .map(|w| group.matches(&w))
                .find(|m| !m.is_empty())
                .unwrap_or_default();
            if !found.is_empty() {
                out.extend(
                    found
                        .into_iter()
                        .map(|r| (file.index, r, Confidence::Fuzzy)),
                );
            } else if total == 1 && roms.len() <= MAX_SIZE_ONLY {
                out.extend(roms.iter().map(|r| (file.index, r.rom, Confidence::Size)));
            }
        }
    }
    out
}

fn key(words: &[String]) -> u64 {
    let mut h = DefaultHasher::new();
    words.hash(&mut h);
    h.finish()
}

fn stem_words(path: &str) -> Vec<String> {
    let leaf = path.rsplit('/').next().unwrap_or(path);
    words(base_name(&normalise_name(leaf)))
}

fn dir_words(path: &str) -> Vec<String> {
    let mut parts = path.rsplit('/');
    parts.next();
    parts
        .next()
        .map_or_else(Vec::new, |d| words(base_name(&d.to_lowercase())))
}

fn has_extension(path: &str, extensions: &[&str]) -> bool {
    let leaf = path.rsplit('/').next().unwrap_or(path);
    leaf.rsplit_once('.')
        .is_some_and(|(_, ext)| extensions.iter().any(|e| e.eq_ignore_ascii_case(ext)))
}

/// The roms of one size, grouped by their words, with lookups by the whole
/// word list, the letters run together, and every leading prefix a file may give.
struct Group<'r> {
    sets: Vec<(Vec<String>, Vec<&'r SizedRom>)>,
    exact: HashMap<String, Vec<usize>>,
    joined: HashMap<String, Vec<usize>>,
    prefix: HashMap<String, Vec<usize>>,
}

impl<'r> Group<'r> {
    fn new(roms: &'r [SizedRom]) -> Self {
        let mut position: HashMap<Vec<String>, usize> = HashMap::new();
        let mut sets: Vec<(Vec<String>, Vec<&SizedRom>)> = Vec::new();
        for rom in roms {
            let w = words(&rom.base);
            if w.is_empty() {
                continue;
            }
            let at = *position.entry(w.clone()).or_insert_with(|| {
                sets.push((w, Vec::new()));
                sets.len() - 1
            });
            sets[at].1.push(rom);
        }
        let (mut exact, mut joined, mut prefix) = (HashMap::new(), HashMap::new(), HashMap::new());
        for (i, (w, _)) in sets.iter().enumerate() {
            exact.entry(w.join(" ")).or_insert_with(Vec::new).push(i);
            joined.entry(w.concat()).or_insert_with(Vec::new).push(i);
            for n in 1..w.len() {
                if !is_sequel(&w[n]) {
                    prefix
                        .entry(w[..n].join(" "))
                        .or_insert_with(Vec::new)
                        .push(i);
                }
            }
        }
        Self {
            sets,
            exact,
            joined,
            prefix,
        }
    }

    /// The roms `words` name: equal ones when there are any, else those
    /// they lead; none when they span more than [`MAX_FUZZY_GROUPS`] groups.
    fn matches(&self, words: &[String]) -> Vec<RomRef> {
        let lookup = |map: &HashMap<String, Vec<usize>>, k: &str| {
            map.get(k).map_or(&[][..], Vec::as_slice).to_vec()
        };
        let mut hits = lookup(&self.exact, &words.join(" "));
        hits.extend(lookup(&self.joined, &words.concat()));
        if hits.is_empty() {
            hits = lookup(&self.prefix, &words.join(" "));
        }
        hits.sort_unstable();
        hits.dedup();
        let roms: Vec<&SizedRom> = hits
            .iter()
            .flat_map(|&i| self.sets[i].1.iter().copied())
            .collect();
        let groups: HashSet<i64> = roms.iter().map(|r| r.group).collect();
        if groups.len() > MAX_FUZZY_GROUPS {
            return Vec::new();
        }
        let mut out: Vec<RomRef> = roms.iter().map(|r| r.rom).collect();
        out.sort_unstable();
        out
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    struct Roms(Vec<(SizedRom, u64)>);

    impl SizeIndex for Roms {
        fn roms_of_size(&self, size: u64) -> Vec<SizedRom> {
            self.0
                .iter()
                .filter(|(_, s)| *s == size)
                .map(|(r, _)| r.clone())
                .collect()
        }
    }

    fn rom(id: i64, base: &str, group: i64, size: u64) -> (SizedRom, u64) {
        let r = SizedRom {
            rom: RomRef(id),
            base: base.to_owned(),
            group,
        };
        (r, size)
    }

    fn file(index: u32, path: &str, size: u64) -> TorrentFile {
        TorrentFile {
            index,
            path: path.to_owned(),
            size,
        }
    }

    /// Runs [`candidates`] with every file unmatched.
    fn run(files: &[TorrentFile], roms: &Roms) -> Vec<(u32, RomRef, Confidence)> {
        let unmatched: Vec<&TorrentFile> = files.iter().collect();
        candidates(files, &unmatched, &["nes"], roms)
    }

    fn nova() -> Roms {
        Roms(vec![
            rom(1, "nova the squirrel", 1, 262_160),
            rom(2, "nova the squirrel", 1, 262_160),
            rom(3, "example quest", 3, 262_160),
        ])
    }

    #[test]
    fn a_short_stem_names_both_versions_of_its_size() {
        let files = [
            file(0, "nova.nes", 262_160),
            file(1, "nova.png", 262_160),
            file(2, "nova.sqlite", 9),
        ];
        assert_eq!(
            run(&files, &nova()),
            [
                (0, RomRef(1), Confidence::Fuzzy),
                (0, RomRef(2), Confidence::Fuzzy)
            ]
        );
    }

    #[test]
    fn only_unmatched_files_are_mapped() {
        let files = [
            file(0, "nova.nes", 262_160),
            file(1, "example quest.nes", 262_160),
        ];
        let unmatched = [&files[0]];
        let found = candidates(&files, &unmatched, &["nes"], &nova());
        assert!(found.iter().all(|(i, _, _)| *i == 0));
    }

    #[test]
    fn a_sole_rom_file_without_a_name_signal_falls_back_to_size() {
        let files = [file(0, "Game/rom.NES", 262_160), file(1, "a.txt", 4)];
        let found = run(&files, &nova());
        assert_eq!(found.len(), 3);
        assert!(found
            .iter()
            .all(|(i, _, c)| *i == 0 && *c == Confidence::Size));
        let crowded = Roms(
            (0..=5)
                .map(|i| rom(i, &format!("title {i}"), i, 8))
                .collect(),
        );
        assert!(run(&[file(0, "x.nes", 8)], &crowded).is_empty());
    }

    #[test]
    fn size_alone_needs_the_only_rom_file_of_the_torrent() {
        let files = [file(0, "rom.nes", 262_160), file(1, "extra.nes", 4)];
        let unmatched = [&files[0]];
        assert!(candidates(&files, &unmatched, &["nes"], &nova()).is_empty());
    }

    #[test]
    fn a_set_of_same_sized_files_gets_no_size_only_candidates() {
        let roms = Roms(
            (0..2_000)
                .map(|i| rom(i, &format!("title {}", i % 50), i % 50, 40_976))
                .collect(),
        );
        let files: Vec<TorrentFile> = (0..5_000)
            .map(|i| file(i, &format!("Set/unnamed {i}.nes"), 40_976))
            .collect();
        assert!(run(&files, &roms).is_empty());
    }

    #[test]
    fn sequels_are_told_apart() {
        let roms = Roms(vec![
            rom(1, "quest", 1, 8),
            rom(2, "quest 2", 2, 8),
            rom(3, "quest 3", 3, 8),
            rom(4, "quest iii", 4, 8),
        ]);
        let names = |p: &str| -> Vec<i64> {
            run(&[file(0, p, 8), file(1, "zzz.nes", 99)], &roms)
                .iter()
                .map(|(_, r, _)| r.0)
                .collect()
        };
        assert_eq!(names("quest_2.nes"), [2]);
        assert_eq!(names("Quest.nes"), [1]);
        assert_eq!(names("quest III.nes"), [4]);
        assert_eq!(names("quest 4.nes"), Vec::<i64>::new());
    }

    #[test]
    fn a_one_word_rom_never_names_a_longer_file() {
        let roms = Roms(vec![rom(1, "the", 1, 8), rom(2, "of", 2, 8)]);
        let files = [file(0, "the legend of quest.nes", 8), file(1, "b.nes", 1)];
        assert!(run(&files, &roms).is_empty());
        let roms = Roms(vec![rom(1, "mega", 1, 8)]);
        assert!(run(&[file(0, "mega quest.nes", 8), file(1, "b.nes", 1)], &roms).is_empty());
    }

    #[test]
    fn a_shared_leaf_uses_its_directory_or_nothing() {
        let roms = Roms(vec![
            rom(1, "nova the squirrel", 1, 8),
            rom(2, "example quest", 2, 8),
        ]);
        let files = [
            file(0, "Nova the Squirrel/game.nes", 8),
            file(1, "Example Quest/game.nes", 8),
        ];
        let found = run(&files, &roms);
        assert_eq!(
            found,
            [
                (0, RomRef(1), Confidence::Fuzzy),
                (1, RomRef(2), Confidence::Fuzzy)
            ]
        );
        let flat = [file(0, "a/game.nes", 8), file(1, "a/game.nes", 8)];
        assert!(run(&flat, &Roms(vec![rom(1, "game", 1, 8)])).is_empty());
    }

    #[test]
    fn many_groups_are_ambiguous_but_versions_of_one_count_once() {
        let many = Roms(
            ["alpha", "beta", "gamma", "delta", "omega"]
                .iter()
                .zip(0..)
                .map(|(w, i)| rom(i, &format!("example quest {w}"), i, 8))
                .collect(),
        );
        assert!(run(&[file(0, "example.nes", 8)], &many).is_empty());
        let versions = Roms((0..10).map(|i| rom(i, "example quest", 7, 8)).collect());
        assert_eq!(run(&[file(0, "example.nes", 8)], &versions).len(), 10);
    }

    #[test]
    fn version_tags_and_other_extensions_are_ignored() {
        let roms = Roms(vec![rom(1, "example", 1, 8), rom(2, "tale", 2, 8)]);
        let v = [file(0, "v2 1.0.nes", 8), file(1, "other.nes", 8)];
        assert!(run(&v, &roms).is_empty());
        assert!(run(&[file(0, "example.zip", 8)], &roms).is_empty());
        assert!(run(&[file(0, "example.nes", 0)], &roms).is_empty());
        assert_eq!(run(&[file(0, "example_v1.2.nes", 8)], &roms).len(), 1);
    }

    #[test]
    fn words_and_tags() {
        assert_eq!(words("Rev 1 quest"), ["quest"]);
        assert_eq!(words("A-a b_B 3"), ["a", "a", "b", "b", "3"]);
        assert!(words("1.0.6 v2").is_empty());
        assert_eq!(words("name.v1.2"), ["name"]);
        assert!(is_version_tag("version") && !is_version_tag("1") && !is_version_tag(""));
        assert_eq!(cut_version("abc1.0"), "abc");
        assert_eq!(cut_version("1.0"), "1.0");
        assert_eq!(cut_version("name.v2"), "name");
        assert_eq!(cut_version("abc1"), "abc1");
    }

    fn word() -> impl Strategy<Value = String> {
        "[a-z]{3,6}"
    }

    /// Words that are neither sequel numbers nor version tags.
    fn plain(ws: &[String]) -> bool {
        ws.iter().all(|w| !is_sequel(w) && !is_version_tag(w))
    }

    proptest! {
        #[test]
        fn a_leading_prefix_of_the_title_signals(ws in prop::collection::vec(word(), 1..6),
                                                 take in 1usize..6, sep in "[ _.-]") {
            prop_assume!(plain(&ws));
            let take = take.min(ws.len());
            let stem = ws[..take].join(&sep);
            let got = signal(&words(&stem), &words(&ws.join(" ")));
            let want = if take == ws.len() { Signal::Equal } else { Signal::Prefix };
            prop_assert!(got <= want);
            prop_assert_ne!(got, Signal::None);
        }

        #[test]
        fn different_sequel_numbers_never_signal(ws in prop::collection::vec(word(), 1..4),
                                                 a in 1u32..30, b in 1u32..30) {
            prop_assume!(a != b && plain(&ws));
            let base = ws.join(" ");
            let (x, y) = (words(&format!("{base} {a}")), words(&format!("{base} {b}")));
            prop_assert_eq!(signal(&x, &y), Signal::None);
            prop_assert_eq!(signal(&words(&base), &y), Signal::None);
        }

        #[test]
        fn a_longer_file_never_signals_a_shorter_rom(ws in prop::collection::vec(word(), 2..6),
                                                     keep in 1usize..5) {
            prop_assume!(plain(&ws));
            let keep = keep.min(ws.len() - 1);
            let rom = words(&ws[..keep].join(" "));
            prop_assert_eq!(signal(&words(&ws.join(" ")), &rom), Signal::None);
        }

        #[test]
        fn version_tags_never_change_the_words(ws in prop::collection::vec(word(), 1..4),
                                               tag in prop_oneof!["v[0-9]{1,2}", "[0-9]\\.[0-9]{1,2}", "v[0-9]\\.[0-9]"]) {
            prop_assume!(plain(&ws));
            prop_assert_eq!(words(&format!("{}_{tag}", ws.join("_"))), words(&ws.join(" ")));
        }

        #[test]
        fn disjoint_words_never_signal(a in prop::collection::btree_set("[a-z]{3}", 1..4),
                                       b in prop::collection::btree_set("[a-z]{3}", 1..4)) {
            prop_assume!(a.is_disjoint(&b));
            let a: Vec<String> = a.into_iter().collect();
            let b: Vec<String> = b.into_iter().collect();
            prop_assert_eq!(signal(&a, &b), Signal::None);
        }

        #[test]
        fn candidates_share_the_size_and_a_signal(
            roms in prop::collection::vec((prop::collection::vec(word(), 1..3), 1u64..4), 0..30),
            files in prop::collection::vec((prop::collection::vec(word(), 1..3), 1u64..4), 0..10),
        ) {
            let index = Roms(roms.iter().enumerate()
                .map(|(i, (w, s))| {
                    let id = i64::try_from(i).unwrap_or(0);
                    rom(id, &w.join(" "), id, *s)
                })
                .collect());
            let files: Vec<TorrentFile> = files.iter().enumerate()
                .map(|(i, (w, s))| file(u32::try_from(i).unwrap_or(0), &format!("{}.nes", w.join("_")), *s))
                .collect();
            let found = run(&files, &index);
            for (i, r, confidence) in &found {
                let f = &files[*i as usize];
                let (ws, size) = &roms[usize::try_from(r.0).unwrap_or(0)];
                prop_assert_eq!(*size, f.size);
                if *confidence == Confidence::Fuzzy {
                    prop_assert_ne!(signal(&stem_words(&f.path), &words(&ws.join(" "))), Signal::None);
                } else {
                    prop_assert_eq!(*confidence, Confidence::Size);
                    prop_assert_eq!(files.len(), 1);
                }
            }
            let mut per_file: HashMap<u32, usize> = HashMap::new();
            for (i, _, c) in &found {
                if *c == Confidence::Fuzzy {
                    *per_file.entry(*i).or_default() += 1;
                }
            }
            prop_assert!(per_file.values().all(|n| *n <= MAX_FUZZY_GROUPS));
        }
    }
}
