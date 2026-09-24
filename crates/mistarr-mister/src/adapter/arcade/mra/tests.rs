use super::*;
use proptest::prelude::*;

const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<misterromdescription>
  <name>Example Blaster &amp; Friends</name>
  <setname>exblast</setname>
  <rbf>examplecore</rbf>
  <rom index="0" zip="exblast.zip|exparent.zip" md5="0123456789ABCDEF0123456789abcdef" type="merged">
    <part zip="exsound.zip" name="snd.bin" crc="00000000"/>
    <part name="cpu.bin" crc="11111111"/>
    <part zip="exblast.zip" name="gfx.bin"/>
  </rom>
  <rom index="1" md5="None"><part>00 FF</part></rom>
  <rom index="2" zip=" " md5="short"/>
</misterromdescription>"#;

#[test]
fn parses_synthetic_mra() {
    let mra = parse(SAMPLE.as_bytes()).expect("parse");
    assert_eq!(mra.name.as_deref(), Some("Example Blaster & Friends"));
    assert_eq!(mra.setname.as_deref(), Some("exblast"));
    assert_eq!(mra.rbf.as_deref(), Some("examplecore"));
    assert_eq!(mra.zips, ["exblast.zip", "exparent.zip", "exsound.zip"]);
    assert_eq!(mra.md5, ["0123456789abcdef0123456789abcdef"]);
}

#[test]
fn rom_structure_is_kept() {
    let mra = parse(SAMPLE.as_bytes()).expect("parse");
    assert_eq!(mra.roms.len(), 3);
    let r0 = &mra.roms[0];
    assert_eq!(r0.index, 0);
    assert_eq!(r0.zips, ["exblast.zip", "exparent.zip"]);
    assert_eq!(r0.md5.as_deref(), Some("0123456789abcdef0123456789abcdef"));
    let names: Vec<_> = r0
        .items
        .iter()
        .map(|i| match i {
            RomItem::Part(p) => p.name.clone().unwrap_or_default(),
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(names, ["snd.bin", "cpu.bin", "gfx.bin"]);
    let RomItem::Part(snd) = &r0.items[0] else {
        panic!("part")
    };
    assert_eq!(snd.zips, ["exsound.zip"]);
    assert_eq!(snd.crc, Some(0));
    assert_eq!(mra.roms[1].md5, None);
    assert_eq!(
        mra.roms[1].items,
        [RomItem::Part(Part {
            data: vec![0x00, 0xff],
            ..Part::default()
        })]
    );
    assert_eq!(mra.roms[2].md5, None);
    assert!(mra.roms[2].zips.is_empty());
}

#[test]
fn part_attributes_interleave_and_patch_are_read() {
    let mra = parse(
        br#"<misterromdescription><rom index="3" zip="exblast.zip">
          <part name="a.bin" offset="0x10" length="020" repeat="2"/>
          <part name="z.bin" length="0"/>
          <interleave output="16">
            <part name="even.bin" map="01"/>
            <part name="odd.bin" map="10"/>
          </interleave>
          <part repeat="4">FF</part>
          <patch offset="0x2">01 02</patch>
          <patch offset="1" operation="XOR">80</patch>
        </rom></misterromdescription>"#,
    )
    .expect("parse");
    let rom = &mra.roms[0];
    assert_eq!(rom.index, 3);
    let RomItem::Part(a) = &rom.items[0] else {
        panic!("part")
    };
    assert_eq!((a.offset, a.length, a.repeat), (16, Some(16), 2));
    let RomItem::Part(z) = &rom.items[1] else {
        panic!("part")
    };
    assert_eq!(z.length, None);
    let RomItem::Interleave(il) = &rom.items[2] else {
        panic!("interleave")
    };
    assert_eq!((il.input, il.output, il.parts.len()), (8, 16, 2));
    assert_eq!(il.parts[1].map.as_deref(), Some("10"));
    let RomItem::Part(fill) = &rom.items[3] else {
        panic!("part")
    };
    assert_eq!((fill.repeat, fill.data.as_slice()), (4, &[0xff][..]));
    assert_eq!(
        rom.items[4],
        RomItem::Patch(Patch {
            offset: 2,
            xor: false,
            data: vec![1, 2]
        })
    );
    assert_eq!(
        rom.items[5],
        RomItem::Patch(Patch {
            offset: 1,
            xor: true,
            data: vec![0x80]
        })
    );
}

#[test]
fn unknown_rom_content_is_kept_as_unsupported() {
    let mra = parse(
        br#"<misterromdescription><rom zip="exblast.zip">
          <group><part name="inside.bin"/></group>
          <part name="a.bin" offset="lots"/>
          <part>not hex</part>
          <patch offset="0">zz</patch>
          <part name="b.bin"/>
        </rom></misterromdescription>"#,
    )
    .expect("parse");
    let items = &mra.roms[0].items;
    assert_eq!(items.len(), 5);
    assert!(matches!(&items[0], RomItem::Unsupported(r) if r.contains("<group>")));
    assert!(matches!(&items[1], RomItem::Unsupported(r) if r.contains("offset")));
    assert!(matches!(&items[2], RomItem::Unsupported(r) if r.contains("hex")));
    assert!(matches!(&items[3], RomItem::Unsupported(r) if r.contains("patch")));
    assert!(matches!(&items[4], RomItem::Part(p) if p.name.as_deref() == Some("b.bin")));
}

#[test]
fn malformed_xml_is_an_error() {
    assert!(matches!(
        parse(b"<misterromdescription><rom zip=\"a.zip\"></oops>"),
        Err(Error::Mra(_))
    ));
}

#[test]
fn cdata_and_character_references_are_kept() {
    let mra = parse(
        b"<misterromdescription><name>Example &#38; Co &#x26; Blaster</name>\
          <setname><![CDATA[ex<blast>]]></setname></misterromdescription>",
    )
    .expect("parse");
    assert_eq!(mra.name.as_deref(), Some("Example & Co & Blaster"));
    assert_eq!(mra.setname.as_deref(), Some("ex<blast>"));
}

#[test]
fn nested_child_text_stays_in_the_enclosing_field() {
    let mra =
        parse(b"<misterromdescription><name>A<b>x</b>B</name><rbf>c</rbf></misterromdescription>")
            .expect("parse");
    assert_eq!(mra.name.as_deref(), Some("AxB"));
    assert_eq!(mra.rbf.as_deref(), Some("c"));
}

#[test]
fn truncated_document_is_an_error() {
    let err = parse(b"<misterromdescription><rom zip=\"exblast.zip\"><part name=\"a\"/>");
    assert!(matches!(err, Err(Error::Mra(_))));
}

#[test]
fn unknown_entity_is_an_error() {
    assert!(matches!(
        parse(b"<m><name>&bogus;</name></m>"),
        Err(Error::Mra(_))
    ));
}

#[test]
fn empty_document_has_no_zips() {
    assert_eq!(
        parse(b"<misterromdescription/>").expect("parse"),
        Mra::default()
    );
}

#[test]
fn read_and_missing_zips() {
    let dir = crate::adapter::testutil::scratch("mra");
    let path = dir.join("Example Blaster.mra");
    std::fs::write(&path, SAMPLE).expect("write");
    let mra = read(&path).expect("read");
    std::fs::write(dir.join("EXBLAST.ZIP"), b"").expect("write");
    assert_eq!(missing_zips(&mra, &dir), ["exparent.zip", "exsound.zip"]);
    assert!(matches!(read(&dir.join("absent.mra")), Err(Error::Io(_))));
}

#[test]
fn zip_locations_follow_the_mister_rules() {
    let rel = |z: &str| zip_location(z).map(|p| p.rel_path());
    assert_eq!(rel("exblast.zip").as_deref(), Some("mame/exblast.zip"));
    assert_eq!(rel("/hbmame/exhb.zip").as_deref(), Some("hbmame/exhb.zip"));
    assert_eq!(
        rel("../hbmame/exhb.zip").as_deref(),
        Some("hbmame/exhb.zip")
    );
    assert_eq!(
        rel("./a/../exblast.zip").as_deref(),
        Some("mame/exblast.zip")
    );
    assert_eq!(rel("/root.zip").as_deref(), Some("root.zip"));
    for bad in ["", "/", "../../x.zip", "a\\b.zip", "dir/"] {
        assert_eq!(rel(bad), None, "{bad}");
    }
    let mra = Mra {
        zips: vec![
            "exblast.zip".into(),
            "./exblast.zip".into(),
            "../../x.zip".into(),
        ],
        ..Mra::default()
    };
    assert_eq!(mra.zip_paths().len(), 1);
}

#[test]
fn numbers_follow_strtoul() {
    assert_eq!(number("0x1F"), Some(31));
    assert_eq!(number("010"), Some(8));
    assert_eq!(number("0"), Some(0));
    assert_eq!(number(" 12 "), Some(12));
    assert_eq!(number("09"), None);
    assert_eq!(number("x"), None);
}

proptest! {
    #[test]
    fn parse_never_panics(s in ".{0,200}") {
        let _ = parse(s.as_bytes());
    }

    #[test]
    fn hex_round_trips(bytes in proptest::collection::vec(any::<u8>(), 0..64)) {
        let text: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
        prop_assert_eq!(hex_bytes(&text.join(" ")), Some(bytes.clone()));
        prop_assert_eq!(hex_bytes(&text.concat()), Some(bytes));
    }
}
