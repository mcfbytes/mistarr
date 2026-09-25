use std::fmt::Write as _;
use std::io::BufReader;

use proptest::prelude::*;

use super::*;
use crate::dat::{collect, export_parents, parse_dat};
use crate::hash::HeaderRule;

fn rewritten(xml: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    rewrite(BufReader::new(xml), &mut out).expect("rewrite");
    out
}

/// The header as the importer sees it when the stream opens, and the whole parse.
fn seen(xml: &[u8], options: &ExportOptions) -> (DatHeader, crate::dat::Dat) {
    let opened = DatStream::with_options(xml, options.clone()).expect("open");
    let at_open = opened.header().clone();
    let mut options = options.clone();
    if let Some(parents) = export_parents(xml).expect("index") {
        options.parents = parents;
    }
    let dat = collect(DatStream::with_options(xml, options).expect("open")).expect("parse");
    (at_open, dat)
}

const NOISY: &str = r#"<?xml version="1.0"?>
<!DOCTYPE datafile PUBLIC "-//Logiqx//DTD ROM Management Datafile//EN" "x.dtd">
<!-- SECRET-COMMENT -->
<?SECRET-PI data?>
<datafile>
  <header><name>Example System</name><version>1</version>
    <clrmamepro header="example.xml"><SECRET-NESTED>x</SECRET-NESTED></clrmamepro>
    <SECRET-HEADER-ELEMENT>payload</SECRET-HEADER-ELEMENT></header>
  SECRET-STRAY-TEXT
  <SECRET-UNKNOWN><![CDATA[SECRET-CDATA]]><inner a="SECRET-ATTR"/></SECRET-UNKNOWN>
  <game name="Example Quest (World)" cloneof="Example Quest (Japan)" SECRET-EXTRA="1">
    <description>Example <![CDATA[Quest]]></description>
    <release name="Example Quest" region="USA" language="En"/>
    <video screen="SECRET-VIDEO"/>
    <rom name="q.bin" size="4" crc="0A0B0C0D" SECRET-ROM-ATTR="x"><SECRET-IN-ROM/></rom>
    <!-- SECRET-GAME-COMMENT -->
  </game>
  <game name="Example Quest (Japan)"><rom name="q.bin" size="4" sha1="0123456789abcdef0123456789abcdef01234567"/></game>
</datafile>
<!-- SECRET-TRAILER -->
"#;

#[test]
fn nothing_the_parser_does_not_read_is_written() {
    let out = rewritten(NOISY.as_bytes());
    let text = String::from_utf8(out.clone()).expect("utf-8");
    assert!(!text.contains("SECRET"), "{text}");
    assert!(!text.contains("<!") && !text.contains("<?SECRET"), "{text}");
    assert_eq!(
        parse_dat(&out).expect("parse"),
        parse_dat(NOISY.as_bytes()).expect("parse")
    );
    assert_eq!(
        parse_dat(&out).expect("parse").games[0]
            .description
            .as_deref(),
        Some("Example Quest")
    );
}

#[test]
fn a_rewrite_is_its_own_rewrite() {
    let once = rewritten(NOISY.as_bytes());
    assert_eq!(rewritten(&once), once);
}

const EXPORT: &str = r#"<header><name>Example Vendor - Example System</name><version>20260101</version></header>
<datafile>
  <game name="Example Quest (Japan)">
    <archive number="0001" clone="P" region="Japan" languages="Ja" status="Proto 2"/>
    <source>
      <file extension="nes" size="20" crc32="0a0b0c0d" format="Headered" header="4E 45 53 1A"/>
      <file extension="unh" size="4" crc32="01020304" format="Headerless"/>
      <file extension="sav" size="2" crc32="0f0f0f0f" item="Save"/>
    </source>
    <source>
      <file extension="nes" size="20" crc32="0a0b0c0e" format="Headered" header="4E 45 53 1A" bad="1"/>
    </source>
    <SECRET-EXPORT-ELEMENT/>
  </game>
  <game name="Example Quest (USA)">
    <archive number="0002" clone="0001" region="USA"/>
    <source><file extension="bin" size="8" sha1="0123456789abcdef0123456789abcdef01234567" forcename="Quest.bin"/></source>
    <source></source>
  </game>
</datafile>"#;

fn export_options() -> [ExportOptions; 3] {
    [
        ExportOptions::default(),
        ExportOptions {
            header_rule: HeaderRule::Ines,
            extension: Some("nes".into()),
            load_extensions: vec!["nes".into()],
            ..ExportOptions::default()
        },
        ExportOptions {
            extension: Some("bin".into()),
            ..ExportOptions::default()
        },
    ]
}

#[test]
fn a_db_export_is_rewritten_as_a_db_export_that_imports_alike() {
    let out = rewritten(EXPORT.as_bytes());
    let text = String::from_utf8(out.clone()).expect("utf-8");
    assert!(!text.contains("SECRET"));
    assert_eq!(
        DatStream::new(&out[..]).expect("open").format(),
        DatFormat::DbExport
    );
    assert_eq!(
        export_parents(&out[..]).expect("index"),
        export_parents(EXPORT.as_bytes()).expect("index")
    );
    for options in export_options() {
        assert_eq!(
            seen(&out, &options),
            seen(EXPORT.as_bytes(), &options),
            "{options:?}"
        );
    }
    assert_eq!(rewritten(&out), out);
}

#[test]
fn a_header_after_the_first_game_stays_after_it() {
    let xml = r#"<datafile><header><name>First</name><clrmamepro header="a.xml"/></header>
      <game name="A"><rom name="a" size="1"/></game>
      <header><name>Later</name><date>2026-01-01</date><clrmamepro/></header>
      <game name="B"><rom name="b" size="1"/></game></datafile>"#;
    let out = rewritten(xml.as_bytes());
    let options = ExportOptions::default();
    let (open, dat) = seen(&out, &options);
    assert_eq!(
        (open.name.as_str(), open.clrmamepro_header.as_deref()),
        ("First", Some("a.xml"))
    );
    assert_eq!(
        (dat.header.name.as_str(), dat.header.clrmamepro_header),
        ("Later", None)
    );
    assert_eq!(seen(&out, &options), seen(xml.as_bytes(), &options));
}

#[test]
fn what_the_parser_refuses_is_not_rewritten() {
    struct Full;
    impl Write for Full {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("full"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut out = Vec::new();
    let e = rewrite(&b"<html><body/></html>"[..], &mut out).expect_err("not a DAT");
    assert!(
        matches!(e, RewriteError::Dat(DatError::NotDatafile { .. })),
        "{e}"
    );
    let e = rewrite(&b"<datafile></datafile>"[..], &mut Vec::new()).expect_err("no games");
    assert!(matches!(e, RewriteError::Dat(DatError::NoGames)), "{e}");
    let e = rewrite(NOISY.as_bytes(), Full).expect_err("write");
    assert!(matches!(e, RewriteError::Write(_)), "{e}");
}

#[test]
fn a_tag_that_escaping_would_take_past_the_event_cap_is_refused() {
    let quotes = "\"".repeat(crate::dat::MAX_FIELD_BYTES);
    let xml = format!(
        "<header/><datafile><game name='g'><source><file size='1' extension='{quotes}' \
         format='{quotes}' header='{quotes}' item='{quotes}'/></source></game></datafile>"
    );
    assert!(
        crate::dat::parse_dat(xml.as_bytes()).is_ok(),
        "the input parses"
    );
    let e = rewrite(xml.as_bytes(), &mut Vec::new()).expect_err("too large once escaped");
    assert!(
        matches!(e, RewriteError::TagTooLarge { element: "file", ref game } if game == "g"),
        "{e}"
    );
}

#[test]
fn many_regions_are_split_across_releases_within_the_field_cap() {
    let mut xml = String::from("<datafile><game name='g'>");
    for i in 0..100 {
        let _ = write!(xml, "<release region='{i:03}{}'/>", "r".repeat(1000));
    }
    xml.push_str("<release language='En'/><rom name='a' size='1'/></game></datafile>");
    let out = rewritten(xml.as_bytes());
    let parsed = parse_dat(&out).expect("the rewrite parses");
    assert_eq!(parsed, parse_dat(xml.as_bytes()).expect("parse"));
    assert_eq!(parsed.games[0].regions.len(), 100);
    let text = String::from_utf8(out).expect("utf-8");
    assert_eq!(
        text.matches("<release").count(),
        3,
        "two of regions, one of languages"
    );
    let joined = joined_within(&["a".into(), "b".into(), "cc".into()], 3);
    assert_eq!(joined, ["a,b", "cc"]);
}

/// `s` escaped for an attribute or text, with references in a different style from the writer's.
fn esc(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '&' => "&#38;".to_owned(),
            '<' => "&#x3C;".to_owned(),
            '>' => "&gt;".to_owned(),
            '"' => "&#34;".to_owned(),
            '\'' => "&apos;".to_owned(),
            '\t' | '\n' | '\r' => format!("&#x{:X};", u32::from(c)),
            c => c.to_string(),
        })
        .collect()
}

const TEXT: &str = "[a-zA-Z0-9 ,&<>\"'\t\n\ré~]{0,10}";

prop_compose! {
    fn arb_rom()(name in TEXT, size: u64, crc in proptest::option::of("[0-9a-fA-F]{8}"),
                 status in 0usize..5, header in proptest::option::of(TEXT)) -> String {
        let status = ["", " status=\"baddump\"", " status=\"nodump\"", " status=\"verified\"", " status=\"good\""][status];
        let crc = crc.map(|c| format!(" crc=\"{c}\"")).unwrap_or_default();
        let header = header.map(|h| format!(" header=\"{}\"", esc(&h))).unwrap_or_default();
        format!("<rom name=\"{}\" size=\"{size}\"{crc}{status}{header}/>", esc(&name))
    }
}

prop_compose! {
    fn arb_game()(name in TEXT, clone in proptest::option::of(TEXT), description in proptest::option::of(TEXT),
                  categories in prop::collection::vec(TEXT, 0..3),
                  releases in prop::collection::vec((proptest::option::of(TEXT), proptest::option::of(TEXT)), 0..3),
                  roms in prop::collection::vec(arb_rom(), 0..4), noise in 0usize..4) -> String {
        let mut g = format!("<game name=\"{}\"", esc(&name));
        if let Some(c) = clone {
            let _ = write!(g, " cloneof=\"{}\"", esc(&c));
        }
        g.push('>');
        if let Some(d) = description {
            let _ = write!(g, "<description>{}</description>", esc(&d));
        }
        for c in categories {
            let _ = write!(g, "<category><![CDATA[{}]]></category>", c.replace("]]>", ""));
        }
        for (region, language) in releases {
            g.push_str("<release");
            if let Some(r) = region {
                let _ = write!(g, " region=\"{}\"", esc(&r));
            }
            if let Some(l) = language {
                let _ = write!(g, " language=\"{}\"", esc(&l));
            }
            g.push_str("/>");
        }
        g.push_str(["", "<!-- n -->", "<x a='1'>y<z/></x>", "stray"][noise]);
        g.extend(roms);
        g.push_str("</game>");
        g
    }
}

prop_compose! {
    fn arb_file()(ext in "[a-z]{0,3}", format in 0usize..4, size in 0u64..64,
                  crc in proptest::option::of("[0-9a-f]{8}"), header in proptest::option::of("[0-9A-F ]{0,8}"),
                  item in proptest::option::of(TEXT), force in proptest::option::of(TEXT), bad: bool) -> String {
        let format = ["", "Headered", "Headerless", "BigEndian"][format];
        let mut f = format!("<file extension=\"{ext}\" format=\"{format}\" size=\"{size}\"");
        if let Some(c) = crc { let _ = write!(f, " crc32=\"{c}\""); }
        if let Some(h) = header { let _ = write!(f, " header=\"{h}\""); }
        if let Some(i) = item { let _ = write!(f, " item=\"{}\"", esc(&i)); }
        if let Some(n) = force { let _ = write!(f, " forcename=\"{}\"", esc(&n)); }
        if bad { f.push_str(" bad=\"1\""); }
        f.push_str("/>");
        f
    }
}

prop_compose! {
    fn arb_export_game()(name in TEXT, number in proptest::option::of("[0-9]{1,3}"),
                         clone in proptest::option::of("P|[0-9]{1,3}"), region in proptest::option::of(TEXT),
                         sources in prop::collection::vec(prop::collection::vec(arb_file(), 0..3), 0..3)) -> String {
        let mut g = format!("<game name=\"{}\"><archive", esc(&name));
        if let Some(n) = number { let _ = write!(g, " number=\"{n}\""); }
        if let Some(c) = clone { let _ = write!(g, " clone=\"{c}\""); }
        if let Some(r) = region { let _ = write!(g, " region=\"{}\"", esc(&r)); }
        g.push_str("/>");
        for files in sources {
            g.push_str("<source>");
            g.extend(files);
            g.push_str("</source><!-- n -->");
        }
        g.push_str("</game>");
        g
    }
}

fn header_of(name: &str, date: Option<&str>) -> String {
    let date = date
        .map(|d| format!("<date>{}</date>", esc(d)))
        .unwrap_or_default();
    format!(
        "<header><name>{}</name>{date}<unknown>x</unknown></header>",
        esc(name)
    )
}

proptest! {
    #[test]
    fn parsing_the_rewrite_gives_what_parsing_the_input_gave(
        name in TEXT, date in proptest::option::of(TEXT), late in proptest::option::of(TEXT),
        games in prop::collection::vec(arb_game(), 1..5)
    ) {
        let mut xml = format!("<datafile>{}", header_of(&name, date.as_deref()));
        let (first, rest) = games.split_at(1);
        xml.push_str(&first[0]);
        if let Some(l) = late {
            xml.push_str(&header_of(&l, None));
        }
        xml.extend(rest.iter().cloned());
        xml.push_str("</datafile>");
        let options = ExportOptions::default();
        prop_assume!(parse_dat(xml.as_bytes()).is_ok());
        let out = rewritten(xml.as_bytes());
        prop_assert_eq!(seen(&out, &options), seen(xml.as_bytes(), &options));
        prop_assert_eq!(rewritten(&out), out);
    }

    #[test]
    fn a_rewritten_export_imports_alike(games in prop::collection::vec(arb_export_game(), 1..5)) {
        let mut xml = "<header><version>7</version></header><datafile>".to_owned();
        xml.extend(games);
        xml.push_str("</datafile>");
        prop_assume!(parse_dat(xml.as_bytes()).is_ok());
        let out = rewritten(xml.as_bytes());
        for options in export_options() {
            prop_assert_eq!(seen(&out, &options), seen(xml.as_bytes(), &options));
        }
        prop_assert_eq!(rewritten(&out), out);
    }
}
