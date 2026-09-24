use std::fmt::Write as _;

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
fn unknown_entity_stays_as_written() {
    let mra = parse(b"<m><name>A &bogus; B</name></m>").expect("parse");
    assert_eq!(mra.name.as_deref(), Some("A &bogus; B"));
}

const MIXED_CASE: &str = r#"<?xml version="1.0"?>
<MisterRomDescription>
  <Name>Example Blaster</NAME>
  <SetName>exblast</setname>
  <RBF>examplecore</Rbf>
  <ROM Index="1" ZIP="exblast.zip" MD5="0123456789ABCDEF0123456789abcdef">
    <PART Name="a.bin" CRC="0000000A" Offset="0x10"/>
    <Interleave OUTPUT="16">
      <Part name="even.bin" MAP="01"></part>
      <part NAME="odd.bin" map="10"></PART>
    </interleave>
    <Patch OFFSET="1">FF</patch>
  </rom>
</misterromdescription>"#;

#[test]
fn tag_and_attribute_case_is_ignored() {
    let mixed = parse(MIXED_CASE.as_bytes()).expect("parse");
    let lower = parse(MIXED_CASE.to_ascii_lowercase().as_bytes()).expect("parse");
    assert_eq!(mixed.name.as_deref(), Some("Example Blaster"));
    assert_eq!(mixed.setname.as_deref(), Some("exblast"));
    assert_eq!(mixed.rbf.as_deref(), Some("examplecore"));
    assert_eq!(mixed.zips, ["exblast.zip"]);
    let rom = &mixed.roms[0];
    assert_eq!(rom.index, 1);
    assert_eq!(rom.md5.as_deref(), Some("0123456789abcdef0123456789abcdef"));
    assert!(matches!(&rom.items[0], RomItem::Part(p) if p.crc == Some(10) && p.offset == 16));
    assert!(matches!(&rom.items[1], RomItem::Interleave(il) if il.parts.len() == 2));
    assert!(matches!(&rom.items[2], RomItem::Patch(p) if p.data == [0xff]));
    assert_eq!(mixed.roms, lower.roms);
}

#[test]
fn stray_and_unclosed_elements_are_tolerated() {
    let mra = parse(
        b"<misterromdescription><name>Example</b> Blaster</name>\
          <rom zip=\"exblast.zip\"><part name=\"a.bin\"></rom>\
          <rbf>excore</rbf></misterromdescription>",
    )
    .expect("parse");
    assert_eq!(mra.name.as_deref(), Some("Example Blaster"));
    assert_eq!(mra.rbf.as_deref(), Some("excore"));
    assert!(
        matches!(&mra.roms[0].items[0], RomItem::Part(p) if p.name.as_deref() == Some("a.bin"))
    );
}

#[test]
fn an_unclosed_field_keeps_out_rom_data_and_is_capped() {
    let hex = "00 ".repeat(400);
    let xml = format!(
        "<misterromdescription><setname>exblast<rom zip=\"exblast.zip\">\
         <part>{hex}</part></rom>{}</misterromdescription>",
        "x".repeat(1000)
    );
    let mra = parse(xml.as_bytes()).expect("parse");
    let setname = mra.setname.expect("setname");
    assert!(setname.starts_with("exblast"));
    assert!(!setname.contains("00"));
    assert_eq!(setname.len(), MAX_FIELD_BYTES);
    assert!(matches!(&mra.roms[0].items[0], RomItem::Part(p) if p.data.len() == 400));
    let wide = format!("<m><name>{}</name></m>", "é".repeat(300));
    let name = parse(wide.as_bytes()).expect("parse").name.expect("name");
    assert!(name.len() <= MAX_FIELD_BYTES && name.chars().all(|c| c == 'é'));
}

#[test]
fn read_refuses_an_oversized_file() {
    let dir = crate::adapter::testutil::scratch("mra-big");
    let path = dir.join("big.mra");
    let pad = " ".repeat(usize::try_from(MAX_MRA_BYTES).expect("fits"));
    std::fs::write(&path, format!("<m>{pad}</m>")).expect("write");
    assert!(matches!(read(&path), Err(Error::Mra(m)) if m.contains("larger")));
}

/// `text` with each ASCII letter upper-cased where `mask` has a set bit, cycling the mask.
fn recase(text: &str, mask: &[bool]) -> String {
    text.chars()
        .zip(mask.iter().cycle())
        .map(|(c, &up)| if up { c.to_ascii_uppercase() } else { c })
        .collect()
}

/// An MRA whose markup, not its values, is recased by `mask`.
fn recased_mra(name: &str, zip: &str, index: u32, mask: &[bool]) -> String {
    let t = |s: &str| recase(s, mask);
    format!(
        "<{m}><{n}>{name}</{n2}><{r} {i}=\"{index}\" {z}=\"{zip}\"><{p} {pn}=\"a.bin\"/></{r2}></{m2}>",
        m = t("misterromdescription"),
        m2 = t("MISTERROMDESCRIPTION"),
        n = t("name"),
        n2 = t("NAME"),
        r = t("rom"),
        r2 = t("ROM"),
        i = t("index"),
        z = t("zip"),
        p = t("part"),
        pn = t("name"),
    )
}

/// `bytes` as hex digit pairs, joined by separators and markup MiSTer's loader reads through.
fn hex_text(bytes: &[u8], seps: &[usize]) -> String {
    let mut text = String::new();
    for (i, b) in bytes.iter().enumerate() {
        let pair = format!("{b:02X}");
        match seps[i % seps.len()] {
            0 => text.push(' '),
            1 => text.push_str(",\n"),
            2 => text.push_str("<!-- c -->"),
            3 => {
                write!(text, "<![CDATA[{pair}]]>").expect("write");
                continue;
            }
            4 => {
                let first = u32::from(pair.as_bytes()[0]);
                write!(text, "&#x{first:X};{}", &pair[1..]).expect("write");
                continue;
            }
            _ => {}
        }
        text.push_str(&pair);
    }
    text
}

#[test]
fn a_read_mra_leaves_inline_data_in_the_file() {
    let dir = crate::adapter::testutil::scratch("mra-inline");
    let path = dir.join("Example Inline.mra");
    let body = "01 02 03 ".repeat(1000);
    let xml = format!(
        "<misterromdescription><name>Example Inline</name><rom index=\"1\" md5=\"none\">\
         <part>{body}</part><PART>ff</Part><part></part><part>zz</part></rom></misterromdescription>"
    );
    std::fs::write(&path, &xml).expect("write");
    let mra = read(&path).expect("read");
    let items = &mra.roms[0].items;
    let RomItem::Part(big) = &items[0] else {
        panic!("{items:?}")
    };
    let inline = big.inline.as_ref().expect("inline");
    assert!(big.data.is_empty());
    assert_eq!(inline.len, 3000);
    assert_eq!(&*inline.file, path.as_path());
    let mut bytes = Vec::new();
    open_inline(inline)
        .expect("open")
        .read_to_end(&mut bytes)
        .expect("read");
    assert_eq!(bytes, [1u8, 2, 3].repeat(1000));
    let RomItem::Part(small) = &items[1] else {
        panic!("{items:?}")
    };
    assert_eq!(small.inline.as_ref().map(|i| i.len), Some(1));
    let RomItem::Part(empty) = &items[2] else {
        panic!("{items:?}")
    };
    assert_eq!((empty.inline.as_ref(), empty.data.len()), (None, 0));
    assert_eq!(
        items[3],
        RomItem::Unsupported("inline part data is not hex".into())
    );

    std::fs::write(&path, xml.replace("01 02 03", "01 02 0Z")).expect("rewrite");
    let mut changed = Vec::new();
    let err = open_inline(inline)
        .expect("open")
        .read_to_end(&mut changed)
        .expect_err("changed");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn a_large_mra_under_the_cap_is_read() {
    let dir = crate::adapter::testutil::scratch("mra-large");
    let path = dir.join("Example Large.mra");
    let body = "00 11 22 33 44 55 66 77\n".repeat(128 * 1024);
    let xml = format!("<m><rom index=\"0\"><part>{body}</part></rom></m>");
    assert!(xml.len() > 3 * 1024 * 1024);
    std::fs::write(&path, &xml).expect("write");
    let mra = read(&path).expect("read");
    let RomItem::Part(part) = &mra.roms[0].items[0] else {
        panic!()
    };
    assert_eq!(part.inline.as_ref().map(|i| i.len), Some(1024 * 1024));
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
    fn markup_case_never_changes_the_result(
        name in "[A-Za-z0-9 ]{1,20}",
        zip in "[a-z0-9]{1,8}\\.zip",
        index in 0u32..8,
        mask in proptest::collection::vec(any::<bool>(), 1..16),
    ) {
        let mixed = parse(recased_mra(&name, &zip, index, &mask).as_bytes()).expect("parse");
        let plain = parse(recased_mra(&name, &zip, index, &[false]).as_bytes()).expect("parse");
        prop_assert_eq!(&mixed, &plain);
        prop_assert_eq!(mixed.roms[0].index, index);
        prop_assert_eq!(mixed.zips, vec![zip]);
    }

    #[test]
    fn inline_data_read_from_a_file_matches_the_bytes(
        bytes in proptest::collection::vec(any::<u8>(), 0..512),
        seps in proptest::collection::vec(0usize..6, 1..16),
        tag in 0u64..1_000_000,
    ) {
        let text = hex_text(&bytes, &seps);
        let xml = format!("<m><name>x</name><rom index=\"0\"><part>{text}</part><part>0A</part></rom></m>");
        let from_memory = parse(xml.as_bytes()).expect("parse");
        let dir = crate::adapter::testutil::scratch("mra-inline-prop");
        let path = dir.join(format!("{tag}.mra"));
        std::fs::write(&path, &xml).expect("write");
        let from_file = read(&path).expect("read");
        let (RomItem::Part(mem), RomItem::Part(file)) =
            (&from_memory.roms[0].items[0], &from_file.roms[0].items[0])
        else {
            panic!("parts expected");
        };
        prop_assert_eq!(&mem.data, &bytes);
        prop_assert!(file.data.is_empty());
        let streamed = file.inline.as_ref().map_or_else(Vec::new, |i| {
            let mut out = Vec::new();
            open_inline(i).expect("open").read_to_end(&mut out).expect("read");
            out
        });
        prop_assert_eq!(streamed, bytes);
        std::fs::remove_file(&path).expect("remove");
    }

    #[test]
    fn hex_decodes_the_same_however_the_text_is_split(text in "[0-9a-fA-F ,\n]{0,64}", cut in 0usize..64) {
        let (head, tail) = text.as_bytes().split_at(cut.min(text.len()));
        let mut hex = Hex::default();
        let mut out = Vec::new();
        hex.feed(head, Some(&mut out));
        hex.feed(tail, Some(&mut out));
        hex.finish(Some(&mut out));
        prop_assert_eq!((!hex.bad).then_some(out), hex_bytes(&text));
    }

    #[test]
    fn hex_round_trips(bytes in proptest::collection::vec(any::<u8>(), 0..64)) {
        let text: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
        prop_assert_eq!(hex_bytes(&text.join(" ")), Some(bytes.clone()));
        prop_assert_eq!(hex_bytes(&text.concat()), Some(bytes));
    }
}
