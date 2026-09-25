use std::fmt::Write as _;
use std::io::{BufReader, Cursor, Write};

use proptest::prelude::*;
use zip::write::SimpleFileOptions;
use zip::CompressionMethod;

use super::*;

const MD5: &str = "0123456789abcdef0123456789abcdef";
const SHA1: &str = "0123456789abcdef0123456789abcdef01234567";

fn full_dat() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE datafile PUBLIC "-//Logiqx//DTD ROM Management Datafile//EN" "datafile.dtd">
<datafile>
  <!-- synthetic -->
  <header>
    <name>Example Vendor - Example System</name>
    <description>Example Vendor - Example System (Parent-Clone)</description>
    <version>20240101-000000</version>
    <date>2024-01-01</date>
    <author>tester</author>
    <homepage>none</homepage>
    <url>none</url>
    <comment>made up &amp; synthetic</comment>
    <clrmamepro header="example-header.xml" forcemerging="none"/>
  </header>
  <game name="Example Quest (USA)" id="0001">
    <description>Example Quest (USA)</description>
    <category>Games</category>
    <category>Extra</category>
    <release name="Example Quest (USA)" region="USA"/>
    <rom name="Example Quest (USA).bin" size="1024" crc="0A0B0C0D" md5="{md5}" sha1="{sha1}"/>
  </game>
  <game name="Example Quest (Europe) (Rev 1)" cloneof="Example Quest (USA)" romof="Example Quest (USA)">
    <description>Example Quest &amp; Friends (Europe) &#233;</description>
    <rom name="Example Quest (Europe) (Rev 1).bin" size="2048" crc="deadbeef" status="baddump" header="4E45531A"></rom>
  </game>
  <machine name="Example Saga (USA)">
    <description><![CDATA[Example <Saga>]]></description>
    <rom name="Example Saga (USA) (Track 1).bin" size="0" status="nodump"/>
    <rom name="Example Saga (USA) (Track 2).bin" size="10" crc="" md5="" status="verified"/>
    <disk name="ignored"/>
  </machine>
  <game name="Example Empty (USA)"/>
</datafile>
"#,
        md5 = MD5.to_ascii_uppercase(),
        sha1 = SHA1
    )
}

#[test]
fn parses_full_dat() {
    let dat = parse_dat(full_dat().as_bytes()).unwrap();
    let h = &dat.header;
    assert_eq!(h.name, "Example Vendor - Example System");
    assert_eq!(
        h.description,
        "Example Vendor - Example System (Parent-Clone)"
    );
    assert_eq!(h.version, "20240101-000000");
    assert_eq!(h.date.as_deref(), Some("2024-01-01"));
    assert_eq!(h.author.as_deref(), Some("tester"));
    assert_eq!(h.homepage.as_deref(), Some("none"));
    assert_eq!(h.url.as_deref(), Some("none"));
    assert_eq!(h.comment.as_deref(), Some("made up & synthetic"));
    assert_eq!(h.clrmamepro_header.as_deref(), Some("example-header.xml"));
    assert_eq!(dat.games.len(), 4);

    let g = &dat.games[0];
    assert_eq!(g.name, "Example Quest (USA)");
    assert_eq!(g.clone_of, None);
    assert_eq!(g.rom_of, None);
    assert_eq!(g.description.as_deref(), Some("Example Quest (USA)"));
    assert_eq!(g.category.as_deref(), Some("Games"));
    assert_eq!(
        g.roms,
        vec![DatRom {
            name: "Example Quest (USA).bin".into(),
            size: 1024,
            crc32: Some("0a0b0c0d".into()),
            md5: Some(MD5.into()),
            sha1: Some(SHA1.into()),
            status: RomStatus::Good,
            header: None,
        }]
    );

    let g = &dat.games[1];
    assert_eq!(g.clone_of.as_deref(), Some("Example Quest (USA)"));
    assert_eq!(g.rom_of.as_deref(), Some("Example Quest (USA)"));
    assert_eq!(
        g.description.as_deref(),
        Some("Example Quest & Friends (Europe) é")
    );
    assert_eq!(g.category, None);
    assert_eq!(g.roms[0].status, RomStatus::BadDump);
    assert_eq!(g.roms[0].header.as_deref(), Some("4E45531A"));
    assert_eq!(g.roms[0].crc32.as_deref(), Some("deadbeef"));
    assert_eq!(g.roms[0].md5, None);

    let g = &dat.games[2];
    assert_eq!(g.name, "Example Saga (USA)");
    assert_eq!(g.description.as_deref(), Some("Example <Saga>"));
    assert_eq!(g.roms.len(), 2);
    assert_eq!(g.roms[0].status, RomStatus::NoDump);
    assert_eq!(g.roms[0].crc32, None);
    assert_eq!(g.roms[1].status, RomStatus::Verified);
    assert_eq!(g.roms[1].crc32, None);
    assert_eq!(g.roms[1].md5, None);

    assert!(dat.games[3].roms.is_empty());
}

#[test]
fn reader_and_slice_agree() {
    let xml = full_dat();
    let a = parse_dat(xml.as_bytes()).unwrap();
    let b = parse_dat_reader(BufReader::with_capacity(16, xml.as_bytes())).unwrap();
    assert_eq!(a, b);
}

#[test]
fn accepts_bom_and_minimal_document() {
    let xml = "\u{feff}<datafile><game name=\"Example Quest (USA)\"/></datafile>";
    let dat = parse_dat(xml.as_bytes()).unwrap();
    assert_eq!(dat.header, DatHeader::default());
    assert_eq!(dat.games[0].name, "Example Quest (USA)");
}

#[test]
fn attribute_entities_are_resolved() {
    let xml = r#"<datafile><game name="Example &amp; Quest &quot;X&quot; (USA)">
        <rom name="a&#47;b.bin" size="1"/></game></datafile>"#;
    let dat = parse_dat(xml.as_bytes()).unwrap();
    assert_eq!(dat.games[0].name, "Example & Quest \"X\" (USA)");
    assert_eq!(dat.games[0].roms[0].name, "a/b.bin");
}

#[test]
fn unknown_text_entity_is_kept() {
    let xml = r"<datafile><header><name>A &custom; B</name></header><game name='x'/></datafile>";
    let dat = parse_dat(xml.as_bytes()).unwrap();
    assert_eq!(dat.header.name, "A &custom; B");
}

#[test]
fn header_may_follow_leading_elements() {
    let xml = r"<datafile><unknown><deep>x</deep></unknown><header><name>N</name><version>1</version></header><game name='x'/></datafile>";
    let dat = parse_dat(xml.as_bytes()).unwrap();
    assert_eq!(dat.header.name, "N");
    assert_eq!(dat.header.version, "1");
}

#[test]
fn clrmamepro_with_body() {
    let xml = r"<datafile><header><clrmamepro header='h.xml'>text</clrmamepro></header><game name='x'/></datafile>";
    let dat = parse_dat(xml.as_bytes()).unwrap();
    assert_eq!(dat.header.clrmamepro_header.as_deref(), Some("h.xml"));
}

#[test]
fn bom_is_skipped_on_the_reader_path() {
    let xml = "\u{feff}<?xml version='1.0' encoding='UTF-8'?><datafile><game name='Caf\u{e9}'/></datafile>";
    let dat = parse_dat_reader(BufReader::with_capacity(2, xml.as_bytes())).unwrap();
    assert_eq!(dat.games[0].name, "Caf\u{e9}");
}

#[test]
fn non_utf8_name_is_rejected_with_its_input_offset() {
    let xml = b"<datafile>\n<game name='Example Caf\xe9 (Europe)'/></datafile>";
    for dat in [
        parse_dat(xml),
        parse_dat_reader(BufReader::with_capacity(3, &xml[..])),
    ] {
        match dat {
            Err(DatError::Xml { position, .. }) => assert_eq!(position, 47),
            other => panic!("{other:?}"),
        }
    }
    let text = b"<datafile><header><name>Caf\xe9</name></header><game name='x'/></datafile>";
    assert!(matches!(parse_dat(text), Err(DatError::Xml { .. })));
    let cdata = b"<datafile><header><name><![CDATA[\xe9]]></name></header></datafile>";
    assert!(matches!(parse_dat(cdata), Err(DatError::Xml { .. })));
}

#[test]
fn non_utf8_bytes_the_parser_ignores_never_fail() {
    let xml = b"<?xml version='1.0' encoding='ISO-8859-1'?>\n<!-- Caf\xe9 -->\
        <datafile><?pi \xe9?><header><name>N</name></header><unknown>\xe9</unknown>\
        <game name='Example Quest (USA)' note='\xe9'><extra>\xe9</extra>\
        <rom name='a.bin' size='1' x\xe9='1'/></game></datafile>";
    for dat in [
        parse_dat(xml).unwrap(),
        parse_dat_reader(BufReader::with_capacity(1, &xml[..])).unwrap(),
    ] {
        assert_eq!(dat.header.name, "N");
        assert_eq!(dat.games[0].name, "Example Quest (USA)");
        assert_eq!(dat.games[0].roms[0].name, "a.bin");
    }
}

fn err(xml: &str) -> DatError {
    parse_dat(xml.as_bytes()).unwrap_err()
}

#[test]
fn rejects_wrong_root() {
    match err("<softwarelist><game name='x'/></softwarelist>") {
        DatError::NotDatafile { root } => assert_eq!(root, "softwarelist"),
        other => panic!("{other:?}"),
    }
    assert!(matches!(err("<other/>"), DatError::NotDatafile { .. }));
}

#[test]
fn rejects_empty_dats() {
    assert!(matches!(err("<datafile/>"), DatError::NoGames));
    assert!(matches!(err("<datafile></datafile>"), DatError::NoGames));
    assert!(matches!(
        err("<datafile><header><name>N</name></header></datafile>"),
        DatError::NoGames
    ));
}

#[test]
fn rejects_truncated_and_malformed() {
    assert!(matches!(err(""), DatError::Truncated));
    assert!(matches!(err("<?xml version='1.0'?>"), DatError::Truncated));
    assert!(matches!(
        err("<datafile><game name='x'>"),
        DatError::Truncated
    ));
    assert!(matches!(
        err("<datafile><game name='x'/>"),
        DatError::Truncated
    ));
    assert!(matches!(
        err("<datafile><header><name>N"),
        DatError::Truncated
    ));
    assert!(matches!(
        err("<datafile><game name='x'></datafile>"),
        DatError::Xml { .. }
    ));
    assert!(matches!(
        err("<datafile><game name='x' name='y'/></datafile>"),
        DatError::Xml { .. }
    ));
    assert!(matches!(
        parse_dat(b"<datafile><game name='\xff'/></datafile>"),
        Err(DatError::Xml { .. })
    ));
}

#[test]
fn rejects_bad_attributes() {
    match err("<datafile><game><rom name='a' size='1'/></game></datafile>") {
        DatError::MissingAttribute {
            element, attribute, ..
        } => assert_eq!((element, attribute), ("game", "name")),
        other => panic!("{other:?}"),
    }
    match err("<datafile><game name='g'><rom name='a'/></game></datafile>") {
        DatError::MissingAttribute {
            element,
            attribute,
            game,
        } => assert_eq!((element, attribute, game.as_str()), ("rom", "size", "g")),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        err("<datafile><game name='g'><rom size='1'/></game></datafile>"),
        DatError::MissingAttribute {
            attribute: "name",
            ..
        }
    ));
    for (attr, value) in [
        ("size", "-1"),
        ("size", "1k"),
        ("crc", "0a0b0c"),
        ("crc", "0a0b0c0g"),
        ("md5", "00"),
        ("sha1", MD5),
        ("status", "great"),
    ] {
        let size = if attr == "size" { "" } else { "size='1'" };
        let xml = format!(
            "<datafile><game name='g'><rom name='a' {size} {attr}='{value}'/></game></datafile>"
        );
        match err(&xml) {
            DatError::InvalidAttribute {
                game,
                attribute,
                value: got,
            } => {
                assert_eq!(game, "g");
                assert_eq!(attribute, attr);
                assert_eq!(got, value);
            }
            other => panic!("{attr}={value}: {other:?}"),
        }
    }
}

#[test]
fn status_and_hash_whitespace_is_tolerated() {
    let xml = format!(
        "<datafile><game name='g'><rom name='a' size=' 7 ' crc=' 0A0B0C0D ' sha1='{}' status='BadDump'/></game></datafile>",
        SHA1.to_ascii_uppercase()
    );
    let rom = &parse_dat(xml.as_bytes()).unwrap().games[0].roms[0];
    assert_eq!(rom.size, 7);
    assert_eq!(rom.crc32.as_deref(), Some("0a0b0c0d"));
    assert_eq!(rom.sha1.as_deref(), Some(SHA1));
    assert_eq!(rom.status, RomStatus::BadDump);
}

#[test]
fn rom_status_strings() {
    for status in [
        RomStatus::Good,
        RomStatus::BadDump,
        RomStatus::NoDump,
        RomStatus::Verified,
    ] {
        assert_eq!(RomStatus::parse(status.as_str()), Some(status));
    }
    assert_eq!(RomStatus::default(), RomStatus::Good);
    assert_eq!(RomStatus::parse(""), None);
}

/// A datfile shaped like a No-Intro export: schema attributes on the root, an
/// `<id>`, long legal elements in the header, and `id`/`cloneofid` on games.
fn no_intro_shaped(games: usize) -> String {
    let legal = "Synthetic notice text for the fixture only. ".repeat(80);
    let mut xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE datafile PUBLIC "-//Logiqx//DTD ROM Management Datafile//EN" "https://example.invalid/datafile.dtd">
<datafile xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xsi:schemaLocation="https://example.invalid/schema https://example.invalid/schema/datfile_v3.xsd">
	<header>
		<id>49</id>
		<name>Nintendo - Super Nintendo Entertainment System</name>
		<description>Nintendo - Super Nintendo Entertainment System</description>
		<version>20260101-000000</version>
		<author>fixture, tester &amp; others</author>
		<homepage>Example group</homepage>
		<url>https://example.invalid/</url>
		<trademarks>{legal}</trademarks>
		<piracy>{legal}</piracy>
		<clrmamepro forcenodump="required"/>
	</header>
"#
    );
    for i in 0..games {
        let clone = if i % 2 == 1 {
            format!(" cloneofid=\"{:04}\"", i - 1)
        } else {
            String::new()
        };
        write!(
            xml,
            "\t<game name=\"Example Title {i} (USA)\" id=\"{i:04}\"{clone}>\n\t\t<description>Example Title {i} (USA)</description>\n\t\t<rom name=\"Example Title {i} (USA).sfc\" size=\"{size}\" crc=\"{i:08x}\" md5=\"{MD5}\" sha1=\"{SHA1}\" sha256=\"{MD5}{MD5}\" status=\"verified\" serial=\"SYN-{i:04}\"/>\n\t</game>\n",
            size = 1024 + i,
        )
        .unwrap();
    }
    xml.push_str("</datafile>\n");
    xml
}

#[test]
fn no_intro_shaped_header_and_games_parse() {
    let xml = no_intro_shaped(50);
    let stream = DatStream::new(BufReader::new(xml.as_bytes())).unwrap();
    let h = stream.header().clone();
    assert_eq!(h.name, "Nintendo - Super Nintendo Entertainment System");
    assert_eq!(h.description, h.name);
    assert_eq!(h.version, "20260101-000000");
    assert_eq!(h.author.as_deref(), Some("fixture, tester & others"));
    assert_eq!(h.homepage.as_deref(), Some("Example group"));
    assert_eq!(h.url.as_deref(), Some("https://example.invalid/"));
    let games: Vec<DatGame> = stream.map(Result::unwrap).collect();
    assert_eq!(games.len(), 50);
    assert_eq!(games[1].name, "Example Title 1 (USA)");
    assert_eq!(games[1].roms[0].status, RomStatus::Verified);
    assert_eq!(games[1].roms[0].size, 1025);
    let zipped = zip_of(&[(
        "Nintendo - Super Nintendo Entertainment System (20260101-000000).dat",
        xml.as_bytes(),
        CompressionMethod::Deflated,
    )]);
    let mut pack = parse_dat_pack(Cursor::new(zipped)).unwrap();
    let member = pack.next().unwrap().unwrap();
    assert_eq!(member.dat.header, h);
    assert_eq!(member.dat.games.len(), 50);
}

fn synthetic_dat(games: usize) -> String {
    let mut xml = String::from("<datafile><header><name>Example System</name></header>");
    for i in 0..games {
        write!(
            xml,
            "<game name=\"Example Title {i} (USA)\"><rom name=\"t{i}.bin\" size=\"{i}\" crc=\"{i:08x}\"/></game>"
        )
        .unwrap();
    }
    xml.push_str("</datafile>");
    xml
}

#[test]
fn stream_yields_games_in_order() {
    let xml = synthetic_dat(5000);
    let mut stream = DatStream::new(BufReader::new(xml.as_bytes())).unwrap();
    assert_eq!(stream.header().name, "Example System");
    let mut n = 0u64;
    for game in &mut stream {
        let game = game.unwrap();
        assert_eq!(game.roms[0].size, n);
        assert_eq!(game.roms[0].crc32, Some(format!("{n:08x}")));
        n += 1;
    }
    assert_eq!(n, 5000);
    assert!(stream.next().is_none());
}

#[test]
fn stream_reports_no_games_once() {
    let mut stream = DatStream::new("<datafile/>".as_bytes()).unwrap();
    assert!(matches!(stream.next(), Some(Err(DatError::NoGames))));
    assert!(stream.next().is_none());
}

#[test]
fn stream_stops_after_error() {
    let xml = "<datafile><game name='a'/><game name='b'><rom name='r'/></game><game name='c'/></datafile>";
    let mut stream = DatStream::new(xml.as_bytes()).unwrap();
    assert_eq!(stream.next().unwrap().unwrap().name, "a");
    assert!(matches!(
        stream.next(),
        Some(Err(DatError::MissingAttribute { .. }))
    ));
    assert!(stream.next().is_none());
}

#[test]
fn stream_new_rejects_bad_root() {
    assert!(matches!(
        DatStream::new("<other/>".as_bytes()),
        Err(DatError::NotDatafile { .. })
    ));
}

fn zip_of(members: &[(&str, &[u8], CompressionMethod)]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, body, method) in members {
        if name.ends_with('/') {
            zip.add_directory(*name, SimpleFileOptions::default())
                .unwrap();
            continue;
        }
        zip.start_file(
            *name,
            SimpleFileOptions::default().compression_method(*method),
        )
        .unwrap();
        zip.write_all(body).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

fn small_dat(system: &str) -> String {
    format!("<datafile><header><name>{system}</name></header><game name=\"Example Quest (USA)\"/></datafile>")
}

#[test]
fn pack_yields_each_dat_member() {
    let a = small_dat("Example System A");
    let b = small_dat("Example System B");
    let c = small_dat("Example System C");
    let bytes = zip_of(&[
        ("readme.txt", b"not a dat", CompressionMethod::Deflated),
        ("A.dat", a.as_bytes(), CompressionMethod::Deflated),
        ("dats/", b"", CompressionMethod::Stored),
        ("dats/B.XML", b.as_bytes(), CompressionMethod::Stored),
        ("dats/C.Dat", c.as_bytes(), CompressionMethod::Deflated),
    ]);
    let pack = parse_dat_pack(Cursor::new(bytes)).unwrap();
    assert_eq!(pack.len(), 3);
    assert!(!pack.is_empty());
    let members: Vec<PackMember> = pack.collect::<Result<_, _>>().unwrap();
    let names: Vec<(&str, &str)> = members
        .iter()
        .map(|m| (m.file_name.as_str(), m.dat.header.name.as_str()))
        .collect();
    assert_eq!(
        names,
        [
            ("A.dat", "Example System A"),
            ("dats/B.XML", "Example System B"),
            ("dats/C.Dat", "Example System C"),
        ]
    );
}

#[test]
fn pack_member_errors_name_the_member() {
    let good = small_dat("Example System");
    let bytes = zip_of(&[
        ("bad.dat", b"<other/>", CompressionMethod::Deflated),
        ("good.dat", good.as_bytes(), CompressionMethod::Deflated),
    ]);
    let mut pack = parse_dat_pack(Cursor::new(bytes)).unwrap();
    match pack.next() {
        Some(Err(DatError::Member { member, source })) => {
            assert_eq!(member, "bad.dat");
            assert!(matches!(*source, DatError::NotDatafile { .. }));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(pack.next().unwrap().unwrap().file_name, "good.dat");
    assert!(pack.next().is_none());
}

#[test]
fn pack_rejects_non_zip_and_empty_packs() {
    assert!(matches!(
        parse_dat_pack(Cursor::new(b"<datafile/>".to_vec())),
        Err(DatError::Zip(_))
    ));
    let bytes = zip_of(&[("notes.txt", b"x", CompressionMethod::Stored)]);
    assert!(matches!(
        parse_dat_pack(Cursor::new(bytes)),
        Err(DatError::EmptyPack)
    ));
}

#[test]
fn errors_display() {
    let e = DatError::Member {
        member: "a.dat".into(),
        source: Box::new(DatError::NoGames),
    };
    assert_eq!(e.to_string(), "a.dat: DAT contains no games");
    assert_eq!(
        DatError::NotDatafile { root: "x".into() }.to_string(),
        "root element is <x>; expected a Logiqx DAT (<datafile>) \
         or a No-Intro DB export (<header> followed by <datafile>)"
    );
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

prop_compose! {
    fn arb_rom()(name in "[ -~]{1,20}", size: u64, crc: Option<u32>) -> DatRom {
        DatRom {
            name: name.trim().to_owned(),
            size,
            crc32: crc.map(|c| format!("{c:08x}")),
            md5: None,
            sha1: None,
            status: RomStatus::Good,
            header: None,
        }
    }
}

prop_compose! {
    fn arb_game()(name in "[ -~]{1,24}", description in proptest::option::of("[ -~]{1,24}"),
                  roms in prop::collection::vec(arb_rom(), 0..3)) -> DatGame {
        DatGame {
            name,
            clone_of: None,
            rom_of: None,
            description: description.map(|d| d.trim().to_owned()),
            category: None,
            regions: Vec::new(),
            languages: Vec::new(),
            status: None,
            roms,
        }
    }
}

fn render(games: &[DatGame]) -> String {
    let mut xml = String::from("<datafile>");
    for g in games {
        write!(xml, "<game name=\"{}\">", xml_escape(&g.name)).unwrap();
        if let Some(d) = &g.description {
            write!(xml, "<description>{}</description>", xml_escape(d)).unwrap();
        }
        for r in &g.roms {
            write!(
                xml,
                "<rom name=\"{}\" size=\"{}\"",
                xml_escape(&r.name),
                r.size
            )
            .unwrap();
            if let Some(c) = &r.crc32 {
                write!(xml, " crc=\"{}\"", c.to_ascii_uppercase()).unwrap();
            }
            xml.push_str("/>");
        }
        xml.push_str("</game>");
    }
    xml.push_str("</datafile>");
    xml
}

proptest! {
    #[test]
    fn parse_never_panics_on_bytes(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
        let _ = parse_dat(&bytes);
    }

    #[test]
    fn parse_never_panics_on_xmlish(s in "[<>/=\"' a-z!?\\[\\]&;#0-9-]{0,200}") {
        let _ = parse_dat(s.as_bytes());
        let _ = parse_dat(format!("<datafile>{s}</datafile>").as_bytes());
        let _ = parse_dat(format!("<datafile><game name='g'>{s}</game></datafile>").as_bytes());
    }

    #[test]
    fn pack_never_panics_on_bytes(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
        if let Ok(pack) = parse_dat_pack(Cursor::new(bytes)) {
            for member in pack {
                let _ = member;
            }
        }
    }

    #[test]
    fn rendered_games_round_trip(games in prop::collection::vec(arb_game(), 1..8)) {
        let dat = parse_dat(render(&games).as_bytes()).unwrap();
        prop_assert_eq!(dat.games, games);
    }
}

#[test]
fn releases_give_regions_and_languages_once_each() {
    let dat = parse_dat(full_dat().as_bytes()).unwrap();
    assert_eq!(dat.games[0].regions, ["USA"]);
    assert!(dat.games[0].languages.is_empty());
    let xml = r#"<datafile><game name="Example Quest (Europe)">
        <release name="a" region="Europe" language="En,Fr"/><release name="b" region="Europe" language="Fr, De"/>
        </game></datafile>"#;
    let game = &parse_dat(xml.as_bytes()).unwrap().games[0];
    assert_eq!(game.regions, ["Europe"]);
    assert_eq!(game.languages, ["En", "Fr", "De"]);
}

const SMALL: &str = r#"<datafile><game name="Example Quest (World)"><rom name="q.bin" size="1"/></game></datafile>"#;

fn trailing(bytes: &[u8]) -> Result<Dat, DatError> {
    let mut all = SMALL.as_bytes().to_vec();
    all.extend_from_slice(bytes);
    parse_dat_reader(BufReader::new(Cursor::new(all)))
}

#[test]
fn only_whitespace_comments_and_instructions_may_follow_the_root() {
    for ok in [&b""[..], b"\n\r\n  ", b"<!-- end -->\n", b"<?pi data?>"] {
        assert!(trailing(ok).is_ok(), "{ok:?}");
    }
    for bad in [
        &b"x"[..],
        b"<game name='g'/>",
        b"</datafile>",
        b"\n\x7fELF\x01\x02",
        b"NES\x1a\x00\xff",
    ] {
        let e = trailing(bad).expect_err("trailing data");
        assert!(
            matches!(e, DatError::TrailingData { .. } | DatError::Xml { .. }),
            "{bad:?}: {e}"
        );
    }
    let e = trailing(b"PK\x03\x04rom bytes").expect_err("an appended file");
    assert!(
        matches!(e, DatError::TrailingData { position } if position >= SMALL.len() as u64),
        "{e}"
    );
    let e = parse_dat(b"<datafile/>junk").expect_err("after an empty root");
    assert!(matches!(e, DatError::TrailingData { .. }), "{e}");
}

#[test]
fn a_pack_member_with_trailing_data_fails() {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file("a.dat", SimpleFileOptions::default())
        .unwrap();
    zip.write_all(format!("{SMALL}appended").as_bytes())
        .unwrap();
    let bytes = zip.finish().unwrap().into_inner();
    let member = parse_dat_pack(Cursor::new(bytes)).unwrap().next().unwrap();
    let e = member.expect_err("trailing data");
    assert!(
        matches!(e, DatError::Member { ref source, .. } if matches!(**source, DatError::TrailingData { .. })),
        "{e}"
    );
}

#[test]
fn an_oversized_event_fails_before_it_is_buffered() {
    let big = "a".repeat(usize::try_from(MAX_EVENT_BYTES).unwrap() + 10);
    let cases = [
        format!(r#"<datafile><game name="{big}"/></datafile>"#),
        format!("<datafile><game name='g'><description>{big}</description></game></datafile>"),
        format!("<datafile><game name='g'><video><x>{big}</x></video></game></datafile>"),
        format!(
            "<datafile><game name='g'/></datafile>{}",
            " ".repeat(big.len())
        ),
    ];
    for xml in &cases {
        let e = parse_dat_reader(BufReader::new(xml.as_bytes())).expect_err("too large");
        assert!(matches!(e, DatError::EventTooLarge { .. }), "{e}");
    }
    let near = "a".repeat(usize::try_from(MAX_EVENT_BYTES).unwrap() - 64);
    let xml = format!(r#"<datafile><game name="{near}"/></datafile>"#);
    assert!(parse_dat_reader(BufReader::new(xml.as_bytes())).is_ok());
}

proptest! {
    #[test]
    fn appended_bytes_are_refused_unless_blank(tail in prop::collection::vec(any::<u8>(), 1..64)) {
        let parsed = trailing(&tail);
        if tail.iter().all(u8::is_ascii_whitespace) {
            prop_assert!(parsed.is_ok());
        } else if !tail.starts_with(b"<!") && !tail.starts_with(b"<?") {
            prop_assert!(parsed.is_err(), "{:?} accepted", tail);
        }
    }
}
