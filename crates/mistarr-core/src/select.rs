//! 1G1R selection and clone-group inference. See
//! `docs/VERIFICATION.md` sections "Clone grouping" and "1G1R selection"
//! for the contract implemented here.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A flag that, by default, removes a variant from the 1G1R pick.
///
/// `unl` and `pirate` are deliberately not members of this enum: they are
/// always shown, only ranked last (see [`rank`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HiddenFlag {
    /// A BIOS entry.
    Bios,
    /// A beta build.
    Beta,
    /// A prototype build.
    Proto,
    /// A demo build.
    Demo,
    /// A sample build.
    Sample,
    /// A utility or test program, not a game.
    Program,
}

impl HiddenFlag {
    /// The lowercase flag name as it appears in [`Variant::flags`].
    ///
    /// ```
    /// use mistarr_core::select::HiddenFlag;
    /// assert_eq!(HiddenFlag::Bios.as_flag_name(), "bios");
    /// ```
    #[must_use]
    pub fn as_flag_name(self) -> &'static str {
        match self {
            HiddenFlag::Bios => "bios",
            HiddenFlag::Beta => "beta",
            HiddenFlag::Proto => "proto",
            HiddenFlag::Demo => "demo",
            HiddenFlag::Sample => "sample",
            HiddenFlag::Program => "program",
        }
    }
}

/// User preferences that drive 1G1R selection, matching the `[prefs]` block
/// in `docs/ARCHITECTURE.md`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prefs {
    /// Preferred regions, most preferred first.
    pub regions: Vec<String>,
    /// Preferred languages; any one match is enough.
    pub languages: Vec<String>,
    /// Prefer the highest revision when `true`, else the lowest.
    pub prefer_latest_revision: bool,
    /// Flags that hide a variant from the pick by default.
    pub hide: Vec<HiddenFlag>,
}

impl Default for Prefs {
    /// Matches the documented default `[prefs]` block.
    ///
    /// ```
    /// use mistarr_core::select::Prefs;
    /// let prefs = Prefs::default();
    /// assert_eq!(prefs.regions[0], "USA");
    /// assert!(prefs.prefer_latest_revision);
    /// ```
    fn default() -> Self {
        Prefs {
            regions: ["USA", "World", "Europe", "Japan"]
                .into_iter()
                .map(String::from)
                .collect(),
            languages: vec!["En".to_string()],
            prefer_latest_revision: true,
            hide: vec![
                HiddenFlag::Bios,
                HiddenFlag::Beta,
                HiddenFlag::Proto,
                HiddenFlag::Demo,
                HiddenFlag::Sample,
                HiddenFlag::Program,
            ],
        }
    }
}

/// One member of a clone group, as input to selection.
///
/// This is a minimal, self-contained input: it does not depend on the DAT
/// or name parser types from other work packages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Variant {
    /// Stable identifier of the underlying title.
    pub id: u64,
    /// Full display name, used only for the deterministic tie-break.
    pub name: String,
    /// Region tags, e.g. `["USA"]` or `["USA", "Europe"]`.
    pub regions: Vec<String>,
    /// Language tags, e.g. `["En"]`.
    pub languages: Vec<String>,
    /// Sortable revision rank; `None` means no revision tag.
    pub revision_rank: Option<u32>,
    /// Lowercase flags: `bios`, `beta`, `proto`, `demo`, `sample`,
    /// `program`, `unl`, `pirate`, `baddump`, or `other:...`.
    pub flags: Vec<String>,
    /// `false` if this dump's rom status is `baddump`.
    pub good_dump: bool,
}

/// The comparable key `select_1g1r` minimises. Lower sorts better.
///
/// Field order is the priority order from "1G1R selection" in
/// `docs/VERIFICATION.md`: unlicensed/pirate status, then region, then
/// language, then revision, then dump status, then name, then id.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Rank {
    unlicensed_or_pirate: bool,
    region_rank: usize,
    lacks_preferred_language: bool,
    revision_key: i64,
    is_bad_dump: bool,
    name_len: usize,
    name: String,
    id: u64,
}

/// `true` if `variant` carries a flag in `prefs.hide`.
///
/// ```
/// use mistarr_core::select::{is_hidden, Prefs, Variant};
/// let prefs = Prefs::default();
/// let bios = Variant {
///     id: 1,
///     name: "Example Quest (BIOS)".to_string(),
///     regions: vec!["USA".to_string()],
///     languages: vec!["En".to_string()],
///     revision_rank: None,
///     flags: vec!["bios".to_string()],
///     good_dump: true,
/// };
/// assert!(is_hidden(&bios, &prefs));
/// ```
#[must_use]
pub fn is_hidden(variant: &Variant, prefs: &Prefs) -> bool {
    prefs
        .hide
        .iter()
        .any(|hidden| variant.flags.iter().any(|f| f == hidden.as_flag_name()))
}

/// The comparable [`Rank`] for `variant` under `prefs`, for sorting a whole
/// group for display (hidden variants are ranked too, not filtered).
///
/// ```
/// use mistarr_core::select::{rank, Prefs, Variant};
/// let prefs = Prefs::default();
/// let usa = Variant {
///     id: 1,
///     name: "Example Quest (USA)".to_string(),
///     regions: vec!["USA".to_string()],
///     languages: vec!["En".to_string()],
///     revision_rank: None,
///     flags: vec![],
///     good_dump: true,
/// };
/// let japan = Variant { regions: vec!["Japan".to_string()], ..usa.clone() };
/// assert!(rank(&usa, &prefs) < rank(&japan, &prefs));
/// ```
#[must_use]
pub fn rank(variant: &Variant, prefs: &Prefs) -> Rank {
    let unlicensed_or_pirate = variant.flags.iter().any(|f| f == "unl" || f == "pirate");
    let region_rank = variant
        .regions
        .iter()
        .filter_map(|r| prefs.regions.iter().position(|p| p == r))
        .min()
        .unwrap_or(prefs.regions.len());
    let lacks_preferred_language = !variant
        .languages
        .iter()
        .any(|l| prefs.languages.iter().any(|p| p == l));
    let revision = i64::from(variant.revision_rank.unwrap_or(0));
    let revision_key = if prefs.prefer_latest_revision {
        -revision
    } else {
        revision
    };
    Rank {
        unlicensed_or_pirate,
        region_rank,
        lacks_preferred_language,
        revision_key,
        is_bad_dump: !variant.good_dump,
        name_len: variant.name.len(),
        name: variant.name.clone(),
        id: variant.id,
    }
}

/// Picks the best variant of `group` under `prefs`, or `None` if every
/// variant is hidden. Deterministic regardless of `group`'s order.
///
/// ```
/// use mistarr_core::select::{select_1g1r, Prefs, Variant};
/// let prefs = Prefs::default();
/// let usa = Variant {
///     id: 1,
///     name: "Example Quest (USA)".to_string(),
///     regions: vec!["USA".to_string()],
///     languages: vec!["En".to_string()],
///     revision_rank: None,
///     flags: vec![],
///     good_dump: true,
/// };
/// let japan = Variant { id: 2, regions: vec!["Japan".to_string()], ..usa.clone() };
/// let group = [japan, usa.clone()];
/// let picked = select_1g1r(&group, &prefs).unwrap();
/// assert_eq!(picked.id, usa.id);
/// ```
#[must_use]
pub fn select_1g1r<'a>(group: &'a [Variant], prefs: &Prefs) -> Option<&'a Variant> {
    group
        .iter()
        .filter(|v| !is_hidden(v, prefs))
        .min_by_key(|v| rank(v, prefs))
}

/// A clone group inferred from a caller-supplied key rather than a DAT
/// `cloneof` link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    /// The grouping key the caller supplied, e.g. `(platform_id, base_name)`.
    pub key: String,
    /// Member ids, elected parent first, then the rest by id.
    pub member_ids: Vec<u64>,
    /// Always `true`: this grouping was inferred, not read from `cloneof`.
    pub inferred: bool,
}

/// Groups `items` by their key and elects a parent per group with
/// [`select_1g1r`] under [`Prefs::default`], putting it first in
/// `member_ids`. Output order is by key, for a deterministic result.
///
/// ```
/// use mistarr_core::select::infer_groups;
/// let groups = infer_groups(vec![(2, "Example Quest".to_string()), (1, "Example Quest".to_string())].into_iter());
/// assert_eq!(groups[0].key, "Example Quest");
/// assert_eq!(groups[0].member_ids.len(), 2);
/// ```
#[must_use]
pub fn infer_groups(items: impl Iterator<Item = (u64, String)>) -> Vec<Group> {
    let mut by_key: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    for (id, key) in items {
        by_key.entry(key).or_default().push(id);
    }
    let prefs = Prefs::default();
    by_key
        .into_iter()
        .map(|(key, mut ids)| {
            ids.sort_unstable();
            // Names are unknown here, so all members share the group key as
            // a stand-in name and selection falls back to the id tie-break.
            let variants: Vec<Variant> = ids
                .iter()
                .map(|&id| Variant {
                    id,
                    name: key.clone(),
                    regions: Vec::new(),
                    languages: Vec::new(),
                    revision_rank: None,
                    flags: Vec::new(),
                    good_dump: true,
                })
                .collect();
            let parent_id = select_1g1r(&variants, &prefs).map(|v| v.id);
            let mut member_ids = ids;
            if let Some(parent_id) = parent_id {
                member_ids.retain(|&id| id != parent_id);
                member_ids.insert(0, parent_id);
            }
            Group {
                key,
                member_ids,
                inferred: true,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base(id: u64, name: &str) -> Variant {
        Variant {
            id,
            name: name.to_string(),
            regions: vec!["USA".to_string()],
            languages: vec!["En".to_string()],
            revision_rank: None,
            flags: Vec::new(),
            good_dump: true,
        }
    }

    #[test]
    fn hidden_flag_names_match_verification_md() {
        assert_eq!(HiddenFlag::Bios.as_flag_name(), "bios");
        assert_eq!(HiddenFlag::Beta.as_flag_name(), "beta");
        assert_eq!(HiddenFlag::Proto.as_flag_name(), "proto");
        assert_eq!(HiddenFlag::Demo.as_flag_name(), "demo");
        assert_eq!(HiddenFlag::Sample.as_flag_name(), "sample");
        assert_eq!(HiddenFlag::Program.as_flag_name(), "program");
    }

    #[test]
    fn default_prefs_match_architecture_md() {
        let prefs = Prefs::default();
        assert_eq!(prefs.regions, vec!["USA", "World", "Europe", "Japan"]);
        assert_eq!(prefs.languages, vec!["En"]);
        assert!(prefs.prefer_latest_revision);
        assert_eq!(prefs.hide.len(), 6);
    }

    #[test]
    fn is_hidden_true_for_default_hidden_flags() {
        let prefs = Prefs::default();
        for flag in ["bios", "beta", "proto", "demo", "sample", "program"] {
            let mut v = base(1, "Example Quest (USA)");
            v.flags = vec![flag.to_string()];
            assert!(is_hidden(&v, &prefs), "{flag} should be hidden");
        }
    }

    #[test]
    fn is_hidden_false_for_unl_and_pirate() {
        let prefs = Prefs::default();
        for flag in ["unl", "pirate"] {
            let mut v = base(1, "Example Quest (USA) (Unl)");
            v.flags = vec![flag.to_string()];
            assert!(!is_hidden(&v, &prefs), "{flag} should not be hidden");
        }
    }

    #[test]
    fn is_hidden_false_for_no_flags() {
        let prefs = Prefs::default();
        assert!(!is_hidden(&base(1, "Example Quest (USA)"), &prefs));
    }

    #[test]
    fn select_returns_none_when_group_empty() {
        let prefs = Prefs::default();
        let group: [Variant; 0] = [];
        assert!(select_1g1r(&group, &prefs).is_none());
    }

    #[test]
    fn select_returns_none_when_all_hidden() {
        let prefs = Prefs::default();
        let mut a = base(1, "Example Quest (Beta)");
        a.flags = vec!["beta".to_string()];
        let group = [a];
        assert!(select_1g1r(&group, &prefs).is_none());
    }

    #[test]
    fn select_drops_bios_by_default() {
        let prefs = Prefs::default();
        let mut bios = base(1, "Example Quest (BIOS)");
        bios.flags = vec!["bios".to_string()];
        let good = base(2, "Example Quest (USA)");
        let group = [bios, good.clone()];
        let picked = select_1g1r(&group, &prefs).unwrap();
        assert_eq!(picked.id, good.id);
    }

    #[test]
    fn select_prefers_region_order() {
        let prefs = Prefs::default();
        let usa = base(1, "Example Quest (USA)");
        let mut japan = base(2, "Example Quest (Japan)");
        japan.regions = vec!["Japan".to_string()];
        let group = [japan, usa.clone()];
        let picked = select_1g1r(&group, &prefs).unwrap();
        assert_eq!(picked.id, usa.id);
    }

    #[test]
    fn select_ranks_multi_region_by_best_region() {
        let prefs = Prefs::default();
        let mut multi = base(1, "Example Quest (Japan, USA)");
        multi.regions = vec!["Japan".to_string(), "USA".to_string()];
        let mut europe_only = base(2, "Example Quest (Europe)");
        europe_only.regions = vec!["Europe".to_string()];
        let group = [europe_only, multi.clone()];
        let picked = select_1g1r(&group, &prefs).unwrap();
        assert_eq!(
            picked.id, multi.id,
            "multi-region variant ranks by its best region"
        );
    }

    #[test]
    fn select_prefers_preferred_language_on_region_tie() {
        let prefs = Prefs::default();
        let mut fr = base(1, "Example Quest (USA) (Fr)");
        fr.languages = vec!["Fr".to_string()];
        let en = base(2, "Example Quest (USA) (En)");
        let group = [fr, en.clone()];
        let picked = select_1g1r(&group, &prefs).unwrap();
        assert_eq!(picked.id, en.id);
    }

    #[test]
    fn select_prefers_highest_revision_by_default() {
        let prefs = Prefs::default();
        let mut rev1 = base(1, "Example Quest (USA) (Rev 1)");
        rev1.revision_rank = Some(1);
        let mut rev2 = base(2, "Example Quest (USA) (Rev 2)");
        rev2.revision_rank = Some(2);
        let group = [rev1, rev2.clone()];
        let picked = select_1g1r(&group, &prefs).unwrap();
        assert_eq!(picked.id, rev2.id);
    }

    #[test]
    fn select_prefers_lowest_revision_when_configured() {
        let prefs = Prefs {
            prefer_latest_revision: false,
            ..Prefs::default()
        };
        let mut rev1 = base(1, "Example Quest (USA) (Rev 1)");
        rev1.revision_rank = Some(1);
        let mut rev2 = base(2, "Example Quest (USA) (Rev 2)");
        rev2.revision_rank = Some(2);
        let group = [rev1.clone(), rev2];
        let picked = select_1g1r(&group, &prefs).unwrap();
        assert_eq!(picked.id, rev1.id);
    }

    #[test]
    fn select_prefers_good_dump_over_baddump() {
        let prefs = Prefs::default();
        let mut bad = base(1, "Example Quest (USA)");
        bad.good_dump = false;
        let good = base(2, "Example Quest (USA) [b]");
        let group = [bad, good.clone()];
        let picked = select_1g1r(&group, &prefs).unwrap();
        assert_eq!(picked.id, good.id);
    }

    #[test]
    fn select_prefers_shortest_name_on_full_tie() {
        let prefs = Prefs::default();
        let long = base(1, "Example Quest Longer (USA)");
        let short = base(2, "Example Quest (USA)");
        let group = [long, short.clone()];
        let picked = select_1g1r(&group, &prefs).unwrap();
        assert_eq!(picked.id, short.id);
    }

    #[test]
    fn select_prefers_lexical_name_on_length_tie() {
        let prefs = Prefs::default();
        let b = base(1, "Bxample Quest (USA)");
        let a = base(2, "Axample Quest (USA)");
        let group = [b, a.clone()];
        let picked = select_1g1r(&group, &prefs).unwrap();
        assert_eq!(picked.id, a.id);
    }

    #[test]
    fn select_prefers_lowest_id_on_full_tie() {
        let prefs = Prefs::default();
        let a = base(2, "Example Quest (USA)");
        let b = base(1, "Example Quest (USA)");
        let group = [a, b.clone()];
        let picked = select_1g1r(&group, &prefs).unwrap();
        assert_eq!(picked.id, b.id);
    }

    #[test]
    fn select_ranks_unl_last_but_not_hidden() {
        let prefs = Prefs::default();
        let mut unl = base(1, "Example Quest (Unl)");
        unl.flags = vec!["unl".to_string()];
        let mut only_option = vec![unl.clone()];
        assert_eq!(select_1g1r(&only_option, &prefs).unwrap().id, unl.id);
        let good = base(2, "Example Quest (USA)");
        only_option.push(good.clone());
        assert_eq!(select_1g1r(&only_option, &prefs).unwrap().id, good.id);
    }

    #[test]
    fn select_ranks_pirate_last_but_not_hidden() {
        let prefs = Prefs::default();
        let mut pirate = base(1, "Example Quest (Pirate)");
        pirate.flags = vec!["pirate".to_string()];
        let good = base(2, "Example Quest (USA)");
        let group = [pirate.clone(), good.clone()];
        let picked = select_1g1r(&group, &prefs).unwrap();
        assert_eq!(picked.id, good.id);
        let group = [pirate.clone()];
        assert_eq!(select_1g1r(&group, &prefs).unwrap().id, pirate.id);
    }

    #[test]
    fn select_prefers_non_unl_even_with_worse_region() {
        let prefs = Prefs::default();
        let mut unl_usa = base(1, "Example Quest (USA) (Unl)");
        unl_usa.flags = vec!["unl".to_string()];
        let mut japan_licensed = base(2, "Example Quest (Japan)");
        japan_licensed.regions = vec!["Japan".to_string()];
        let group = [unl_usa, japan_licensed.clone()];
        let picked = select_1g1r(&group, &prefs).unwrap();
        assert_eq!(picked.id, japan_licensed.id);
    }

    #[test]
    fn select_no_region_match_ranks_worse_than_any_match() {
        let prefs = Prefs::default();
        let mut unknown_region = base(1, "Example Quest (Elsewhere)");
        unknown_region.regions = vec!["Elsewhere".to_string()];
        let japan = base(2, "Example Quest (Japan)");
        let mut japan = japan;
        japan.regions = vec!["Japan".to_string()];
        let group = [unknown_region, japan.clone()];
        let picked = select_1g1r(&group, &prefs).unwrap();
        assert_eq!(picked.id, japan.id);
    }

    #[test]
    fn select_language_beats_worse_region_only_on_region_tie() {
        let prefs = Prefs::default();
        let mut usa_fr = base(1, "Example Quest (USA) (Fr)");
        usa_fr.languages = vec!["Fr".to_string()];
        let mut japan_en = base(2, "Example Quest (Japan) (En)");
        japan_en.regions = vec!["Japan".to_string()];
        let group = [usa_fr.clone(), japan_en];
        let picked = select_1g1r(&group, &prefs).unwrap();
        assert_eq!(
            picked.id, usa_fr.id,
            "region outranks language when regions differ"
        );
    }

    #[test]
    fn select_no_language_match_ranks_worse() {
        let prefs = Prefs::default();
        let mut de = base(1, "Example Quest (USA) (De)");
        de.languages = vec!["De".to_string()];
        let en = base(2, "Example Quest (USA) (En)");
        let group = [de, en.clone()];
        let picked = select_1g1r(&group, &prefs).unwrap();
        assert_eq!(picked.id, en.id);
    }

    #[test]
    fn select_prefers_revision_over_dump_status() {
        let prefs = Prefs::default();
        let mut bad_high_rev = base(1, "Example Quest (USA) (Rev 2)");
        bad_high_rev.revision_rank = Some(2);
        bad_high_rev.good_dump = false;
        let mut good_low_rev = base(2, "Example Quest (USA) (Rev 1)");
        good_low_rev.revision_rank = Some(1);
        let group = [bad_high_rev.clone(), good_low_rev];
        let picked = select_1g1r(&group, &prefs).unwrap();
        assert_eq!(
            picked.id, bad_high_rev.id,
            "revision (step 4) outranks dump status (step 5)"
        );
    }

    #[test]
    fn select_revision_beats_region_only_on_region_tie() {
        let prefs = Prefs::default();
        let mut usa_rev1 = base(1, "Example Quest (USA) (Rev 1)");
        usa_rev1.revision_rank = Some(1);
        let mut japan_rev2 = base(2, "Example Quest (Japan) (Rev 2)");
        japan_rev2.regions = vec!["Japan".to_string()];
        japan_rev2.revision_rank = Some(2);
        let group = [usa_rev1.clone(), japan_rev2];
        let picked = select_1g1r(&group, &prefs).unwrap();
        assert_eq!(picked.id, usa_rev1.id);
    }

    #[test]
    fn select_no_revision_treated_as_lowest() {
        let prefs = Prefs::default();
        let base_variant = base(1, "Example Quest (USA)");
        let mut rev1 = base(2, "Example Quest (USA) (Rev 1)");
        rev1.revision_rank = Some(1);
        let group = [base_variant, rev1.clone()];
        let picked = select_1g1r(&group, &prefs).unwrap();
        assert_eq!(
            picked.id, rev1.id,
            "with prefer_latest_revision, Rev 1 beats no tag"
        );
    }

    #[test]
    fn select_no_revision_wins_when_preferring_lowest() {
        let prefs = Prefs {
            prefer_latest_revision: false,
            ..Prefs::default()
        };
        let base_variant = base(1, "Example Quest (USA)");
        let mut rev1 = base(2, "Example Quest (USA) (Rev 1)");
        rev1.revision_rank = Some(1);
        let group = [base_variant.clone(), rev1];
        let picked = select_1g1r(&group, &prefs).unwrap();
        assert_eq!(picked.id, base_variant.id);
    }

    #[test]
    fn select_dumps_baddump_flag_does_not_by_itself_hide() {
        let prefs = Prefs::default();
        let mut bad = base(1, "Example Quest (USA) [b]");
        bad.flags = vec!["baddump".to_string()];
        bad.good_dump = false;
        assert!(!is_hidden(&bad, &prefs));
        let group = [bad.clone()];
        assert_eq!(select_1g1r(&group, &prefs).unwrap().id, bad.id);
    }

    #[test]
    fn select_other_flag_never_hides() {
        let prefs = Prefs::default();
        let mut other = base(1, "Example Quest (USA)");
        other.flags = vec!["other:alt".to_string()];
        assert!(!is_hidden(&other, &prefs));
    }

    #[test]
    fn select_custom_hide_list_can_be_narrower() {
        let prefs = Prefs {
            hide: vec![HiddenFlag::Bios],
            ..Prefs::default()
        };
        let mut demo = base(1, "Example Quest (Demo)");
        demo.flags = vec!["demo".to_string()];
        assert!(!is_hidden(&demo, &prefs));
        let group = [demo.clone()];
        assert_eq!(select_1g1r(&group, &prefs).unwrap().id, demo.id);
    }

    #[test]
    fn select_full_group_reproduces_documented_order() {
        let prefs = Prefs::default();
        let mut bios = base(1, "Example Quest (BIOS)");
        bios.flags = vec!["bios".to_string()];
        let mut unl = base(2, "Example Quest (Unl)");
        unl.flags = vec!["unl".to_string()];
        let mut japan_rev1 = base(3, "Example Quest (Japan) (Rev 1)");
        japan_rev1.regions = vec!["Japan".to_string()];
        japan_rev1.revision_rank = Some(1);
        let usa_rev1 = {
            let mut v = base(4, "Example Quest (USA) (Rev 1)");
            v.revision_rank = Some(1);
            v
        };
        let usa_rev2 = {
            let mut v = base(5, "Example Quest (USA) (Rev 2)");
            v.revision_rank = Some(2);
            v
        };
        let group = vec![bios, unl, japan_rev1, usa_rev1, usa_rev2.clone()];
        assert_eq!(select_1g1r(&group, &prefs).unwrap().id, usa_rev2.id);
    }

    #[test]
    fn rank_orders_by_region_then_language_then_revision() {
        let prefs = Prefs::default();
        let usa = base(1, "Example Quest (USA)");
        let mut japan = base(2, "Example Quest (Japan)");
        japan.regions = vec!["Japan".to_string()];
        assert!(rank(&usa, &prefs) < rank(&japan, &prefs));
    }

    #[test]
    fn infer_groups_groups_by_key_and_marks_inferred() {
        let items = vec![
            (1, "Example Quest".to_string()),
            (2, "Example Quest".to_string()),
            (3, "Other Adventure".to_string()),
        ];
        let groups = infer_groups(items.into_iter());
        assert_eq!(groups.len(), 2);
        assert!(groups.iter().all(|g| g.inferred));
        let eq = groups.iter().find(|g| g.key == "Example Quest").unwrap();
        assert_eq!(eq.member_ids, vec![1, 2]);
    }

    #[test]
    fn infer_groups_is_deterministic_regardless_of_input_order() {
        let forward = vec![
            (1, "Example Quest".to_string()),
            (2, "Example Quest".to_string()),
        ];
        let reversed = vec![
            (2, "Example Quest".to_string()),
            (1, "Example Quest".to_string()),
        ];
        assert_eq!(
            infer_groups(forward.into_iter()),
            infer_groups(reversed.into_iter())
        );
    }

    #[test]
    fn infer_groups_empty_input_yields_no_groups() {
        assert!(infer_groups(std::iter::empty()).is_empty());
    }

    proptest::proptest! {
        #[test]
        fn select_1g1r_is_permutation_invariant(
            perm_seed in 0u64..10_000,
            n in 2usize..7,
        ) {
            let prefs = Prefs::default();
            let regions = ["USA", "Europe", "Japan", "Elsewhere"];
            let languages = ["En", "Fr", "De"];
            let mut group: Vec<Variant> = (0..n as u64)
                .map(|i| {
                    let mut flags = Vec::new();
                    if i % 5 == 0 {
                        flags.push("unl".to_string());
                    }
                    Variant {
                        id: i,
                        name: format!("Example Quest {i}"),
                        regions: vec![regions[(i as usize + perm_seed as usize) % regions.len()].to_string()],
                        languages: vec![languages[(i as usize) % languages.len()].to_string()],
                        revision_rank: Some((i % 3) as u32),
                        flags,
                        good_dump: i % 4 != 0,
                    }
                })
                .collect();
            let baseline = select_1g1r(&group, &prefs).map(|v| v.id);

            // Deterministic shuffle keyed on perm_seed, no external RNG crate needed.
            let mut seed = perm_seed.wrapping_add(1);
            for i in (1..group.len()).rev() {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                let j = (seed >> 33) as usize % (i + 1);
                group.swap(i, j);
            }
            let shuffled = select_1g1r(&group, &prefs).map(|v| v.id);
            proptest::prop_assert_eq!(baseline, shuffled);
        }
    }
}
