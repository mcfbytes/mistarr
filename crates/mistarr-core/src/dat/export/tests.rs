use std::fmt::Write as _;
use std::io::{BufReader, Cursor, Write as _};

use proptest::prelude::*;

use crate::dat::{
    export_name, export_parents, parse_dat, parse_dat_pack, parse_dat_with, DatError, DatFormat,
    DatGame, DatRom, DatStream, ExportOptions, RomStatus,
};
use crate::hash::HeaderRule;

/// One synthetic dump: the headerless hashes a board compares, and its iNES header.
#[derive(Debug, Clone)]
struct Dump {
    size: u64,
    seed: u64,
    header: Vec<u8>,
}

/// One synthetic game, rendered as either DAT form.
#[derive(Debug, Clone)]
struct Game {
    name: String,
    parent: Option<usize>,
    regions: Vec<String>,
    languages: Vec<String>,
    dump: Option<Dump>,
    sources: usize,
    /// `bad="1"` on the dump's files.
    bad: bool,
    /// `mia="1"` on the dump's files, which leaves the dump verifiable.
    mia: bool,
    /// `forcename` on the dump's files.
    forcename: Option<String>,
    /// `archive@status`, which only the export form carries.
    status: Option<String>,
}

impl Game {
    /// The attributes every file of the dump carries besides its hashes.
    fn marks(&self) -> String {
        let mut m = String::new();
        if self.bad {
            m.push_str(" bad=\"1\"");
        }
        if self.mia {
            m.push_str(" mia=\"1\"");
        }
        if let Some(f) = &self.forcename {
            write!(m, " forcename=\"{f}\"").unwrap();
        }
        m
    }
}

fn hex(seed: u64, len: usize) -> String {
    let mut out = String::new();
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    while out.len() < len {
        write!(out, "{x:016x}").unwrap();
        x = x.rotate_left(23).wrapping_mul(0x2545_F491_4F6C_DD1D);
    }
    out.truncate(len);
    out
}

fn header_hex(header: &[u8]) -> String {
    header
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn number(i: usize) -> String {
    format!("{:04}", i + 1)
}

/// The DB export form: `<header>`, then `<datafile>` whose games repeat each dump per source.
fn export_xml(games: &[Game]) -> String {
    let mut x = String::from(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<header>\n\t<version>20260101-000000</version>\n\
         \t<author>tester</author>\n\t<url>none</url>\n</header>\n<datafile>\n",
    );
    for (i, g) in games.iter().enumerate() {
        let clone = g.parent.map_or_else(|| "P".to_owned(), number);
        let status = g
            .status
            .as_ref()
            .map(|s| format!(" status=\"{s}\""))
            .unwrap_or_default();
        writeln!(
            x,
            "\t<game name=\"{}\">\n\t\t<archive number=\"{}\" clone=\"{clone}\" regparent=\"\" name=\"{}\" \
             region=\"{}\" languages=\"{}\" langchecked=\"yes\"{status}/>",
            g.name,
            number(i),
            g.name,
            g.regions.join(", "),
            g.languages.join(","),
        )
        .unwrap();
        let marks = g.marks();
        for s in 0..g.sources {
            writeln!(
                x,
                "\t\t<source>\n\t\t\t<details id=\"{s}\" section=\"Trusted Dump\" region=\"{}\" originalformat=\"Headerless\"/>\n\
                 \t\t\t<serials media_serial1=\"\"/>",
                g.regions.join(", ")
            )
            .unwrap();
            if let Some(d) = &g.dump {
                let whole = d.size + d.header.len() as u64;
                writeln!(
                    x,
                    "\t\t\t<file id=\"{s}1\" extension=\"nes\" size=\"{whole}\" crc32=\"{}\" md5=\"{}\" \
                     sha1=\"{}\" sha256=\"{}\" header=\"{}\" format=\"Headered\"{marks}/>\n\
                     \t\t\t<file id=\"{s}2\" extension=\"unh\" size=\"{}\" crc32=\"{}\" md5=\"{}\" sha1=\"{}\" \
                     sha256=\"{}\" format=\"Headerless\"{marks}/>\n\
                     \t\t\t<file id=\"{s}3\" extension=\"sav\" size=\"8192\" crc32=\"{}\" sha1=\"{}\" format=\"Headerless\"/>\n\
                     \t\t\t<file id=\"{s}4\" extension=\"bin\" size=\"4096\" crc32=\"{}\" sha1=\"{}\" \
                     format=\"Headerless\" item=\"Unfixed\" note=\"extra chip\"/>",
                    hex(d.seed ^ 1, 8),
                    hex(d.seed ^ 2, 32),
                    hex(d.seed ^ 3, 40),
                    hex(d.seed ^ 4, 64),
                    header_hex(&d.header),
                    d.size,
                    hex(d.seed, 8),
                    hex(d.seed, 32),
                    hex(d.seed, 40),
                    hex(d.seed, 64),
                    hex(d.seed ^ 5, 8),
                    hex(d.seed ^ 5, 40),
                    hex(d.seed ^ 6, 8),
                    hex(d.seed ^ 6, 40),
                )
                .unwrap();
            }
            x.push_str("\t\t</source>\n");
        }
        writeln!(
            x,
            "\t\t<release name=\"{}\" region=\"Elsewhere\"/>\n\t</game>",
            g.name
        )
        .unwrap();
    }
    x.push_str("</datafile>\n");
    x
}

/// The Logiqx form of the same games, as a headerless DAT with the header recorded.
fn logiqx_xml(games: &[Game]) -> String {
    let mut x = String::from(
        "<?xml version=\"1.0\"?>\n<datafile>\n<header><name>Example Vendor - Example System</name>\
         <version>20260101-000000</version></header>\n",
    );
    for g in games {
        write!(x, "<game name=\"{}\"", g.name).unwrap();
        if let Some(p) = g.parent {
            write!(x, " cloneof=\"{}\"", games[p].name).unwrap();
        }
        x.push('>');
        for (k, r) in g.regions.iter().enumerate() {
            write!(x, "<release name=\"{}\" region=\"{r}\"", g.name).unwrap();
            if k == 0 && !g.languages.is_empty() {
                write!(x, " language=\"{}\"", g.languages.join(",")).unwrap();
            }
            x.push_str("/>");
        }
        if g.regions.is_empty() && !g.languages.is_empty() {
            write!(
                x,
                "<release name=\"{}\" language=\"{}\"/>",
                g.name,
                g.languages.join(",")
            )
            .unwrap();
        }
        if let Some(d) = g.dump.as_ref().filter(|_| g.sources > 0) {
            let name = g
                .forcename
                .clone()
                .unwrap_or_else(|| format!("{}.nes", g.name));
            write!(
                x,
                "<rom name=\"{name}\" size=\"{}\" crc=\"{}\" md5=\"{}\" sha1=\"{}\" header=\"{}\"",
                d.size,
                hex(d.seed, 8),
                hex(d.seed, 32),
                hex(d.seed, 40),
                header_hex(&d.header)
            )
            .unwrap();
            if g.bad {
                x.push_str(" status=\"baddump\"");
            }
            x.push_str("/>");
        }
        x.push_str("</game>\n");
    }
    x.push_str("</datafile>\n");
    x
}

fn ines_options() -> ExportOptions {
    ExportOptions {
        header_rule: HeaderRule::Ines,
        extension: Some("nes".into()),
        ..ExportOptions::default()
    }
}

fn ines_header(prg: u8) -> Vec<u8> {
    let mut h = b"NES\x1a".to_vec();
    h.extend_from_slice(&[prg, 1]);
    h.resize(16, 0);
    h
}

/// A clone listed before its parent, a parent with two sources, a bad dump, a game without
/// files, a dump marked missing, and a prototype by status only whose files carry a forced name.
fn fixture() -> Vec<Game> {
    let dump = |seed, prg| Dump {
        size: 16_384 * u64::from(prg),
        seed,
        header: ines_header(prg),
    };
    let game = |name: &str, parent, region: &str, lang: &str, d, sources| Game {
        name: name.into(),
        parent,
        regions: vec![region.into()],
        languages: vec![lang.into()],
        dump: d,
        sources,
        bad: false,
        mia: false,
        forcename: None,
        status: None,
    };
    let quest_usa = game(
        "Example Quest (USA)",
        Some(1),
        "USA",
        "En",
        Some(dump(11, 2)),
        1,
    );
    let quest_japan = game(
        "Example Quest (Japan)",
        None,
        "Japan",
        "Ja",
        Some(dump(12, 2)),
        2,
    );
    let racer = Game {
        bad: true,
        ..game(
            "Sample Racer (Europe)",
            None,
            "Europe",
            "En",
            Some(dump(13, 1)),
            3,
        )
    };
    let dungeon = game("Demo Dungeon (World)", None, "World", "En", None, 1);
    let trail = Game {
        mia: true,
        ..game(
            "Trial Trail (Japan)",
            None,
            "Japan",
            "Ja",
            Some(dump(14, 1)),
            1,
        )
    };
    let manor = Game {
        forcename: Some("Mock Manor (USA) (Alt).nes".into()),
        status: Some("Proto".into()),
        ..game("Mock Manor (USA)", None, "USA", "En", Some(dump(15, 1)), 1)
    };
    vec![quest_usa, quest_japan, racer, dungeon, trail, manor]
}

#[test]
fn a_db_export_yields_headerless_roms_named_like_the_dat() {
    let xml = export_xml(&fixture());
    let dat = parse_dat_with(xml.as_bytes(), ines_options()).unwrap();
    assert_eq!(dat.header.name, "");
    assert_eq!(dat.header.version, "20260101-000000");
    assert_eq!(dat.header.author.as_deref(), Some("tester"));
    let names: Vec<&str> = dat.games.iter().map(|g| g.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "Example Quest (USA)",
            "Example Quest (Japan)",
            "Sample Racer (Europe)",
            "Demo Dungeon (World)",
            "Trial Trail (Japan)",
            "Mock Manor (USA)"
        ]
    );
    let clone = &dat.games[0];
    assert_eq!(clone.clone_of.as_deref(), Some("Example Quest (Japan)"));
    assert_eq!(clone.regions, ["USA"]);
    assert_eq!(clone.languages, ["En"]);
    assert_eq!(
        clone.roms,
        [DatRom {
            name: "Example Quest (USA).nes".into(),
            size: 32_768,
            crc32: Some(hex(11, 8)),
            md5: Some(hex(11, 32)),
            sha1: Some(hex(11, 40)),
            status: RomStatus::Good,
            header: Some(header_hex(&ines_header(2))),
        }]
    );
    let parent = &dat.games[1];
    assert_eq!(parent.clone_of, None);
    assert_eq!(parent.roms.len(), 1, "two sources of one dump are one rom");
    assert!(
        dat.games.iter().all(|g| g.roms.len() <= 1),
        "extra items and save files are not roms"
    );
    assert_eq!(dat.games[2].roms[0].status, RomStatus::BadDump);
    assert!(dat.games[3].roms.is_empty());
    assert_eq!(
        dat.games[4].roms[0].status,
        RomStatus::Good,
        "mia keeps the dump"
    );
    let manor = &dat.games[5];
    assert_eq!(manor.roms[0].name, "Mock Manor (USA) (Alt).nes");
    assert_eq!(manor.roms[0].status, RomStatus::Good);
    assert_eq!(manor.status.as_deref(), Some("Proto"));
    assert_eq!(manor.status_flags(), [crate::naming::Flag::Proto]);
    assert!(dat.games.iter().all(|g| g.description.is_none()));
}

#[test]
fn archive_status_maps_to_stage_flags() {
    use crate::naming::Flag;
    let flags = |status: Option<&str>| {
        DatGame {
            name: "Example Quest (World)".into(),
            clone_of: None,
            rom_of: None,
            description: None,
            category: None,
            regions: Vec::new(),
            languages: Vec::new(),
            status: status.map(str::to_owned),
            roms: Vec::new(),
        }
        .status_flags()
    };
    assert_eq!(flags(Some("Beta")), [Flag::Beta]);
    assert_eq!(flags(Some("Beta 2")), [Flag::Beta]);
    assert_eq!(flags(Some("Proto 3")), [Flag::Proto]);
    assert_eq!(flags(Some("Possible Proto")), [Flag::Proto]);
    assert_eq!(flags(Some("Demo")), [Flag::Demo]);
    assert_eq!(flags(Some("sample")), [Flag::Sample]);
    assert!(flags(Some("Verified")).is_empty());
    assert!(flags(None).is_empty());
}

#[test]
fn without_a_header_rule_the_headered_file_is_taken() {
    let xml = export_xml(&fixture());
    let dat = parse_dat(xml.as_bytes()).unwrap();
    let rom = &dat.games[1].roms[0];
    assert_eq!(rom.name, "Example Quest (Japan).nes");
    assert_eq!(rom.size, 32_768 + 16);
    assert_eq!(rom.sha1.as_deref(), Some(hex(0b1100 ^ 0b11, 40).as_str()));
    assert_eq!(rom.header, None);
}

#[test]
fn a_single_pass_leaves_clones_unlinked() {
    let xml = export_xml(&fixture());
    let stream = DatStream::with_options(xml.as_bytes(), ines_options()).unwrap();
    assert_eq!(stream.format(), DatFormat::DbExport);
    let games: Vec<DatGame> = stream.collect::<Result<_, _>>().unwrap();
    assert!(games.iter().all(|g| g.clone_of.is_none()));
    let parents = export_parents(xml.as_bytes()).unwrap().unwrap();
    assert_eq!(parents.len(), 5, "the clone is not indexed");
    assert_eq!(parents["0002"], "Example Quest (Japan)");
    assert!(!parents.contains_key("0001"));
}

#[test]
fn a_zipped_export_links_its_clones() {
    let xml = export_xml(&fixture());
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let member = "Example Vendor - Example System (DB Export) (20260101-000000).xml";
    zip.start_file(member, zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(xml.as_bytes()).unwrap();
    let bytes = zip.finish().unwrap().into_inner();
    let members: Vec<_> = parse_dat_pack(Cursor::new(bytes))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].file_name, member);
    let games = &members[0].dat.games;
    assert_eq!(games[0].clone_of.as_deref(), Some("Example Quest (Japan)"));
    let name = export_name(&members[0].file_name).unwrap();
    assert_eq!(name.dat_name, "Example Vendor - Example System (DB Export)");
}

#[test]
fn n64_and_plain_platforms_take_their_format() {
    let xml = r#"<header/><datafile><game name="Example Quest (World)"><archive number="1" clone="P"/>
      <source><file extension="v64" size="8" crc32="00000001" format="ByteSwapped"/>
      <file extension="z64" size="8" crc32="00000002" format="BigEndian"/></source>
      <source><file extension="bin" size="8" crc32="00000003" format="Default"/></source></game></datafile>"#;
    let n64 = ExportOptions {
        header_rule: HeaderRule::N64,
        ..ExportOptions::default()
    };
    let dat = parse_dat_with(xml.as_bytes(), n64).unwrap();
    assert_eq!(dat.games[0].roms.len(), 1);
    assert_eq!(dat.games[0].roms[0].name, "Example Quest (World).z64");
    let dat = parse_dat(xml.as_bytes()).unwrap();
    assert_eq!(dat.games[0].roms[0].name, "Example Quest (World).bin");
}

#[test]
fn a_headerless_file_without_a_platform_extension_takes_the_headered_one() {
    let xml = export_xml(&fixture()[1..2]);
    let options = ExportOptions {
        header_rule: HeaderRule::Ines,
        ..ExportOptions::default()
    };
    let dat = parse_dat_with(xml.as_bytes(), options).unwrap();
    assert_eq!(dat.games[0].roms[0].name, "Example Quest (Japan).nes");
}

fn platform(rule: HeaderRule, written: &str, loads: &[&str]) -> ExportOptions {
    ExportOptions {
        header_rule: rule,
        extension: Some(written.into()),
        load_extensions: loads.iter().map(|e| (*e).to_owned()).collect(),
        ..ExportOptions::default()
    }
}

#[test]
fn any_loaded_extension_is_an_image_and_a_lone_file_is_renamed() {
    let xml = r#"<header/><datafile>
      <game name="Example Quest (World)"><archive number="1" clone="P"/>
      <source><file extension="gen" size="8" crc32="00000001"/>
      <file extension="srm" size="8" crc32="00000002"/></source></game>
      <game name="Sample Racer (World)"><archive number="2" clone="P"/>
      <source><file extension="pce" size="8" crc32="00000003"/>
      <file extension="txt" size="8" crc32="00000004" item="Manual"/></source></game>
      <game name="Demo Dungeon (World)"><archive number="3" clone="P"/>
      <source><file extension="aaa" size="8" crc32="00000005"/>
      <file extension="bbb" size="8" crc32="00000006"/></source></game>
    </datafile>"#;
    let md = parse_dat_with(
        xml.as_bytes(),
        platform(HeaderRule::None, "md", &["md", "gen", "bin"]),
    )
    .unwrap();
    assert_eq!(md.games[0].roms.len(), 1);
    assert_eq!(md.games[0].roms[0].name, "Example Quest (World).gen");
    let sgx = parse_dat_with(xml.as_bytes(), platform(HeaderRule::None, "sgx", &["sgx"])).unwrap();
    assert_eq!(sgx.games[1].roms[0].name, "Sample Racer (World).sgx");
    assert_eq!(sgx.games[1].roms[0].crc32.as_deref(), Some("00000003"));
    assert!(
        sgx.games[2].roms.is_empty(),
        "two unknown files give no image"
    );
}

#[test]
fn lynx_takes_the_headerless_image_under_the_written_name() {
    let xml = r#"<header/><datafile><game name="Example Quest (World)"><archive number="1" clone="P"/>
      <source><file extension="lnx" size="80" crc32="00000001" header="4c594e5800" format="Headered"/>
      <file extension="lyx" size="16" crc32="00000002" format="Headerless"/></source></game></datafile>"#;
    let dat = parse_dat_with(xml.as_bytes(), platform(HeaderRule::Lnx, "lnx", &["lnx"])).unwrap();
    let rom = &dat.games[0].roms[0];
    assert_eq!(rom.name, "Example Quest (World).lnx");
    assert_eq!(rom.crc32.as_deref(), Some("00000002"));
    assert_eq!(rom.header.as_deref(), Some("4c594e5800"));
}

#[test]
fn a_good_dump_wins_over_a_bad_one_of_the_same_name() {
    let xml = r#"<header/><datafile><game name="Example Quest (World)"><archive number="1" clone="P"/>
      <source><file extension="gb" size="8" crc32="00000001" bad="1"/></source>
      <source><file extension="gb" size="8" crc32="00000002" mia="1"/></source></game></datafile>"#;
    let dat = parse_dat_with(xml.as_bytes(), platform(HeaderRule::None, "gb", &["gb"])).unwrap();
    assert_eq!(dat.games[0].roms.len(), 1);
    assert_eq!(dat.games[0].roms[0].crc32.as_deref(), Some("00000002"));
    assert_eq!(dat.games[0].roms[0].status, RomStatus::Good);
}

#[test]
fn an_odd_extra_never_rejects_the_export() {
    let xml = r#"<header/><datafile><game name="Example Quest (World)"><archive number="1" clone="P"/>
      <source><file extension="gb" size="8" crc32="00000001"/>
      <file extension="sav" size="" crc32="zz" item="Save"/>
      <file item="Box"/></source></game></datafile>"#;
    let dat = parse_dat_with(xml.as_bytes(), platform(HeaderRule::None, "gb", &["gb"])).unwrap();
    assert_eq!(dat.games[0].roms.len(), 1);
    let bad = xml.replace(r#"crc32="00000001""#, r#"crc32="zz""#);
    assert!(parse_dat_with(bad.as_bytes(), ExportOptions::default()).is_err());
}

#[test]
fn unknown_xml_names_the_supported_formats() {
    let e = parse_dat(b"<softwarelist><game name='x'/></softwarelist>").unwrap_err();
    let text = e.to_string();
    assert!(text.contains("<softwarelist>"), "{text}");
    assert!(
        text.contains("Logiqx") && text.contains("DB export"),
        "{text}"
    );
    match parse_dat(b"<header/><softwarelist/>").unwrap_err() {
        DatError::NotDatafile { root } => assert_eq!(root, "softwarelist"),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        parse_dat(b"<header><version>1</version></header>").unwrap_err(),
        DatError::Truncated
    ));
    assert!(matches!(
        parse_dat(b"<header/><datafile/>").unwrap_err(),
        DatError::NoGames
    ));
}

#[test]
fn bad_file_attributes_reject_the_export() {
    let bad = |file: &str| {
        let xml = format!(
            "<header/><datafile><game name=\"G\"><source>{file}</source></game></datafile>"
        );
        parse_dat(xml.as_bytes()).unwrap_err()
    };
    assert!(matches!(
        bad("<file extension=\"nes\"/>"),
        DatError::MissingAttribute {
            element: "file",
            attribute: "size",
            ..
        }
    ));
    assert!(matches!(
        bad("<file size=\"x\"/>"),
        DatError::InvalidAttribute {
            attribute: "size",
            ..
        }
    ));
    assert!(matches!(
        bad("<file size=\"1\" crc32=\"xyz\"/>"),
        DatError::InvalidAttribute {
            attribute: "crc32",
            ..
        }
    ));
}

#[test]
fn export_names_are_read_from_file_names() {
    let n =
        export_name("Example Vendor - Example System (DB Export) (20260101-000000).zip").unwrap();
    assert_eq!(n.version, "20260101-000000");
    let n = export_name("Example Vendor - Example System (db export)").unwrap();
    assert_eq!(n.dat_name, "Example Vendor - Example System (DB Export)");
    assert_eq!(n.version, "");
    let n = export_name("Example System (DB Export) (2) (1).xml").unwrap();
    assert_eq!(n.version, "2");
    assert!(export_name("(DB Export) (1).xml").is_none());
    assert!(export_name("").is_none());
}

#[test]
fn a_stream_reads_an_export_from_a_buffered_reader() {
    let xml = export_xml(&fixture());
    let parents = export_parents(BufReader::new(xml.as_bytes()))
        .unwrap()
        .unwrap();
    let options = ExportOptions {
        parents,
        ..ines_options()
    };
    let stream = DatStream::with_options(BufReader::new(xml.as_bytes()), options).unwrap();
    let games: Vec<DatGame> = stream.collect::<Result<_, _>>().unwrap();
    assert_eq!(games[0].clone_of.as_deref(), Some("Example Quest (Japan)"));
}

const REGIONS: [&str; 4] = ["USA", "Europe", "Japan", "World"];
const LANGUAGES: [&str; 4] = ["En", "Fr", "De", "Ja"];

prop_compose! {
    fn arb_dump()(size in 1u64..1 << 20, seed: u64, prg in 1u8..8) -> Dump {
        Dump { size, seed, header: ines_header(prg) }
    }
}

prop_compose! {
    fn arb_games()(count in 1usize..8)(
        parents in prop::collection::vec(proptest::option::of(0usize..8), count),
        regions in prop::collection::vec(proptest::sample::subsequence(REGIONS.to_vec(), 0..3), count),
        languages in prop::collection::vec(proptest::sample::subsequence(LANGUAGES.to_vec(), 0..3), count),
        dumps in prop::collection::vec(proptest::option::of(arb_dump()), count),
        sources in prop::collection::vec(1usize..4, count),
        bad in prop::collection::vec(any::<bool>(), count),
        mia in prop::collection::vec(proptest::bool::weighted(0.2), count),
        forced in prop::collection::vec(any::<bool>(), count),
    ) -> Vec<Game> {
        (0..parents.len())
            .map(|i| Game {
                name: format!("Example Game {i:03} (Test)"),
                // Only a game listed as a parent keeps parents, so no clone chains form.
                parent: parents[i].filter(|&p| p < parents.len() && p != i && parents[p].is_none()),
                regions: regions[i].iter().map(|r| (*r).to_owned()).collect(),
                languages: languages[i].iter().map(|l| (*l).to_owned()).collect(),
                dump: dumps[i].clone(),
                sources: sources[i],
                bad: bad[i],
                mia: mia[i],
                forcename: forced[i].then(|| format!("Example Game {i:03} (Forced).nes")),
                status: None,
            })
            .collect()
    }
}

proptest! {
    #[test]
    fn export_and_logiqx_forms_give_the_same_games(games in arb_games()) {
        let export = parse_dat_with(export_xml(&games).as_bytes(), ines_options()).unwrap();
        let logiqx = parse_dat(logiqx_xml(&games).as_bytes()).unwrap();
        prop_assert_eq!(&export.games, &logiqx.games);
        prop_assert_eq!(export.header.version, logiqx.header.version);
    }

    #[test]
    fn export_parsing_never_panics(s in "[<>/=\"' a-z!?&;#0-9-]{0,200}") {
        let _ = parse_dat(format!("<header/><datafile>{s}</datafile>").as_bytes());
        let _ = parse_dat(format!("<header/><datafile><game name='g'><source>{s}</source></game></datafile>").as_bytes());
        let _ = export_parents(format!("<header/>{s}").as_bytes());
        let _ = export_name(&s);
    }
}
