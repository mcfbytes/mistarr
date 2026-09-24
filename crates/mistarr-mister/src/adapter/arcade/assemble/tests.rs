use std::collections::HashMap;

use super::super::mra::parse;
use super::*;

/// Parts held in memory as `(zip, member) -> bytes`.
#[derive(Default)]
struct Mem(HashMap<(String, String), Vec<u8>>, Vec<String>);

impl Mem {
    fn with(parts: &[(&str, &str, &[u8])]) -> Self {
        let mut m = Self::default();
        for (zip, name, bytes) in parts {
            m.0.insert(((*zip).to_owned(), (*name).to_owned()), bytes.to_vec());
        }
        m
    }
}

impl PartSource for Mem {
    fn open(
        &mut self,
        zip: &str,
        name: &str,
        crc: Option<u32>,
    ) -> io::Result<Option<Box<dyn Read + '_>>> {
        self.1.push(format!("{zip}#{name}"));
        let by_name = self.0.get(&(zip.to_owned(), name.to_owned()));
        let by_crc = || {
            let want = crc?;
            self.0
                .iter()
                .find(|((z, _), b)| z == zip && crc32(b) == want)
                .map(|(_, b)| b)
        };
        Ok(by_name
            .or_else(by_crc)
            .map(|b| Box::new(io::Cursor::new(b.clone())) as Box<dyn Read>))
    }
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                crc >> 1 ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn rom(xml: &str) -> MraRom {
    let mra = parse(format!("<misterromdescription>{xml}</misterromdescription>").as_bytes())
        .expect("parse");
    mra.roms.into_iter().next().expect("one rom")
}

fn md5_of(bytes: &[u8]) -> String {
    let mut m = Md5Stream::new();
    m.update(bytes);
    m.finish()
}

#[test]
fn whole_parts_concatenate_and_hash_like_mister() {
    let mut src = Mem::with(&[
        ("exblast.zip", "a.bin", b"ABC"),
        ("exblast.zip", "b.bin", b"DE"),
    ]);
    let r = rom(r#"<rom zip="exblast.zip"><part name="a.bin"/><part name="b.bin"/></rom>"#);
    let out = assemble(&r, &mut src).expect("assemble");
    assert_eq!(out.data, b"ABCDE");
    assert_eq!(out.md5, md5_of(b"ABCDE"));
    assert_eq!(md5(&r, &mut src).expect("md5"), out.md5);
}

#[test]
fn offset_length_repeat_and_inline_data() {
    let mut src = Mem::with(&[("exblast.zip", "a.bin", b"0123456789")]);
    let r = rom(r#"<rom zip="exblast.zip">
             <part name="a.bin" offset="0x2" length="3" repeat="2"/>
             <part repeat="3">FF</part>
             <part name="a.bin" offset="8"/>
             <part name="a.bin" length="99"/>
           </rom>"#);
    let out = assemble(&r, &mut src).expect("assemble");
    let want = b"234234\xff\xff\xff890123456789";
    assert_eq!(out.data, want);
    assert_eq!(out.md5, md5_of(want));
}

#[test]
fn interleave_places_bytes_and_md5_follows_input_order() {
    let mut src = Mem::with(&[
        ("exblast.zip", "even.bin", b"aceg"),
        ("exblast.zip", "odd.bin", b"bdfh"),
    ]);
    let r = rom(r#"<rom zip="exblast.zip"><part>00</part>
             <interleave output="16">
               <part name="even.bin" map="01"/>
               <part name="odd.bin" map="10"/>
             </interleave>
             <part>11</part></rom>"#);
    let out = assemble(&r, &mut src).expect("assemble");
    assert_eq!(out.data, b"\x00abcdefgh\x11");
    assert_eq!(out.md5, md5_of(b"\x00acegbdfh\x11"));
}

#[test]
fn byte_swapping_and_wide_words() {
    let mut src = Mem::with(&[
        ("exblast.zip", "w.bin", b"ABCD"),
        ("exblast.zip", "p0.bin", b"15"),
        ("exblast.zip", "p1.bin", b"26"),
        ("exblast.zip", "p2.bin", b"37"),
        ("exblast.zip", "p3.bin", b"48"),
    ]);
    let r = rom(r#"<rom zip="exblast.zip">
             <interleave output="16"><part name="w.bin" map="12"/></interleave>
             <interleave output="32">
               <part name="p0.bin" map="0001"/><part name="p1.bin" map="0010"/>
               <part name="p2.bin" map="0100"/><part name="p3.bin" map="1000"/>
             </interleave></rom>"#);
    let out = assemble(&r, &mut src).expect("assemble");
    assert_eq!(out.data, b"BADC12345678");
}

#[test]
fn two_byte_map_with_a_gap() {
    let mut src = Mem::with(&[
        ("exblast.zip", "a.bin", b"ABCD"),
        ("exblast.zip", "b.bin", b"xy"),
    ]);
    let r = rom(r#"<rom zip="exblast.zip"><interleave output="32">
             <part name="a.bin" map="0201"/>
             <part name="b.bin" map="0010"/>
           </interleave></rom>"#);
    let out = assemble(&r, &mut src).expect("assemble");
    assert_eq!(out.data, b"AxB\0CyD\0");
}

#[test]
fn patches_replace_or_xor_and_do_not_change_the_md5() {
    let mut src = Mem::with(&[("exblast.zip", "a.bin", &[0u8, 0x0f, 0xf0, 0xff])]);
    let r = rom(r#"<rom zip="exblast.zip"><part name="a.bin"/>
             <patch offset="1">AA BB</patch>
             <patch offset="3" operation="xor">0F</patch></rom>"#);
    let out = assemble(&r, &mut src).expect("assemble");
    assert_eq!(out.data, [0, 0xaa, 0xbb, 0xf0]);
    assert_eq!(out.md5, md5_of(&[0, 0x0f, 0xf0, 0xff]));
    let past = rom(r#"<rom><part>00</part><patch offset="1">01</patch></rom>"#);
    assert!(matches!(
        md5(&past, &mut NoParts),
        Err(Error::MraUnsupported(_))
    ));
}

#[test]
fn zip_lists_part_overrides_and_crc_lookup() {
    let mut src = Mem::with(&[
        ("exparent.zip", "a.bin", b"P"),
        ("exsound.zip", "s.bin", b"S"),
        ("exblast.zip", "renamed.bin", b"C"),
    ]);
    let crc = format!("{:08x}", crc32(b"C"));
    let r = rom(&format!(
        r#"<rom zip="exblast.zip|exparent.zip">
             <part name="a.bin"/>
             <part zip="exsound.zip" name="s.bin"/>
             <part name="c.bin" crc="{crc}"/></rom>"#
    ));
    let out = assemble(&r, &mut src).expect("assemble");
    assert_eq!(out.data, b"PSC");
    src.1.dedup();
    assert_eq!(
        src.1[..3],
        [
            "exblast.zip#a.bin",
            "exparent.zip#a.bin",
            "exsound.zip#s.bin"
        ]
    );
}

#[test]
fn a_missing_part_is_reported() {
    let mut src = Mem::with(&[("exblast.zip", "a.bin", b"A")]);
    let r = rom(r#"<rom zip="exblast.zip|exparent.zip"><part name="gone.bin"/></rom>"#);
    match md5(&r, &mut src) {
        Err(Error::MissingPart { part, zips }) => {
            assert_eq!(part, "gone.bin");
            assert_eq!(zips, "exblast.zip|exparent.zip");
        }
        other => panic!("expected a missing part, got {other:?}"),
    }
}

#[test]
fn unsupported_content_is_refused_before_reading() {
    for xml in [
        r#"<rom zip="exblast.zip"><group/></rom>"#,
        r#"<rom zip="exblast.zip"><part name="a.bin" map="01"/></rom>"#,
        r#"<rom zip="exblast.zip"><interleave input="16" output="32"/></rom>"#,
        r#"<rom zip="exblast.zip"><interleave output="12"/></rom>"#,
        r#"<rom zip="exblast.zip"><interleave output="8"><part name="a.bin" map="10"/></interleave></rom>"#,
        r#"<rom zip="exblast.zip"><interleave output="16"><part name="a.bin" map="03"/></interleave></rom>"#,
        r#"<rom zip="exblast.zip"><interleave output="16"><part name="a.bin" map="zz"/></interleave></rom>"#,
        r#"<rom><part name="a.bin"/></rom>"#,
    ] {
        let mut src = Mem::with(&[("exblast.zip", "a.bin", b"AB")]);
        let r = rom(xml);
        assert!(
            matches!(md5(&r, &mut src), Err(Error::MraUnsupported(_))),
            "{xml}"
        );
        if xml.contains("<group") || xml.contains("map") || xml.contains("input") {
            assert!(src.1.is_empty(), "{xml} read a part");
        }
    }
}

#[test]
fn offsets_past_the_end_and_ragged_words_are_refused() {
    let mut src = Mem::with(&[("exblast.zip", "a.bin", b"ABC")]);
    let past = rom(r#"<rom zip="exblast.zip"><part name="a.bin" offset="4"/></rom>"#);
    assert!(matches!(
        md5(&past, &mut src),
        Err(Error::MraUnsupported(_))
    ));
    let ragged = rom(
        r#"<rom zip="exblast.zip"><interleave output="16"><part name="a.bin" map="21"/></interleave></rom>"#,
    );
    assert!(matches!(
        md5(&ragged, &mut src),
        Err(Error::MraUnsupported(_))
    ));
}

#[test]
fn runaway_repeat_is_refused() {
    let r = rom(r#"<rom><part repeat="0xffffffff">00 00 00 00 00 00 00 00</part></rom>"#);
    assert!(matches!(
        md5(&r, &mut NoParts),
        Err(Error::MraUnsupported(_))
    ));
}

#[test]
fn layout_matches_mister_rom_data() {
    let l = layout(Some("10"), 2).expect("layout");
    assert_eq!((l.idx, l.offsets.as_slice()), (1, &[1][..]));
    let l = layout(None, 1).expect("layout");
    assert_eq!((l.idx, l.offsets.as_slice()), (0, &[0][..]));
    let l = layout(Some("12"), 2).expect("layout");
    assert_eq!(l.offsets, [1, 0]);
    assert!(layout(Some("0123456789abcdef0"), 8).is_err());
}

#[test]
fn empty_parts_may_not_repeat() {
    let mut src = Mem::with(&[("exblast.zip", "empty.bin", b"")]);
    for xml in [
        r#"<rom zip="exblast.zip"><part name="empty.bin" repeat="2"/></rom>"#,
        r#"<rom zip="exblast.zip"><part name="empty.bin" repeat="0xffffffffffffffff"/></rom>"#,
        r#"<rom><part repeat="3"></part></rom>"#,
    ] {
        assert!(
            matches!(md5(&rom(xml), &mut src), Err(Error::MraUnsupported(_))),
            "{xml}"
        );
    }
    let once = rom(r#"<rom zip="exblast.zip"><part name="empty.bin"/><part>01</part></rom>"#);
    assert_eq!(assemble(&once, &mut src).expect("assemble").data, [1]);
}

#[test]
fn repeats_read_a_named_part_once_and_are_capped() {
    let mut src = Mem::with(&[("exblast.zip", "a.bin", b"AB")]);
    let r = rom(r#"<rom zip="exblast.zip"><part name="a.bin" repeat="3"/></rom>"#);
    assert_eq!(assemble(&r, &mut src).expect("assemble").data, b"ABABAB");
    assert_eq!(src.1, ["exblast.zip#a.bin"]);

    let over = rom(&format!(
        r#"<rom zip="exblast.zip"><part name="a.bin" repeat="{}"/></rom>"#,
        MAX_REPEAT + 1
    ));
    assert!(matches!(
        md5(&over, &mut src),
        Err(Error::MraUnsupported(_))
    ));

    let big = vec![0u8; 256 * 1024];
    let mut src = Mem::with(&[("exblast.zip", "big.bin", &big)]);
    let r = rom(&format!(
        r#"<rom zip="exblast.zip"><part name="big.bin" repeat="{MAX_REPEAT}"/></rom>"#
    ));
    match md5(&r, &mut src) {
        Err(Error::MraUnsupported(m)) => assert!(m.contains("larger than"), "{m}"),
        other => panic!("expected refusal, got {other:?}"),
    }
}

#[test]
fn the_digest_streams_each_repeat_and_skips_repeat_zero() {
    let big: Vec<u8> = (0..STREAM_CHUNK + 5)
        .map(|i| u8::try_from(i % 253).expect("byte"))
        .collect();
    let mut src = Mem::with(&[("exblast.zip", "a.bin", &big)]);
    let r = rom(r#"<rom zip="exblast.zip"><part name="a.bin" offset="1" repeat="3"/></rom>"#);
    let whole = assemble(&r, &mut src).expect("assemble");
    assert_eq!(whole.data.len(), 3 * (big.len() - 1));
    src.1.clear();
    assert_eq!(md5(&r, &mut src).expect("md5"), whole.md5);
    assert_eq!(src.1.len(), 3);

    src.1.clear();
    let none =
        rom(r#"<rom zip="exblast.zip"><part name="gone.bin" repeat="0"/><part>01</part></rom>"#);
    assert_eq!(md5(&none, &mut src).expect("md5"), md5_of(&[1]));
    assert_eq!(assemble(&none, &mut src).expect("assemble").data, [1]);
    assert!(src.1.is_empty());
}

#[test]
fn overflowing_offsets_are_refused() {
    let mut src = Mem::with(&[("exblast.zip", "a.bin", b"ABCD")]);
    for xml in [
        r#"<rom zip="exblast.zip"><part name="a.bin"/><patch offset="0xffffffffffffffff">01 02</patch></rom>"#,
        r#"<rom zip="exblast.zip"><part name="a.bin" offset="0xffffffffffffffff"/></rom>"#,
        r#"<rom zip="exblast.zip"><part name="a.bin" offset="0xffffffffffffffff" length="0xffffffffffffffff"/></rom>"#,
    ] {
        let r = rom(xml);
        assert!(
            matches!(assemble(&r, &mut src), Err(Error::MraUnsupported(_))),
            "{xml}"
        );
        assert!(
            matches!(md5(&r, &mut src), Err(Error::MraUnsupported(_))),
            "{xml}"
        );
    }
}

#[test]
fn streamed_md5_matches_the_assembled_rom_across_chunks() {
    let big: Vec<u8> = (0..3 * STREAM_CHUNK + 6)
        .map(|i| u8::try_from(i % 251).expect("byte"))
        .collect();
    let odd: Vec<u8> = big.iter().rev().copied().collect();
    let mut src = Mem::with(&[
        ("exblast.zip", "a.bin", &big),
        ("exblast.zip", "b.bin", &odd),
    ]);
    let r = rom(
        r#"<rom zip="exblast.zip"><part name="a.bin" offset="3" length="0x30001"/>
           <interleave output="16"><part name="a.bin" map="01"/><part name="b.bin" map="10"/></interleave>
           <part name="b.bin" repeat="2" length="4"/></rom>"#,
    );
    let out = assemble(&r, &mut src).expect("assemble");
    assert_eq!(md5(&r, &mut src).expect("md5"), out.md5);
    let odd_words = rom(
        r#"<rom zip="exblast.zip"><interleave output="16"><part name="a.bin" length="3" map="12"/></interleave></rom>"#,
    );
    assert!(matches!(
        md5(&odd_words, &mut src),
        Err(Error::MraUnsupported(_))
    ));
    let past = rom(r#"<rom zip="exblast.zip"><part name="a.bin" offset="0x100000"/></rom>"#);
    assert!(matches!(md5(&past, &mut src), Err(Error::MraUnsupported(m)) if m.contains("past")));
}

#[test]
fn inline_parts_read_from_a_file_hash_and_assemble_as_in_memory() {
    let hex = (0..3 * STREAM_CHUNK).fold(String::new(), |mut s, i| {
        std::fmt::Write::write_fmt(&mut s, format_args!("{:02x} ", i % 253)).expect("write");
        s
    });
    let xml = format!(
        r#"<m><rom zip="exblast.zip"><part>{hex}</part><part name="a.bin"/>
           <interleave output="16"><part map="01">{hex}</part><part name="b.bin" map="10"/></interleave>
           <part repeat="3">0a 0b</part><patch offset="2">ff</patch></rom>
           <rom><part repeat="2"> </part></rom></m>"#
    );
    let b = vec![7u8; 3 * STREAM_CHUNK];
    let mut src = Mem::with(&[
        ("exblast.zip", "a.bin", b"abcd"),
        ("exblast.zip", "b.bin", &b),
    ]);
    let dir = crate::adapter::testutil::scratch("assemble-inline");
    let path = dir.join("Example Inline.mra");
    std::fs::write(&path, &xml).expect("write");
    let from_file = crate::adapter::arcade::mra::read(&path).expect("read");
    let in_memory = parse(xml.as_bytes()).expect("parse");
    let (file_rom, mem_rom) = (&from_file.roms[0], &in_memory.roms[0]);
    let expected = assemble(mem_rom, &mut src).expect("assemble");
    assert_eq!(md5(file_rom, &mut src).expect("md5"), expected.md5);
    assert_eq!(assemble(file_rom, &mut src).expect("assemble"), expected);
    assert!(matches!(
        md5(&from_file.roms[1], &mut src),
        Err(Error::MraUnsupported(m)) if m.contains("empty")
    ));
}
