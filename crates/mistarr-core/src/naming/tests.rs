use proptest::prelude::*;

use super::*;

struct Case {
    name: &'static str,
    base: &'static str,
    regions: &'static [Region],
    languages: &'static [&'static str],
    revision: Option<&'static str>,
    flags: &'static [&'static str],
    disc: Option<u32>,
}

const fn case(name: &'static str, base: &'static str) -> Case {
    Case {
        name,
        base,
        regions: &[],
        languages: &[],
        revision: None,
        flags: &[],
        disc: None,
    }
}

impl Case {
    const fn r(mut self, regions: &'static [Region]) -> Self {
        self.regions = regions;
        self
    }
    const fn l(mut self, languages: &'static [&'static str]) -> Self {
        self.languages = languages;
        self
    }
    const fn rev(mut self, label: &'static str) -> Self {
        self.revision = Some(label);
        self
    }
    const fn f(mut self, flags: &'static [&'static str]) -> Self {
        self.flags = flags;
        self
    }
    const fn d(mut self, disc: u32) -> Self {
        self.disc = Some(disc);
        self
    }

    fn check(&self) {
        let p = parse_name(self.name);
        assert_eq!(p.base_name, self.base, "base of {:?}", self.name);
        assert_eq!(p.regions, self.regions, "regions of {:?}", self.name);
        assert_eq!(p.languages, self.languages, "languages of {:?}", self.name);
        assert_eq!(
            p.revision.as_ref().map(|r| r.label.as_str()),
            self.revision,
            "revision of {:?}",
            self.name
        );
        let flags: Vec<String> = p.flags.iter().map(ToString::to_string).collect();
        assert_eq!(flags, self.flags, "flags of {:?}", self.name);
        assert_eq!(p.disc, self.disc, "disc of {:?}", self.name);
    }
}

use Region::{Asia, Europe, Japan, Usa, World};

// A flat table reads better than one split across helpers.
#[allow(clippy::too_many_lines)]
fn explicit_cases() -> Vec<Case> {
    vec![
        case("Example Quest", "Example Quest"),
        case("  Example Quest  ", "Example Quest"),
        case("", ""),
        case("(USA)", "").r(&[Usa]),
        case("Example Quest (USA)", "Example Quest").r(&[Usa]),
        case("Example Quest (usa)", "Example Quest").r(&[Usa]),
        case("Example Quest (USA, Europe)", "Example Quest").r(&[Usa, Europe]),
        case("Example Quest (USA,Europe)", "Example Quest").r(&[Usa, Europe]),
        case("Example Quest (USA, USA)", "Example Quest").r(&[Usa]),
        case("Example Quest (Japan, USA, Europe)", "Example Quest").r(&[Japan, Usa, Europe]),
        case("Example Quest (Hong Kong)", "Example Quest").r(&[Region::HongKong]),
        case("Example Quest (United Kingdom)", "Example Quest").r(&[Region::Uk]),
        case("Example Quest (USA) (Japan)", "Example Quest")
            .r(&[Usa])
            .f(&["other:Japan"]),
        case("Example Quest (USA, Atlantis)", "Example Quest").f(&["other:USA, Atlantis"]),
        case("Example Quest (Atlantis)", "Example Quest").f(&["other:Atlantis"]),
        case("Example Quest (Europe) (En,Fr,De)", "Example Quest")
            .r(&[Europe])
            .l(&["En", "Fr", "De"]),
        case("Example Quest (Europe) (En, Fr)", "Example Quest")
            .r(&[Europe])
            .l(&["En", "Fr"]),
        case("Example Quest (Asia) (Zh-Hant,En)", "Example Quest")
            .r(&[Asia])
            .l(&["Zh-Hant", "En"]),
        case("Example Quest (Brazil) (Pt-BR)", "Example Quest")
            .r(&[Region::Brazil])
            .l(&["Pt-BR"]),
        case("Example Quest (En)", "Example Quest").l(&["En"]),
        case("Example Quest (USA) (En) (Fr)", "Example Quest")
            .r(&[Usa])
            .l(&["En"])
            .f(&["other:Fr"]),
        case("Example Quest (USA) (EN)", "Example Quest")
            .r(&[Usa])
            .f(&["other:EN"]),
        case("Example Quest (USA) (Eng)", "Example Quest")
            .r(&[Usa])
            .f(&["other:Eng"]),
        case("Example Quest (USA) (Rev 1)", "Example Quest")
            .r(&[Usa])
            .rev("Rev 1"),
        case("Example Quest (USA) (Rev 12)", "Example Quest")
            .r(&[Usa])
            .rev("Rev 12"),
        case("Example Quest (USA) (Rev A)", "Example Quest")
            .r(&[Usa])
            .rev("Rev A"),
        case("Example Quest (USA) (rev b)", "Example Quest")
            .r(&[Usa])
            .rev("rev b"),
        case("Example Quest (USA) (Rev 1.1)", "Example Quest")
            .r(&[Usa])
            .rev("Rev 1.1"),
        case("Example Quest (USA) (Rev)", "Example Quest")
            .r(&[Usa])
            .f(&["other:Rev"]),
        case("Example Quest (USA) (Rev 1-2)", "Example Quest")
            .r(&[Usa])
            .f(&["other:Rev 1-2"]),
        case("Example Quest (USA) (v1.1)", "Example Quest")
            .r(&[Usa])
            .rev("v1.1"),
        case("Example Quest (USA) (v1.10)", "Example Quest")
            .r(&[Usa])
            .rev("v1.10"),
        case("Example Quest (USA) (v2.0.1)", "Example Quest")
            .r(&[Usa])
            .rev("v2.0.1"),
        case("Example Quest (USA) (v1.1a)", "Example Quest")
            .r(&[Usa])
            .rev("v1.1a"),
        case("Example Quest (USA) (V3)", "Example Quest")
            .r(&[Usa])
            .rev("V3"),
        case("Example Quest (USA) (Version 1.2)", "Example Quest")
            .r(&[Usa])
            .rev("Version 1.2"),
        case("Example Quest (USA) (vs)", "Example Quest")
            .r(&[Usa])
            .f(&["other:vs"]),
        case("Example Quest (USA) (v1.1 Extra)", "Example Quest")
            .r(&[Usa])
            .f(&["other:v1.1 Extra"]),
        case("Example Quest (USA) (Beta)", "Example Quest")
            .r(&[Usa])
            .rev("Beta")
            .f(&["beta"]),
        case("Example Quest (USA) (Beta 3)", "Example Quest")
            .r(&[Usa])
            .rev("Beta 3")
            .f(&["beta"]),
        case("Example Quest (USA) (Beta Extra)", "Example Quest")
            .r(&[Usa])
            .f(&["other:Beta Extra"]),
        case("Example Quest (USA) (Proto)", "Example Quest")
            .r(&[Usa])
            .rev("Proto")
            .f(&["proto"]),
        case("Example Quest (USA) (Proto 2)", "Example Quest")
            .r(&[Usa])
            .rev("Proto 2")
            .f(&["proto"]),
        case("Example Quest (USA) (Prototype)", "Example Quest")
            .r(&[Usa])
            .rev("Prototype")
            .f(&["proto"]),
        case("Example Quest (USA) (Alt)", "Example Quest")
            .r(&[Usa])
            .rev("Alt"),
        case("Example Quest (USA) (Alt 2)", "Example Quest")
            .r(&[Usa])
            .rev("Alt 2"),
        case("Example Quest (USA) (Rev 1) (Alt)", "Example Quest")
            .r(&[Usa])
            .rev("Rev 1, Alt"),
        case("Example Quest (USA) (v1.1) (Beta 2)", "Example Quest")
            .r(&[Usa])
            .rev("v1.1, Beta 2")
            .f(&["beta"]),
        case("Example Quest (USA) (Demo)", "Example Quest")
            .r(&[Usa])
            .f(&["demo"]),
        case("Example Quest (USA) (Demo 2)", "Example Quest")
            .r(&[Usa])
            .f(&["demo"]),
        case("Example Quest (USA) (Kiosk)", "Example Quest")
            .r(&[Usa])
            .f(&["demo"]),
        case("Example Quest (USA) (Kiosk Demo)", "Example Quest")
            .r(&[Usa])
            .f(&["demo"]),
        case("Example Quest (USA) (Demo) (Kiosk)", "Example Quest")
            .r(&[Usa])
            .f(&["demo"]),
        case("Example Quest (USA) (Sample)", "Example Quest")
            .r(&[Usa])
            .f(&["sample"]),
        case("Example Quest (USA) (Unl)", "Example Quest")
            .r(&[Usa])
            .f(&["unl"]),
        case("Example Quest (USA) (Unlicensed)", "Example Quest")
            .r(&[Usa])
            .f(&["unl"]),
        case("Example Quest (USA) (Pirate)", "Example Quest")
            .r(&[Usa])
            .f(&["pirate"]),
        case("Example Quest (USA) (Program)", "Example Quest")
            .r(&[Usa])
            .f(&["program"]),
        case("Example Quest (USA) (BIOS)", "Example Quest")
            .r(&[Usa])
            .f(&["bios"]),
        case("[BIOS] Example Firmware (Japan) (v1.0)", "Example Firmware")
            .r(&[Japan])
            .rev("v1.0")
            .f(&["bios"]),
        case("[BIOS] [b] Example Firmware (World)", "Example Firmware")
            .r(&[World])
            .f(&["bios", "baddump"]),
        case("Example Quest (USA) [b]", "Example Quest")
            .r(&[Usa])
            .f(&["baddump"]),
        case("Example Quest (USA) [b2]", "Example Quest")
            .r(&[Usa])
            .f(&["baddump"]),
        case("Example Quest (USA) [b] [b]", "Example Quest")
            .r(&[Usa])
            .f(&["baddump"]),
        case("Example Quest (USA) [!]", "Example Quest")
            .r(&[Usa])
            .f(&["other:[!]"]),
        case("Example Quest (USA) [T+Eng]", "Example Quest")
            .r(&[Usa])
            .f(&["other:[T+Eng]"]),
        case("Example Quest (USA) [bx]", "Example Quest")
            .r(&[Usa])
            .f(&["other:[bx]"]),
        case("Example Quest [b] (USA)", "Example Quest")
            .r(&[Usa])
            .f(&["baddump"]),
        case("Example Saga (USA) (Disc 1)", "Example Saga")
            .r(&[Usa])
            .d(1),
        case("Example Saga (USA) (Disc 12)", "Example Saga")
            .r(&[Usa])
            .d(12),
        case("Example Saga (USA) (Disc A)", "Example Saga")
            .r(&[Usa])
            .f(&["other:Disc A"]),
        case(
            "Example Saga (Europe) (En,De) (Disc 2) (Rev 1)",
            "Example Saga",
        )
        .r(&[Europe])
        .l(&["En", "De"])
        .rev("Rev 1")
        .d(2),
        case("Example Saga (USA) (Track 01)", "Example Saga")
            .r(&[Usa])
            .f(&["other:Track 01"]),
        case("Example Quest (USA) (Collector's Edition)", "Example Quest")
            .r(&[Usa])
            .f(&["other:Collector's Edition"]),
        case("Example Quest (USA) (2001-05-12)", "Example Quest")
            .r(&[Usa])
            .f(&["other:2001-05-12"]),
        case("Example Quest (USA) ()", "Example Quest").r(&[Usa]),
        case("Example Quest (USA) (  )", "Example Quest").r(&[Usa]),
        case("Example Quest (USA", "Example Quest (USA"),
        case("Example Quest USA)", "Example Quest USA)"),
        case("Example Quest (USA) (Rev 1", "Example Quest").r(&[Usa]),
        case("Example Quest (USA (Europe))", "Example Quest").f(&["other:USA (Europe)"]),
        case("Example Quest (USA) - Extra (Rev 1)", "Example Quest")
            .r(&[Usa])
            .rev("Rev 1"),
        case("Example Quest [", "Example Quest ["),
        case("Example Quest ]", "Example Quest ]"),
        case("Example: The Quest (World)", "Example: The Quest").r(&[World]),
        case("Example Quest, The (USA)", "Example Quest, The").r(&[Usa]),
        case("Exämple Qüest (Europe)", "Exämple Qüest").r(&[Europe]),
        case("Example Quest (USA) (Beta) (Beta)", "Example Quest")
            .r(&[Usa])
            .rev("Beta, Beta")
            .f(&["beta"]),
        case("Example Quest (Unknown)", "Example Quest").r(&[Region::Unknown]),
        case("Example Quest (World) (Unl) (Pirate)", "Example Quest")
            .r(&[World])
            .f(&["unl", "pirate"]),
        case("Example Quest (Sample) (USA)", "Example Quest")
            .r(&[Usa])
            .f(&["sample"]),
    ]
}

#[test]
fn explicit_table() {
    for case in explicit_cases() {
        case.check();
    }
}

const BASES: &[&str] = &[
    "Example Quest",
    "Sample Racer 2",
    "Tiny Puzzle - Deluxe",
    "Demo Land, The",
    "Homebrew & Co",
];

const REGION_TAGS: &[(&str, &[Region])] = &[
    ("", &[]),
    (" (USA)", &[Usa]),
    (" (Europe)", &[Europe]),
    (" (Japan)", &[Japan]),
    (" (World)", &[World]),
    (" (USA, Europe)", &[Usa, Europe]),
    (" (Japan, Asia)", &[Japan, Asia]),
    (" (Australia)", &[Region::Australia]),
    (" (Brazil)", &[Region::Brazil]),
    (" (Canada)", &[Region::Canada]),
    (" (China)", &[Region::China]),
    (" (France)", &[Region::France]),
    (" (Germany)", &[Region::Germany]),
    (" (Italy)", &[Region::Italy]),
    (" (Korea)", &[Region::Korea]),
    (" (Netherlands)", &[Region::Netherlands]),
    (" (Spain)", &[Region::Spain]),
    (" (Sweden)", &[Region::Sweden]),
    (" (Taiwan)", &[Region::Taiwan]),
    (" (UK)", &[Region::Uk]),
    (" (Unknown)", &[Region::Unknown]),
];

type Extra = (
    &'static str,
    &'static [&'static str],
    Option<&'static str>,
    &'static [&'static str],
    Option<u32>,
);

const EXTRA_TAGS: &[Extra] = &[
    ("", &[], None, &[], None),
    (" (En,Fr)", &["En", "Fr"], None, &[], None),
    (" (Ja)", &["Ja"], None, &[], None),
    (" (Rev 1)", &[], Some("Rev 1"), &[], None),
    (" (Rev A)", &[], Some("Rev A"), &[], None),
    (" (v1.1)", &[], Some("v1.1"), &[], None),
    (" (Beta)", &[], Some("Beta"), &["beta"], None),
    (" (Beta 2)", &[], Some("Beta 2"), &["beta"], None),
    (" (Proto)", &[], Some("Proto"), &["proto"], None),
    (" (Proto 1)", &[], Some("Proto 1"), &["proto"], None),
    (" (Alt)", &[], Some("Alt"), &[], None),
    (" (Alt 3)", &[], Some("Alt 3"), &[], None),
    (" (Demo)", &[], None, &["demo"], None),
    (" (Sample)", &[], None, &["sample"], None),
    (" (Unl)", &[], None, &["unl"], None),
    (" (Pirate)", &[], None, &["pirate"], None),
    (" (Program)", &[], None, &["program"], None),
    (" (BIOS)", &[], None, &["bios"], None),
    (" [b]", &[], None, &["baddump"], None),
    (" (Disc 2)", &[], None, &[], Some(2)),
    (" (Limited Run)", &[], None, &["other:Limited Run"], None),
    (" [!]", &[], None, &["other:[!]"], None),
    (
        " (En,De) (Rev 2) (Disc 1) [b]",
        &["En", "De"],
        Some("Rev 2"),
        &["baddump"],
        Some(1),
    ),
];

#[test]
fn generated_corpus() {
    let mut count = 0;
    for base in BASES {
        for (region_tag, regions) in REGION_TAGS {
            for &(extra, languages, revision, flags, disc) in EXTRA_TAGS {
                let name = format!("{base}{region_tag}{extra}");
                let p = parse_name(&name);
                assert_eq!(p.base_name, *base, "{name}");
                assert_eq!(p.regions, *regions, "{name}");
                assert_eq!(p.languages, languages, "{name}");
                assert_eq!(p.revision.map(|r| r.label).as_deref(), revision, "{name}");
                let got: Vec<String> = p.flags.iter().map(ToString::to_string).collect();
                assert_eq!(got, flags, "{name}");
                assert_eq!(p.disc, disc, "{name}");
                count += 1;
            }
        }
    }
    assert!(count + explicit_cases().len() >= 200);
}

fn rank(name: &str) -> RevisionRank {
    parse_name(name).revision_rank()
}

#[test]
fn revision_ranks_order() {
    let ordered = [
        "X (Proto 1)",
        "X (Proto 2)",
        "X (Beta)",
        "X (Beta 1)",
        "X (Beta 2)",
        "X (USA)",
        "X (Alt)",
        "X (Alt 2)",
        "X (Rev 1)",
        "X (Rev 1) (Alt)",
        "X (Rev 2)",
        "X (v1.9)",
        "X (v1.10)",
        "X (v2.0)",
    ];
    for pair in ordered.windows(2) {
        assert!(rank(pair[0]) < rank(pair[1]), "{} < {}", pair[0], pair[1]);
    }
}

#[test]
fn revision_rank_equivalences() {
    assert_eq!(rank("X (Rev A)"), rank("X (Rev 1)"));
    assert_eq!(rank("X (Rev B)"), rank("X (v1.2)"));
    assert_eq!(rank("X (v1.0)"), RevisionRank::RELEASE);
    assert_eq!(rank("X"), RevisionRank::default());
    assert!(rank("X (v1.0a)") > rank("X (v1.0)"));
    assert!(rank("X (v1.0a)") < rank("X (v1.1)"));
    assert!(rank("X (v0.9)") < RevisionRank::RELEASE);
    assert!(rank("X (v1.1) (Beta)") > RevisionRank::RELEASE);
    assert!(rank("X (Rev 1.1)") > rank("X (Rev 1)"));
    assert!(rank("X (Rev ZZZZZZZZZZZZ)") > rank("X (Rev Z)"));
    assert_eq!(rank("X (Rev 99999999999)"), RevisionRank::RELEASE);
}

#[test]
fn region_table_round_trips() {
    for &(label, region) in REGIONS {
        assert_eq!(Region::from_name(label), Some(region), "{label}");
        assert_eq!(Region::from_name(&label.to_ascii_uppercase()), Some(region));
        assert_eq!(Region::from_name(region.name()), Some(region));
    }
    assert_eq!(Region::Uk.name(), "UK");
    assert_eq!(Region::Usa.to_string(), "USA");
    assert_eq!(Region::from_name(" Europe "), Some(Europe));
    assert_eq!(Region::from_name(""), None);
}

#[test]
fn flag_display_and_labels() {
    let labels: Vec<String> = [
        Flag::Bios,
        Flag::Beta,
        Flag::Proto,
        Flag::Demo,
        Flag::Sample,
        Flag::Unlicensed,
        Flag::Pirate,
        Flag::Program,
        Flag::BadDump,
        Flag::Other("Kiosk Edition".into()),
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    assert_eq!(
        labels,
        [
            "bios",
            "beta",
            "proto",
            "demo",
            "sample",
            "unl",
            "pirate",
            "program",
            "baddump",
            "other:Kiosk Edition"
        ]
    );
    let p = parse_name("Example Saga (USA) (Disc 3) (Beta) [b]");
    assert_eq!(p.flag_labels(), ["beta", "baddump", "disc:3"]);
    assert!(p.has_flag(&Flag::BadDump));
    assert!(!p.has_flag(&Flag::Demo));
    assert_eq!(
        parse_name("Example Saga").flag_labels(),
        Vec::<String>::new()
    );
}

#[test]
fn revision_display() {
    let p = parse_name("Example Quest (USA) (Rev 2) (Alt)");
    assert_eq!(
        p.revision.map(|r| r.to_string()).as_deref(),
        Some("Rev 2, Alt")
    );
}

#[test]
fn normalize_examples() {
    let cases = [
        ("Example Quest", "example quest"),
        ("  Example\t\n Quest  ", "example quest"),
        ("Example: Quest", "example_ quest"),
        ("A&B*C/D:E`F<G>H?I\\J|K\"L", "a_b_c_d_e_f_g_h_i_j_k_l"),
        ("ＥＸＡＭＰＬＥ　Ｑｕｅｓｔ", "example quest"),
        ("Example Quest？", "example quest_"),
        ("Cafe\u{301} Quest", "café quest"),
        ("Example ⅱ", "example ii"),
        ("Example\u{a0}Quest", "example quest"),
        ("", ""),
        ("   ", ""),
    ];
    for (input, expected) in cases {
        assert_eq!(normalize_for_match(input), expected, "{input:?}");
    }
}

#[test]
fn group_keys() {
    let key = |n: &str| group_key(&parse_name(n));
    assert_eq!(
        key("Example Quest (USA)"),
        key("Example Quest (Europe) (Rev 1)")
    );
    assert_eq!(
        key("Example Quest (USA)"),
        key("example quest (Japan) (Beta)")
    );
    assert_eq!(
        key("Example Saga (USA) (Disc 1)"),
        key("Example Saga (USA) (Disc 2)")
    );
    assert_eq!(key("[BIOS] Example Firmware (Japan)"), "example firmware");
    assert_ne!(key("Example Quest (USA)"), key("Example Quest 2 (USA)"));
    assert_eq!(key("Example: Quest (USA)"), "example_ quest");
}

fn tagged_name() -> impl Strategy<Value = String> {
    let tag = prop_oneof![
        Just("(USA)".to_owned()),
        Just("(En,Fr)".to_owned()),
        Just("[b]".to_owned()),
        "\\((Rev|Beta|Proto|Alt|Disc|v) ?[0-9A-Za-z.]{0,4}\\)",
        "\\([ -~]{0,12}\\)",
        "\\[[ -~]{0,6}\\]",
        "[()\\[\\]]",
    ];
    ("[A-Za-z0-9 ]{0,16}", prop::collection::vec(tag, 0..6))
        .prop_map(|(base, tags)| format!("{base} {}", tags.join(" ")))
}

proptest! {
    #[test]
    fn parse_never_panics(s in any::<String>()) {
        let p = parse_name(&s);
        prop_assert!(s.contains(p.base_name.as_str()));
    }

    #[test]
    fn parse_tagged_never_panics(s in tagged_name()) {
        let p = parse_name(&s);
        let _ = p.flag_labels();
        let _ = group_key(&p);
    }

    #[test]
    fn normalize_is_idempotent(s in any::<String>()) {
        let once = normalize_for_match(&s);
        prop_assert_eq!(normalize_for_match(&once), once);
    }

    #[test]
    fn normalize_tagged_is_idempotent(s in tagged_name()) {
        let once = normalize_for_match(&s);
        prop_assert_eq!(normalize_for_match(&once), once);
    }
}
