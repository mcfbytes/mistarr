//! The fuzzy and size-only tiers of pre-download mapping, run on the files
//! the name tiers left unmatched; see `docs/VERIFICATION.md` "Pre-download matching".

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::binding::{normalise_name, Confidence, RomRef};
use crate::torrent::TorrentFile;

/// Most roms one file may name by the fuzzy tier; a file naming more is
/// ambiguous and gets no fuzzy candidate.
pub const MAX_FUZZY: usize = 8;

/// Most roms of one size the size-only tier accepts.
pub const MAX_SIZE_ONLY: usize = 4;

/// The roms of the bound platform by size, for the fuzzy tiers.
pub trait SizeIndex {
    /// Live, non-BIOS roms a file of `size` bytes may be, each with its
    /// `match_base`: those of that size, or of that size less a header the
    /// platform's hashing skips.
    fn roms_of_size(&self, size: u64) -> Vec<(RomRef, String)>;
}

/// True for a token that says nothing about a title on its own: only
/// digits, or a version tag such as `v2`, `rev`, `ver` or `version`.
///
/// ```
/// use mistarr_sources::fuzzy::is_filler;
/// assert!(is_filler("2") && is_filler("v10") && is_filler("rev"));
/// assert!(!is_filler("nova") && !is_filler("v") && !is_filler("2d"));
/// ```
#[must_use]
pub fn is_filler(token: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    digits(token)
        || token.strip_prefix('v').is_some_and(digits)
        || matches!(token, "rev" | "ver" | "version")
}

/// The significant words of a name: lowercased runs of letters and digits,
/// without fillers ([`is_filler`]), deduplicated, in first-seen order.
///
/// ```
/// use mistarr_sources::fuzzy::significant_tokens;
/// assert_eq!(significant_tokens("example_quest_v2"), ["example", "quest"]);
/// assert_eq!(significant_tokens("Nova the Squirrel 1.0.6"), ["nova", "the", "squirrel"]);
/// ```
#[must_use]
pub fn significant_tokens(name: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for token in name.split(|c: char| !c.is_alphanumeric()) {
        let token = token.to_lowercase();
        if !token.is_empty() && !is_filler(&token) && !out.contains(&token) {
            out.push(token);
        }
    }
    out
}

/// Whether two names share a name signal: the significant words of one are
/// a non-empty subset of the other's (a prefix is one), or both spell the
/// same letters once the spaces between their words are gone.
///
/// ```
/// use mistarr_sources::fuzzy::{name_signal, significant_tokens as t};
/// assert!(name_signal(&t("nova"), &t("nova the squirrel")));
/// assert!(name_signal(&t("example_quest_v2"), &t("example quest")));
/// assert!(name_signal(&t("novathesquirrel"), &t("nova the squirrel")));
/// assert!(!name_signal(&t("1"), &t("1")));
/// ```
#[must_use]
pub fn name_signal(a: &[String], b: &[String]) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let within = |x: &[String], y: &[String]| x.iter().all(|t| y.contains(t));
    within(a, b) || within(b, a) || a.concat() == b.concat()
}

/// The fuzzy and size-only candidates for `unmatched`, the files the name
/// tiers left unmatched, among files whose extension is in `extensions`.
///
/// A file names every rom of exactly its size with a [`name_signal`] between
/// its stem and the rom's `match_base`, as [`Confidence::Fuzzy`], unless
/// more than [`MAX_FUZZY`] roms qualify. When that finds nothing, and the
/// file is the only one of `unmatched` with such an extension, it names every
/// rom of its size as [`Confidence::Size`] if there are 1 to [`MAX_SIZE_ONLY`].
///
/// ```
/// use mistarr_sources::binding::{Confidence, RomRef};
/// use mistarr_sources::fuzzy::{candidates, SizeIndex};
/// use mistarr_sources::torrent::TorrentFile;
///
/// struct Nova;
/// impl SizeIndex for Nova {
///     fn roms_of_size(&self, size: u64) -> Vec<(RomRef, String)> {
///         if size != 16 { return Vec::new(); }
///         vec![(RomRef(1), "nova the squirrel".into()), (RomRef(2), "other tale".into())]
///     }
/// }
/// let file = TorrentFile { index: 3, path: "nova.nes".into(), size: 16 };
/// assert_eq!(candidates(&[&file], &["nes"], &Nova), vec![(3, RomRef(1), Confidence::Fuzzy)]);
/// ```
#[must_use]
pub fn candidates(
    unmatched: &[&TorrentFile],
    extensions: &[&str],
    index: &dyn SizeIndex,
) -> Vec<(u32, RomRef, Confidence)> {
    let eligible: Vec<&TorrentFile> = unmatched
        .iter()
        .copied()
        .filter(|f| f.size > 0 && has_extension(&f.path, extensions))
        .collect();
    let mut by_size: BTreeMap<u64, Vec<&TorrentFile>> = BTreeMap::new();
    for file in &eligible {
        by_size.entry(file.size).or_default().push(file);
    }
    let mut out = Vec::new();
    for (size, files) in by_size {
        let roms = index.roms_of_size(size);
        if roms.is_empty() {
            continue;
        }
        let group = Group::new(&roms);
        for file in files {
            let stem = normalise_name(leaf(&file.path));
            let found = group.matches(&significant_tokens(&stem));
            if !found.is_empty() {
                out.extend(
                    found
                        .into_iter()
                        .map(|r| (file.index, r, Confidence::Fuzzy)),
                );
            } else if eligible.len() == 1 && roms.len() <= MAX_SIZE_ONLY {
                out.extend(roms.iter().map(|(r, _)| (file.index, *r, Confidence::Size)));
            }
        }
    }
    out
}

fn leaf(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn has_extension(path: &str, extensions: &[&str]) -> bool {
    leaf(path)
        .rsplit_once('.')
        .is_some_and(|(_, ext)| extensions.iter().any(|e| e.eq_ignore_ascii_case(ext)))
}

/// The roms of one size, grouped by their significant words so roms that
/// read alike are compared once, with lookups that avoid a scan per file.
struct Group {
    /// Each distinct word list, with the roms that have it.
    sets: Vec<(Vec<String>, Vec<RomRef>)>,
    /// Word to the sets containing it.
    containing: HashMap<String, Vec<usize>>,
    /// Word to the sets whose rarest word it is.
    rarest: HashMap<String, Vec<usize>>,
    /// Words run together to the sets spelling them.
    joined: HashMap<String, Vec<usize>>,
}

impl Group {
    fn new(roms: &[(RomRef, String)]) -> Self {
        let mut position: HashMap<Vec<String>, usize> = HashMap::new();
        let mut sets: Vec<(Vec<String>, Vec<RomRef>)> = Vec::new();
        for (rom, base) in roms {
            let words = significant_tokens(base);
            if words.is_empty() {
                continue;
            }
            let at = *position.entry(words.clone()).or_insert_with(|| {
                sets.push((words, Vec::new()));
                sets.len() - 1
            });
            sets[at].1.push(*rom);
        }
        let mut containing: HashMap<String, Vec<usize>> = HashMap::new();
        let mut joined: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, (words, _)) in sets.iter().enumerate() {
            for w in words {
                containing.entry(w.clone()).or_default().push(i);
            }
            joined.entry(words.concat()).or_default().push(i);
        }
        let mut rarest: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, (words, _)) in sets.iter().enumerate() {
            let pick = words
                .iter()
                .min_by_key(|w| containing.get(*w).map_or(0, Vec::len));
            if let Some(w) = pick {
                rarest.entry(w.clone()).or_default().push(i);
            }
        }
        Self {
            sets,
            containing,
            rarest,
            joined,
        }
    }

    /// The roms whose words give a name signal with `words`, or none when
    /// more than [`MAX_FUZZY`] do.
    fn matches(&self, words: &[String]) -> Vec<RomRef> {
        if words.is_empty() {
            return Vec::new();
        }
        let narrowest = words
            .iter()
            .map(|w| self.containing.get(w).map_or(&[][..], Vec::as_slice))
            .min_by_key(|s| s.len())
            .unwrap_or(&[]);
        let mut seen: HashSet<usize> = HashSet::new();
        let tried = narrowest
            .iter()
            .chain(
                words
                    .iter()
                    .flat_map(|w| self.rarest.get(w).into_iter().flatten()),
            )
            .chain(self.joined.get(&words.concat()).into_iter().flatten());
        let mut out = Vec::new();
        for &i in tried {
            if !seen.insert(i) {
                continue;
            }
            let (set, roms) = &self.sets[i];
            if name_signal(words, set) {
                out.extend_from_slice(roms);
                if out.len() > MAX_FUZZY {
                    return Vec::new();
                }
            }
        }
        out.sort_unstable();
        out
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    struct Roms(Vec<(RomRef, String, u64)>);

    impl SizeIndex for Roms {
        fn roms_of_size(&self, size: u64) -> Vec<(RomRef, String)> {
            self.0
                .iter()
                .filter(|(_, _, s)| *s == size)
                .map(|(r, b, _)| (*r, b.clone()))
                .collect()
        }
    }

    fn file(index: u32, path: &str, size: u64) -> TorrentFile {
        TorrentFile {
            index,
            path: path.to_owned(),
            size,
        }
    }

    fn nova() -> Roms {
        Roms(vec![
            (RomRef(1), "nova the squirrel".into(), 262_160),
            (RomRef(2), "nova the squirrel".into(), 262_160),
            (RomRef(3), "example quest".into(), 262_160),
        ])
    }

    #[test]
    fn a_short_stem_names_both_versions_of_its_size() {
        let files = [
            file(0, "nova.nes", 262_160),
            file(1, "nova.png", 262_160),
            file(2, "nova.sqlite", 9),
        ];
        let refs: Vec<&TorrentFile> = files.iter().collect();
        let found = candidates(&refs, &["nes"], &nova());
        assert_eq!(
            found,
            [
                (0, RomRef(1), Confidence::Fuzzy),
                (0, RomRef(2), Confidence::Fuzzy)
            ]
        );
    }

    #[test]
    fn a_sole_file_without_a_name_signal_falls_back_to_size() {
        let files = [file(0, "Game/rom.NES", 262_160), file(1, "a.txt", 4)];
        let refs: Vec<&TorrentFile> = files.iter().collect();
        let found = candidates(&refs, &["nes"], &nova());
        assert_eq!(found.len(), 3);
        assert!(found
            .iter()
            .all(|(i, _, c)| *i == 0 && *c == Confidence::Size));
        let crowded = Roms(
            (0..=i64::try_from(MAX_SIZE_ONLY).unwrap_or(0))
                .map(|i| (RomRef(i), format!("title {i}"), 8))
                .collect(),
        );
        assert!(candidates(&[&file(0, "x.nes", 8)], &["nes"], &crowded).is_empty());
    }

    #[test]
    fn size_alone_needs_the_file_to_be_the_only_candidate_file() {
        let files = [file(0, "rom.nes", 262_160), file(1, "other.nes", 262_160)];
        let refs: Vec<&TorrentFile> = files.iter().collect();
        assert!(candidates(&refs, &["nes"], &nova()).is_empty());
    }

    #[test]
    fn a_set_of_same_sized_files_gets_no_size_only_candidates() {
        let roms = Roms(
            (0..2_000)
                .map(|i| (RomRef(i), format!("title {}", i % 50), 40_976))
                .collect(),
        );
        let files: Vec<TorrentFile> = (0..5_000)
            .map(|i| file(i, &format!("Set/unnamed {i}.nes"), 40_976))
            .collect();
        let refs: Vec<&TorrentFile> = files.iter().collect();
        assert!(candidates(&refs, &["nes"], &roms).is_empty());
    }

    #[test]
    fn ambiguous_and_filler_names_are_skipped() {
        let many = Roms(
            (0..=i64::try_from(MAX_FUZZY).unwrap_or(0))
                .map(|i| (RomRef(i), format!("example quest {i}"), 8))
                .collect(),
        );
        assert!(candidates(&[&file(0, "example.nes", 8)], &["nes"], &many).is_empty());
        let few = Roms(vec![
            (RomRef(1), "example".into(), 8),
            (RomRef(2), "tale".into(), 8),
        ]);
        let v2 = file(0, "v2 1.nes", 8);
        let other = file(1, "other.nes", 8);
        assert!(candidates(&[&v2, &other], &["nes"], &few).is_empty());
        assert!(candidates(&[&file(0, "example.zip", 8)], &["nes"], &few).is_empty());
        assert!(candidates(&[&file(0, "example.nes", 0)], &["nes"], &few).is_empty());
    }

    #[test]
    fn fillers_and_tokens() {
        assert!(is_filler("0") && is_filler("version") && is_filler("v1"));
        assert!(!is_filler("") && !is_filler("va") && !is_filler("revenge"));
        assert_eq!(significant_tokens("A-a b_B 3"), ["a", "b"]);
        assert!(significant_tokens("1.0.6 v2").is_empty());
    }

    fn word() -> impl Strategy<Value = String> {
        "[a-z]{3}"
    }

    fn filler() -> impl Strategy<Value = String> {
        prop_oneof!["[0-9]{1,4}", "v[0-9]{1,3}", Just("rev".to_owned())]
    }

    proptest! {
        #[test]
        fn signal_is_symmetric(a in prop::collection::vec("[a-z0-9]{1,4}", 0..5),
                               b in prop::collection::vec("[a-z0-9]{1,4}", 0..5)) {
            let (a, b) = (significant_tokens(&a.join(" ")), significant_tokens(&b.join(" ")));
            prop_assert_eq!(name_signal(&a, &b), name_signal(&b, &a));
        }

        #[test]
        fn a_prefix_of_the_title_signals(words in prop::collection::vec(word(), 1..6),
                                         take in 1usize..6, sep in "[ _.-]") {
            let take = take.min(words.len());
            let stem = words[..take].join(&sep);
            let rom = words.join(" ");
            prop_assert!(name_signal(&significant_tokens(&stem), &significant_tokens(&rom)));
        }

        #[test]
        fn fillers_never_change_the_answer(a in prop::collection::vec(word(), 0..4),
                                           b in prop::collection::vec(word(), 0..4),
                                           extra in prop::collection::vec(filler(), 0..4)) {
            let plain = name_signal(&significant_tokens(&a.join(" ")), &significant_tokens(&b.join(" ")));
            let padded = format!("{} {}", a.join(" "), extra.join("_"));
            prop_assert_eq!(
                name_signal(&significant_tokens(&padded), &significant_tokens(&b.join(" "))),
                plain
            );
        }

        #[test]
        fn fillers_alone_never_signal(a in prop::collection::vec(filler(), 0..5),
                                      b in prop::collection::vec("[a-z0-9 ]{0,12}", 0..3)) {
            let t = significant_tokens(&a.join("."));
            prop_assert!(!name_signal(&t, &significant_tokens(&b.join(" "))));
        }

        #[test]
        fn disjoint_words_never_signal(a in prop::collection::btree_set(word(), 1..4),
                                       b in prop::collection::btree_set(word(), 1..4)) {
            prop_assume!(a.is_disjoint(&b));
            let a: Vec<String> = a.into_iter().collect();
            let b: Vec<String> = b.into_iter().collect();
            prop_assert!(!name_signal(&a, &b));
        }

        #[test]
        fn candidates_share_the_size_and_a_signal(
            roms in prop::collection::vec((prop::collection::vec(word(), 1..3), 1u64..4), 0..30),
            files in prop::collection::vec((prop::collection::vec(word(), 1..3), 1u64..4), 0..10),
        ) {
            let index = Roms(roms.iter().enumerate()
                .map(|(i, (w, s))| (RomRef(i64::try_from(i).unwrap_or(0)), w.join(" "), *s))
                .collect());
            let files: Vec<TorrentFile> = files.iter().enumerate()
                .map(|(i, (w, s))| file(u32::try_from(i).unwrap_or(0), &format!("{}.nes", w.join("_")), *s))
                .collect();
            let refs: Vec<&TorrentFile> = files.iter().collect();
            let found = candidates(&refs, &["nes"], &index);
            for (i, rom, confidence) in &found {
                let f = &files[*i as usize];
                let (words, size) = &roms[usize::try_from(rom.0).unwrap_or(0)];
                prop_assert_eq!(*size, f.size);
                if *confidence == Confidence::Fuzzy {
                    let stem = significant_tokens(&normalise_name(&f.path));
                    prop_assert!(name_signal(&stem, &significant_tokens(&words.join(" "))));
                } else {
                    prop_assert_eq!(*confidence, Confidence::Size);
                    prop_assert_eq!(files.len(), 1);
                }
            }
            let per_file = found.iter().filter(|(_, _, c)| *c == Confidence::Fuzzy)
                .fold(HashMap::<u32, usize>::new(), |mut m, (i, _, _)| { *m.entry(*i).or_default() += 1; m });
            prop_assert!(per_file.values().all(|n| *n <= MAX_FUZZY));
        }
    }
}
