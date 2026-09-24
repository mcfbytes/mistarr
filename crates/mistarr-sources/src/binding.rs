//! Binding a torrent's file list to a platform by name and size overlap with
//! loaded DATs. See `docs/ARCHITECTURE.md` "Source import" steps 2-4 and
//! `docs/VERIFICATION.md` "Pre-download matching".

use std::collections::{HashMap, HashSet};

use unicode_normalization::UnicodeNormalization;

use crate::torrent::TorrentFile;
use crate::PlatformId;

/// A DAT entry's row id, mirroring `roms.id` in `docs/DATA-MODEL.md`. A
/// placeholder until `mistarr-core` (WP-01) exposes its own rom id type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RomRef(pub i64);

/// How a torrent file was matched to a rom, from strongest to weakest; the
/// tiers are in `docs/VERIFICATION.md` "Pre-download matching".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Confidence {
    /// Matched by exact or normalised name.
    Name,
    /// Matched by base name (before the first parenthesised tag) plus size.
    Base,
    /// Same size, and the names share their significant words; see [`crate::fuzzy`].
    Fuzzy,
    /// Same size alone, under the narrow rule in [`crate::fuzzy::candidates`].
    Size,
    /// No rom matched this file.
    Unmatched,
}

/// A minimal read-only view over loaded DATs, sufficient to bind and match
/// without depending on `mistarr-core`'s DAT model (WP-01).
pub trait DatIndex {
    /// Roms whose normalised name equals `name`, with the platform each
    /// belongs to.
    fn by_normalised_name(&self, name: &str) -> Vec<(PlatformId, RomRef)>;
    /// Roms whose base name equals `base_name` and whose size equals `size`.
    fn by_base_name_and_size(&self, base_name: &str, size: u64) -> Vec<(PlatformId, RomRef)>;
}

/// The result of [`bind`].
#[derive(Debug, Clone, PartialEq)]
pub enum Binding {
    /// Bound to a platform, with the hit rate that produced it.
    Bound(PlatformId, f32),
    /// No platform reached the threshold; every candidate and its rate.
    Unbound(Vec<(PlatformId, f32)>),
}

/// Strips a file extension, applies Unicode NFKC, lowercases, replaces the
/// libretro-style disallowed characters with `_`, and collapses whitespace.
///
/// ```
/// use mistarr_sources::binding::normalise_name;
/// assert_eq!(normalise_name("Example: Quest (USA).nes"), "example_ quest (usa)");
/// ```
#[must_use]
pub fn normalise_name(name: &str) -> String {
    let without_ext = strip_extension(name);
    let nfkc: String = without_ext.nfkc().collect();
    let lowered = nfkc.to_lowercase();
    let substituted: String = lowered
        .chars()
        .map(|c| if is_disallowed(c) { '_' } else { c })
        .collect();
    collapse_whitespace(&substituted)
}

fn strip_extension(name: &str) -> &str {
    match name.rfind('.') {
        Some(0) | None => name,
        Some(i) => &name[..i],
    }
}

fn is_disallowed(c: char) -> bool {
    matches!(
        c,
        '&' | '*' | '/' | ':' | '`' | '<' | '>' | '?' | '\\' | '|' | '"'
    )
}

fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_was_space = false;
    for c in s.trim().chars() {
        if c.is_whitespace() {
            if !last_was_space {
                out.push(' ');
            }
            last_was_space = true;
        } else {
            out.push(c);
            last_was_space = false;
        }
    }
    out
}

/// The part of a normalised name before its first parenthesised tag, per
/// `docs/VERIFICATION.md` "Name parsing".
///
/// ```
/// use mistarr_sources::binding::base_name;
/// assert_eq!(base_name("example quest (usa) (rev 1)"), "example quest");
/// ```
#[must_use]
pub fn base_name(normalised: &str) -> &str {
    match normalised.find('(') {
        Some(i) => normalised[..i].trim_end(),
        None => normalised,
    }
}

// The torrent's directory segments are not part of the rom's own name; only
// the final path segment (the file name) is normalised and matched.
fn leaf_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn candidates_for(
    file: &TorrentFile,
    index: &dyn DatIndex,
) -> Vec<(PlatformId, RomRef, Confidence)> {
    let normalised = normalise_name(leaf_name(&file.path));
    let name_matches = index.by_normalised_name(&normalised);
    let matched_platforms: HashSet<PlatformId> =
        name_matches.iter().map(|(p, _)| p.clone()).collect();

    let mut candidates: Vec<(PlatformId, RomRef, Confidence)> = name_matches
        .into_iter()
        .map(|(p, r)| (p, r, Confidence::Name))
        .collect();

    // A platform already matched by name keeps that stronger match; only
    // platforms with no name match fall back to base name plus size.
    let size_matches = index.by_base_name_and_size(base_name(&normalised), file.size);
    candidates.extend(
        size_matches
            .into_iter()
            .filter(|(p, _)| !matched_platforms.contains(p))
            .map(|(p, r)| (p, r, Confidence::Base)),
    );
    candidates
}

/// The hit rate (fraction of `files` with at least one match) for every
/// platform any file matched, descending by rate then platform id.
///
/// ```
/// use mistarr_sources::binding::{score_platforms, DatIndex, RomRef};
/// use mistarr_sources::torrent::TorrentFile;
/// use mistarr_sources::PlatformId;
///
/// struct Empty;
/// impl DatIndex for Empty {
///     fn by_normalised_name(&self, _: &str) -> Vec<(PlatformId, RomRef)> { Vec::new() }
///     fn by_base_name_and_size(&self, _: &str, _: u64) -> Vec<(PlatformId, RomRef)> { Vec::new() }
/// }
/// let files = vec![TorrentFile { index: 0, path: "a.nes".into(), size: 1 }];
/// assert!(score_platforms(&files, &Empty).is_empty());
/// ```
#[must_use]
#[allow(
    clippy::cast_precision_loss,
    reason = "file and hit counts fit f32 exactly for any realistic torrent"
)]
pub fn score_platforms(files: &[TorrentFile], index: &dyn DatIndex) -> Vec<(PlatformId, f32)> {
    if files.is_empty() {
        return Vec::new();
    }
    let mut hits: HashMap<PlatformId, usize> = HashMap::new();
    for file in files {
        let mut seen: HashSet<PlatformId> = HashSet::new();
        for (platform, _, _) in candidates_for(file, index) {
            if seen.insert(platform.clone()) {
                *hits.entry(platform).or_insert(0) += 1;
            }
        }
    }
    let total = files.len() as f32;
    let mut scored: Vec<(PlatformId, f32)> = hits
        .into_iter()
        .map(|(p, n)| (p, n as f32 / total))
        .collect();
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0 .0.cmp(&b.0 .0))
    });
    scored
}

/// Binds a torrent's files to the best-scoring platform, or leaves it
/// [`Binding::Unbound`] when nothing reaches `threshold`.
///
/// A rate equal to `threshold` counts as bound, matching "at or above" from
/// `docs/ARCHITECTURE.md`.
///
/// ```
/// use mistarr_sources::binding::{bind, Binding, DatIndex, RomRef};
/// use mistarr_sources::torrent::TorrentFile;
/// use mistarr_sources::PlatformId;
///
/// struct Empty;
/// impl DatIndex for Empty {
///     fn by_normalised_name(&self, _: &str) -> Vec<(PlatformId, RomRef)> { Vec::new() }
///     fn by_base_name_and_size(&self, _: &str, _: u64) -> Vec<(PlatformId, RomRef)> { Vec::new() }
/// }
/// let files = vec![TorrentFile { index: 0, path: "a.nes".into(), size: 1 }];
/// assert_eq!(bind(&files, &Empty, 0.6), Binding::Unbound(Vec::new()));
/// ```
#[must_use]
pub fn bind(files: &[TorrentFile], index: &dyn DatIndex, threshold: f32) -> Binding {
    let scored = score_platforms(files, index);
    match scored.first() {
        Some((platform, rate)) if *rate >= threshold => Binding::Bound(platform.clone(), *rate),
        _ => Binding::Unbound(scored),
    }
}

/// Matches every file to a rom under the given platform, for populating
/// `torrent_files`. Files under a different platform's roms are ignored.
///
/// ```
/// use mistarr_sources::binding::{match_files, Confidence, DatIndex, RomRef};
/// use mistarr_sources::torrent::TorrentFile;
/// use mistarr_sources::PlatformId;
///
/// struct Empty;
/// impl DatIndex for Empty {
///     fn by_normalised_name(&self, _: &str) -> Vec<(PlatformId, RomRef)> { Vec::new() }
///     fn by_base_name_and_size(&self, _: &str, _: u64) -> Vec<(PlatformId, RomRef)> { Vec::new() }
/// }
/// let files = vec![TorrentFile { index: 0, path: "a.nes".into(), size: 1 }];
/// let out = match_files(&files, &PlatformId("nes".into()), &Empty);
/// assert_eq!(out, vec![(0, None, Confidence::Unmatched)]);
/// ```
#[must_use]
pub fn match_files(
    files: &[TorrentFile],
    platform: &PlatformId,
    index: &dyn DatIndex,
) -> Vec<(u32, Option<RomRef>, Confidence)> {
    files
        .iter()
        .map(|file| {
            let best = candidates_for(file, index)
                .into_iter()
                .find(|(p, _, _)| p == platform);
            match best {
                Some((_, rom, confidence)) => (file.index, Some(rom), confidence),
                None => (file.index, None, Confidence::Unmatched),
            }
        })
        .collect()
}

/// The name tiers of mapping a torrent under one platform: the best rom per
/// file for `torrent_files`, and the other roms the same tier found.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Mapping {
    /// Each file's best rom and confidence, as [`match_files`] gives them.
    pub matches: Vec<(u32, Option<RomRef>, Confidence)>,
    /// Further roms a file matched as well as its best one, for `torrent_candidates`.
    pub extra: Vec<(u32, RomRef, Confidence)>,
}

impl Mapping {
    /// The files no name tier matched.
    ///
    /// ```
    /// use mistarr_sources::binding::{Confidence, Mapping};
    /// use mistarr_sources::torrent::TorrentFile;
    /// let files = vec![TorrentFile { index: 4, path: "a.nes".into(), size: 1 }];
    /// let m = Mapping { matches: vec![(4, None, Confidence::Unmatched)], extra: Vec::new() };
    /// assert_eq!(m.unmatched(&files).len(), 1);
    /// ```
    #[must_use]
    pub fn unmatched<'f>(&self, files: &'f [TorrentFile]) -> Vec<&'f TorrentFile> {
        let hit: HashSet<u32> = self
            .matches
            .iter()
            .filter(|(_, rom, _)| rom.is_some())
            .map(|(i, _, _)| *i)
            .collect();
        files.iter().filter(|f| !hit.contains(&f.index)).collect()
    }
}

/// Like [`match_files`], and also keeps every further rom of the platform
/// that the winning tier found for a file, such as regional variants sharing
/// a base name and size.
///
/// ```
/// use mistarr_sources::binding::{map_files, Confidence, DatIndex, RomRef};
/// use mistarr_sources::torrent::TorrentFile;
/// use mistarr_sources::PlatformId;
///
/// struct Two;
/// impl DatIndex for Two {
///     fn by_normalised_name(&self, _: &str) -> Vec<(PlatformId, RomRef)> { Vec::new() }
///     fn by_base_name_and_size(&self, _: &str, _: u64) -> Vec<(PlatformId, RomRef)> {
///         vec![(PlatformId("nes".into()), RomRef(1)), (PlatformId("nes".into()), RomRef(2))]
///     }
/// }
/// let files = vec![TorrentFile { index: 0, path: "a.nes".into(), size: 1 }];
/// let m = map_files(&files, &PlatformId("nes".into()), &Two);
/// assert_eq!(m.matches, vec![(0, Some(RomRef(1)), Confidence::Base)]);
/// assert_eq!(m.extra, vec![(0, RomRef(2), Confidence::Base)]);
/// ```
#[must_use]
pub fn map_files(files: &[TorrentFile], platform: &PlatformId, index: &dyn DatIndex) -> Mapping {
    let mut out = Mapping {
        matches: Vec::with_capacity(files.len()),
        extra: Vec::new(),
    };
    for file in files {
        let mut found = candidates_for(file, index)
            .into_iter()
            .filter(|(p, _, _)| p == platform);
        let Some((_, best, confidence)) = found.next() else {
            out.matches.push((file.index, None, Confidence::Unmatched));
            continue;
        };
        out.matches.push((file.index, Some(best), confidence));
        for (_, rom, c) in found {
            if c == confidence && rom != best {
                out.extra.push((file.index, rom, c));
            }
        }
    }
    out
}

/// Names that may say which platform a torrent is for, without any DAT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameHints {
    /// The dropped file's stem and the torrent's info name.
    pub names: [String; 2],
    /// Each directory holding at least half of the files, with how many it
    /// holds, in order of first appearance.
    pub dirs: Vec<(String, usize)>,
}

/// The [`NameHints`] of a torrent dropped as `origin_file`.
///
/// ```
/// use mistarr_sources::binding::name_hints;
/// use mistarr_sources::torrent::TorrentFile;
/// let files = vec![TorrentFile { index: 0, path: "Sets/Example System/a.zip".into(), size: 1 }];
/// let hints = name_hints("Example Pack.torrent", "Example Pack", &files);
/// assert_eq!(hints.names, ["Example Pack", "Example Pack"]);
/// assert_eq!(hints.dirs, [("Sets".to_owned(), 1), ("Example System".to_owned(), 1)]);
/// ```
#[must_use]
pub fn name_hints(origin_file: &str, info_name: &str, files: &[TorrentFile]) -> NameHints {
    let stem = origin_file
        .strip_suffix(".torrent")
        .or_else(|| origin_file.strip_suffix(".magnet"))
        .unwrap_or(origin_file);
    let names = [stem.to_owned(), info_name.to_owned()];
    let mut counts: HashMap<&str, usize> = HashMap::new();
    let mut order: Vec<&str> = Vec::new();
    for file in files {
        let mut parts: Vec<&str> = file.path.split('/').collect();
        parts.pop();
        let mut dirs: Vec<&str> = Vec::with_capacity(parts.len());
        for part in parts {
            if !part.is_empty() && !dirs.contains(&part) {
                dirs.push(part);
            }
        }
        for dir in dirs {
            let n = counts.entry(dir).or_insert(0);
            if *n == 0 {
                order.push(dir);
            }
            *n += 1;
        }
    }
    let half = files.len().div_ceil(2);
    let dirs = order
        .into_iter()
        .filter_map(|d| {
            let n = counts.get(d).copied().unwrap_or(0);
            (n >= half).then(|| (d.to_owned(), n))
        })
        .collect();
    NameHints { names, dirs }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use proptest::prelude::*;

    use super::*;

    fn file(index: u32, path: &str) -> TorrentFile {
        TorrentFile {
            index,
            path: path.to_owned(),
            size: 1,
        }
    }

    #[test]
    fn hints_skip_directories_of_a_minority_of_files() {
        let files = [
            file(0, "Top/System A/a.zip"),
            file(1, "Top/System A/b.zip"),
            file(2, "Top/System B/c.zip"),
            file(3, "loose.txt"),
        ];
        let hints = name_hints("pack.magnet", "Pack", &files);
        assert_eq!(hints.names, ["pack", "Pack"]);
        let dirs: Vec<(&str, usize)> = hints.dirs.iter().map(|(d, n)| (d.as_str(), *n)).collect();
        assert_eq!(dirs, [("Top", 3), ("System A", 2)]);
        assert!(name_hints("x", "y", &[]).dirs.is_empty());
    }

    proptest! {
        #[test]
        fn hints_start_with_the_names_and_never_hold_a_leaf(
            origin in "[a-z ]{0,10}", info in "[a-z ]{0,10}",
            paths in prop::collection::vec("[a-c]{1,2}(/[a-c]{1,2}){0,3}\\.bin", 0..20)
        ) {
            let files: Vec<TorrentFile> = paths
                .iter()
                .enumerate()
                .map(|(i, p)| file(u32::try_from(i).unwrap_or(0), p))
                .collect();
            let hints = name_hints(&format!("{origin}.torrent"), &info, &files);
            prop_assert_eq!(&hints.names[0], &origin);
            prop_assert_eq!(&hints.names[1], &info);
            for (hint, n) in &hints.dirs {
                prop_assert!(!hint.contains(".bin"));
                let holding = files
                    .iter()
                    .filter(|f| f.path.split('/').rev().skip(1).any(|d| d == hint))
                    .count();
                prop_assert_eq!(holding, *n);
                prop_assert!(holding * 2 >= files.len());
            }
        }

        #[test]
        fn a_directory_holding_every_file_is_a_hint(
            dir in "[A-Za-z]{1,8}", leaves in prop::collection::vec("[a-z]{1,6}", 1..10)
        ) {
            let files: Vec<TorrentFile> = leaves
                .iter()
                .enumerate()
                .map(|(i, l)| file(u32::try_from(i).unwrap_or(0), &format!("{dir}/{l}.zip")))
                .collect();
            let hints = name_hints("a", "b", &files);
            prop_assert!(hints.dirs.contains(&(dir.clone(), leaves.len())));
        }
    }

    struct FakeDat {
        by_name: BTreeMap<String, Vec<(PlatformId, RomRef)>>,
        by_size: BTreeMap<(String, u64), Vec<(PlatformId, RomRef)>>,
    }

    impl DatIndex for FakeDat {
        fn by_normalised_name(&self, name: &str) -> Vec<(PlatformId, RomRef)> {
            self.by_name.get(name).cloned().unwrap_or_default()
        }

        fn by_base_name_and_size(&self, base_name: &str, size: u64) -> Vec<(PlatformId, RomRef)> {
            self.by_size
                .get(&(base_name.to_owned(), size))
                .cloned()
                .unwrap_or_default()
        }
    }

    fn platform(id: &str) -> PlatformId {
        PlatformId(id.to_owned())
    }

    #[test]
    fn normalise_matches_pre_download_matching_spec() {
        assert_eq!(
            normalise_name("Example Quest (USA).nes"),
            "example quest (usa)"
        );
        assert_eq!(
            normalise_name("A/B:C*D<E>F?G|H\"I`J&K"),
            "a_b_c_d_e_f_g_h_i_j_k"
        );
        assert_eq!(normalise_name("Spaced   Out.bin"), "spaced out");
    }

    #[test]
    fn base_name_strips_first_tag() {
        assert_eq!(base_name("example quest (usa) (rev 1)"), "example quest");
        assert_eq!(base_name("no tags here"), "no tags here");
    }

    #[test]
    fn scores_two_platforms_from_one_torrent() {
        let mut by_name = BTreeMap::new();
        by_name.insert(
            "example quest (usa)".to_owned(),
            vec![(platform("nes"), RomRef(1))],
        );
        by_name.insert(
            "other title (usa)".to_owned(),
            vec![(platform("snes"), RomRef(2))],
        );
        let dat = FakeDat {
            by_name,
            by_size: BTreeMap::new(),
        };

        let files = vec![
            TorrentFile {
                index: 0,
                path: "Example Quest (USA).nes".into(),
                size: 10,
            },
            TorrentFile {
                index: 1,
                path: "Other Title (USA).sfc".into(),
                size: 20,
            },
            TorrentFile {
                index: 2,
                path: "Unrelated (USA).bin".into(),
                size: 30,
            },
        ];
        let scores = score_platforms(&files, &dat);
        assert_eq!(scores.len(), 2);
        assert!(scores
            .iter()
            .all(|(_, rate)| (*rate - (1.0 / 3.0)).abs() < 1e-6));
    }

    #[test]
    fn threshold_boundary_binds_at_and_rejects_below() {
        let mut by_name = BTreeMap::new();
        by_name.insert("hit (usa)".to_owned(), vec![(platform("nes"), RomRef(1))]);
        let dat = FakeDat {
            by_name,
            by_size: BTreeMap::new(),
        };
        let files = vec![
            TorrentFile {
                index: 0,
                path: "Hit (USA).nes".into(),
                size: 1,
            },
            TorrentFile {
                index: 1,
                path: "Miss (USA).nes".into(),
                size: 1,
            },
        ];
        // Exactly 0.5: bound at threshold 0.5, unbound at 0.51.
        assert_eq!(
            bind(&files, &dat, 0.5),
            Binding::Bound(platform("nes"), 0.5)
        );
        assert_eq!(
            bind(&files, &dat, 0.51),
            Binding::Unbound(vec![(platform("nes"), 0.5)])
        );
    }

    #[test]
    fn falls_back_to_base_name_and_size() {
        let mut by_size = BTreeMap::new();
        by_size.insert(
            ("fallback".to_owned(), 99),
            vec![(platform("genesis"), RomRef(7))],
        );
        let dat = FakeDat {
            by_name: BTreeMap::new(),
            by_size,
        };
        let files = vec![TorrentFile {
            index: 0,
            path: "Fallback (Alt).bin".into(),
            size: 99,
        }];
        let matches = match_files(&files, &platform("genesis"), &dat);
        assert_eq!(matches, vec![(0, Some(RomRef(7)), Confidence::Base)]);
    }

    #[test]
    fn matches_files_in_subdirectories_by_leaf_name_only() {
        let mut by_name = BTreeMap::new();
        by_name.insert(
            "example quest (usa)".to_owned(),
            vec![(platform("nes"), RomRef(1))],
        );
        let dat = FakeDat {
            by_name,
            by_size: BTreeMap::new(),
        };
        let files = vec![TorrentFile {
            index: 0,
            path: "Set/Sub.Dir/Example Quest (USA).nes".into(),
            size: 10,
        }];
        let scores = score_platforms(&files, &dat);
        assert_eq!(scores, vec![(platform("nes"), 1.0)]);
    }

    #[test]
    fn size_fallback_applies_per_platform_alongside_a_name_match() {
        let mut by_name = BTreeMap::new();
        by_name.insert(
            "matched (usa)".to_owned(),
            vec![(platform("nes"), RomRef(1))],
        );
        let mut by_size = BTreeMap::new();
        by_size.insert(
            ("matched".to_owned(), 10),
            vec![(platform("snes"), RomRef(2))],
        );
        let dat = FakeDat { by_name, by_size };
        let files = vec![TorrentFile {
            index: 0,
            path: "Matched (USA).nes".into(),
            size: 10,
        }];

        let scores = score_platforms(&files, &dat);
        assert_eq!(scores.len(), 2);
        assert!(scores.contains(&(platform("nes"), 1.0)));
        assert!(scores.contains(&(platform("snes"), 1.0)));

        let matches = match_files(&files, &platform("snes"), &dat);
        assert_eq!(matches, vec![(0, Some(RomRef(2)), Confidence::Base)]);
    }

    #[test]
    fn map_files_keeps_every_rom_of_the_winning_tier() {
        let mut by_name = BTreeMap::new();
        by_name.insert(
            "twin (usa)".to_owned(),
            vec![(platform("nes"), RomRef(1)), (platform("nes"), RomRef(2))],
        );
        let mut by_size = BTreeMap::new();
        by_size.insert(
            ("twin".to_owned(), 10),
            vec![(platform("nes"), RomRef(3)), (platform("snes"), RomRef(4))],
        );
        let dat = FakeDat { by_name, by_size };
        let files = vec![
            TorrentFile {
                index: 0,
                path: "Twin (USA).nes".into(),
                size: 10,
            },
            TorrentFile {
                index: 1,
                path: "Twin (Japan).nes".into(),
                size: 10,
            },
            TorrentFile {
                index: 2,
                path: "none.nes".into(),
                size: 10,
            },
        ];
        let m = map_files(&files, &platform("nes"), &dat);
        assert_eq!(
            m.matches,
            vec![
                (0, Some(RomRef(1)), Confidence::Name),
                (1, Some(RomRef(3)), Confidence::Base),
                (2, None, Confidence::Unmatched),
            ]
        );
        assert_eq!(m.extra, vec![(0, RomRef(2), Confidence::Name)]);
        let left: Vec<u32> = m.unmatched(&files).iter().map(|f| f.index).collect();
        assert_eq!(left, [2]);
        assert!(Confidence::Name < Confidence::Base && Confidence::Fuzzy < Confidence::Size);
    }

    #[test]
    fn match_files_reports_unmatched() {
        let dat = FakeDat {
            by_name: BTreeMap::new(),
            by_size: BTreeMap::new(),
        };
        let files = vec![TorrentFile {
            index: 3,
            path: "Nothing.bin".into(),
            size: 1,
        }];
        assert_eq!(
            match_files(&files, &platform("nes"), &dat),
            vec![(3, None, Confidence::Unmatched)]
        );
    }
}
