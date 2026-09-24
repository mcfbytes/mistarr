use proptest::prelude::*;
use quick_xml::events::Event;
use quick_xml::{Reader, XmlVersion};

use super::*;
use crate::adapter::testutil::scratch;
use crate::platforms::by_id;

fn touch(root: &Path, rel: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, b"").expect("write");
}

fn fresh(tag: &str) -> PathBuf {
    let dir = scratch(tag);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

fn row(id: &str) -> &'static Platform {
    by_id(id).expect("id is in the table")
}

const SLOT: LaunchSlot = LaunchSlot {
    mode: LoadMode::File,
    index: 0,
    delay: 2,
    verify_on_board: true,
};

#[test]
fn newest_dated_core_wins_across_folders() {
    let root = fresh("find-newest");
    touch(&root, "_Console/NES_20230101.rbf");
    touch(&root, "_Console/Older/NES_20240301.rbf");
    touch(&root, "_Console/NES.rbf");
    touch(&root, "_Console/SNES_20250101.rbf");
    let core = find_core(&root, row("nes")).expect("core");
    assert_eq!(core.path, root.join("_Console/Older/NES_20240301.rbf"));
    assert_eq!(core.mgl_rbf, "_Console/Older/NES");
    assert_eq!(
        find_core(&root, row("fds")).map(|c| c.path),
        Some(core.path)
    );
}

#[test]
fn equal_dates_prefer_the_shorter_path() {
    let root = fresh("find-tie");
    touch(&root, "_Console/Sub/NES_20240101.rbf");
    touch(&root, "_Console/NES_20240101.rbf");
    let core = find_core(&root, row("nes")).expect("core");
    assert_eq!(core.mgl_rbf, "_Console/NES");
}

#[test]
fn undated_core_is_used_when_alone() {
    let root = fresh("find-undated");
    touch(&root, "_Other/Gameboy.rbf");
    let core = find_core(&root, row("gbc")).expect("core");
    assert_eq!(core.mgl_rbf, "_Other/Gameboy");
}

#[test]
fn row_core_names_choose_and_carry_their_parameters() {
    let root = fresh("find-alias");
    touch(&root, "_Console/Genesis_20240101.rbf");
    touch(&root, "_Console/TurboGrafx16_20240101.rbf");
    touch(&root, "_Console/ColecoVision_20240101.rbf");
    touch(&root, "_Console/Atari7800_20240101.rbf");
    touch(&root, "_Arcade/cores/NES_20990101.rbf");
    touch(&root, "_Arcade/cores/jtngp_20240101.rbf");
    let rbf = |id: &str| find_core(&root, row(id)).map(|c| c.mgl_rbf);
    assert_eq!(rbf("megadrive").as_deref(), Some("_Console/Genesis"));
    assert_eq!(rbf("pcecd").as_deref(), Some("_Console/TurboGrafx16"));
    assert_eq!(rbf("sg1000").as_deref(), Some("_Console/ColecoVision"));
    assert_eq!(rbf("atari2600").as_deref(), Some("_Console/Atari7800"));
    assert_eq!(rbf("ngp").as_deref(), Some("_Arcade/cores/jtngp"));
    assert_eq!(rbf("nes"), None, "an _Arcade rbf needs an explicit row");
    assert_eq!(rbf("arcade"), None);
    let sg = find_core(&root, row("sg1000")).expect("core");
    assert_eq!((sg.slot.index, sg.slot.delay), (0, 1));
    let coleco = find_core(&root, row("coleco")).expect("core");
    assert_eq!(coleco.path, sg.path);
    assert_eq!((coleco.slot.index, coleco.slot.delay), (1, 1));
    assert!(find_core(&root.join("absent"), row("snes")).is_none());
}

#[test]
fn an_earlier_core_name_wins_over_a_fallback() {
    let root = fresh("find-fallback");
    touch(&root, "_Console/Atari7800_20250101.rbf");
    touch(&root, "_Console/Atari2600_20200101.rbf");
    let core = find_core(&root, row("atari2600")).expect("core");
    assert_eq!(core.mgl_rbf, "_Console/Atari2600");
}

#[test]
fn zip_members_split_only_after_a_zip() {
    assert_eq!(
        split_zip_member("NES/a.zip#b.nes"),
        ("NES/a.zip", Some("b.nes"))
    );
    assert_eq!(
        split_zip_member("NES/A.Zip#b#2.nes"),
        ("NES/A.Zip", Some("b#2.nes"))
    );
    assert_eq!(split_zip_member("NES/No #1.nes"), ("NES/No #1.nes", None));
    assert_eq!(split_zip_member("NES/x.zipper#y"), ("NES/x.zipper#y", None));
}

#[test]
fn game_paths_per_kind() {
    let games = Path::new("/nonexistent");
    let path = |kind, files: &[&str]| game_path(kind, games, files);
    assert_eq!(
        path(Kind::Cartridge, &["NES/Example Quest (USA).nes"]).as_deref(),
        Some("NES/Example Quest (USA).nes")
    );
    assert_eq!(
        path(Kind::Cartridge, &["SNES/x.zip#x.sfc"]).as_deref(),
        Some("SNES/x.zip/x.sfc")
    );
    assert_eq!(
        path(Kind::Cartridge, &["NES/Quest #2 (USA).nes"]).as_deref(),
        Some("NES/Quest #2 (USA).nes")
    );
    assert_eq!(
        path(Kind::Cartridge, &["NES/Set #1.zip#Quest #2.nes"]).as_deref(),
        Some("NES/Set #1.zip/Quest #2.nes")
    );
    assert_eq!(
        path(Kind::Disc, &["Saturn/G/g.iso"]).as_deref(),
        Some("Saturn/G/g.iso")
    );
    assert_eq!(path(Kind::Disc, &["PSX/G/g.bin", "PSX/G/g.cue"]), None);
    assert_eq!(
        path(Kind::Romset, &["NeoGeo/exset.zip#p1.bin"]).as_deref(),
        Some("NeoGeo/exset.zip")
    );
    assert_eq!(
        path(Kind::Romset, &["NeoGeo/exset"]).as_deref(),
        Some("NeoGeo/exset")
    );
    assert_eq!(
        path(Kind::Romset, &["NeoGeo/exset/sub/p1.bin"]).as_deref(),
        Some("NeoGeo/exset")
    );
    assert_eq!(
        path(Kind::Romset, &["NeoGeo/Set #1/p1.bin"]).as_deref(),
        Some("NeoGeo/Set #1")
    );
    assert_eq!(path(Kind::Cartridge, &[]), None);
}

#[test]
fn a_disc_loads_the_cue_whose_files_exist() {
    let games = fresh("cue-pick");
    std::fs::create_dir_all(games.join("PSX/G")).expect("mkdir");
    std::fs::write(
        games.join("PSX/G/broken.cue"),
        "FILE \"gone.bin\" BINARY\n  TRACK 01 MODE2/2352\n",
    )
    .expect("write");
    std::fs::write(
        games.join("PSX/G/G.CUE"),
        "REM x\nFILE \"G (Track 1).bin\" BINARY\nfile G2.bin BINARY\n",
    )
    .expect("write");
    touch(&games, "PSX/G/G (Track 1).bin");
    touch(&games, "PSX/G/G2.bin");
    let files = ["PSX/G/G (Track 1).bin", "PSX/G/broken.cue", "PSX/G/G.CUE"];
    assert_eq!(
        game_path(Kind::Disc, &games, &files).as_deref(),
        Some("PSX/G/G.CUE")
    );
    std::fs::write(games.join("PSX/G/G.CUE"), "FILE \"../G2.bin\" BINARY\n").expect("write");
    assert_eq!(game_path(Kind::Disc, &games, &files), None);
}

#[test]
fn cue_files_reads_quoted_and_bare_names() {
    assert_eq!(
        cue_files("FILE \"a b.bin\" BINARY\n  File c.bin WAVE\nFILEX d\nTRACK 01\n"),
        ["a b.bin", "c.bin"]
    );
    assert!(cue_files("").is_empty());
}

#[test]
fn mgl_has_the_documented_shape() {
    let doc = mgl(
        "_Console/NES",
        SLOT,
        Path::new("/media/fat/games/NES/Example Quest (USA).nes"),
    )
    .expect("mgl");
    assert_eq!(
        doc,
        "<mistergamedescription>\n  <rbf>_Console/NES</rbf>\n  \
         <file delay=\"2\" type=\"f\" index=\"0\" \
         path=\"../../../../../media/fat/games/NES/Example Quest (USA).nes\"/>\n\
         </mistergamedescription>\n"
    );
    let disc = LaunchSlot {
        mode: LoadMode::Mount,
        index: 1,
        delay: 1,
        verify_on_board: true,
    };
    let doc = mgl("_Console/PSX", disc, Path::new("/g/PSX/a/a.cue")).expect("mgl");
    assert!(doc.contains(r#"delay="1" type="s" index="1""#));
}

#[test]
fn mgl_escapes_and_refuses_unsafe_paths() {
    let doc = mgl("_Console/A&B", SLOT, Path::new("/g/<a> \"b\" 'c' & d.nes")).expect("mgl");
    assert!(doc.contains("<rbf>_Console/A&amp;B</rbf>"));
    assert!(doc.contains("/g/&lt;a&gt; &quot;b&quot; &apos;c&apos; &amp; d.nes\""));
    for bad in [
        "relative/x.nes",
        "/g/line\nbreak.nes",
        "/g/tab\t.nes",
        "/g/nul\0.nes",
    ] {
        assert!(
            matches!(
                mgl("_Console/NES", SLOT, Path::new(bad)),
                Err(Error::UnsafePath(_))
            ),
            "{bad:?}"
        );
    }
    assert!(matches!(
        mgl("_Console/\nNES", SLOT, Path::new("/g/x.nes")),
        Err(Error::UnsafePath(_))
    ));
}

/// The `path` attribute of the first `<file>`, unescaped by an XML parser.
fn parsed_path(doc: &str) -> String {
    let mut reader = Reader::from_str(doc);
    loop {
        match reader.read_event().expect("well-formed") {
            Event::Empty(e) if e.local_name().as_ref() == "file" => {
                let attr = e
                    .attributes()
                    .map(|a| a.expect("attribute"))
                    .find(|a| a.key.local_name().as_ref() == "path")
                    .expect("path attribute");
                return attr
                    .normalized_value(XmlVersion::Implicit1_0)
                    .expect("unescape")
                    .into_owned();
            }
            Event::Eof => panic!("no <file> in {doc}"),
            _ => {}
        }
    }
}

proptest! {
    #[test]
    fn any_printable_path_round_trips_through_xml(name in "[^\\p{Cc}]{0,80}") {
        let game = format!("/media/fat/games/NES/{name}");
        let doc = mgl("_Console/NES", SLOT, Path::new(&game)).expect("mgl");
        prop_assert_eq!(parsed_path(&doc), format!("../../../../..{game}"));
    }
}

#[test]
fn write_mgl_makes_a_new_file_and_keeps_the_last_few() {
    let dir = fresh("write-mgl");
    touch(&dir, "other.mgl");
    let paths: Vec<PathBuf> = (0..5)
        .map(|i| write_mgl(&dir, &format!("doc {i}")).expect("write"))
        .collect();
    let mut unique = paths.clone();
    unique.dedup();
    assert_eq!(unique.len(), 5);
    assert_eq!(std::fs::read_to_string(&paths[4]).expect("read"), "doc 4");
    let kept: Vec<_> = paths.iter().filter(|p| p.exists()).collect();
    assert_eq!(kept, paths[5 - MGL_KEEP..].iter().collect::<Vec<_>>());
    assert!(dir.join("other.mgl").exists());
    assert!(write_mgl(&dir.join("absent"), "x").is_err());
}

#[test]
fn load_core_lines() {
    assert_eq!(
        load_core(Path::new("/tmp/mistarr.mgl")).expect("line"),
        "load_core /tmp/mistarr.mgl\n"
    );
    assert!(matches!(
        load_core(Path::new("x.rbf")),
        Err(Error::UnsafePath(_))
    ));
    assert!(matches!(
        load_core(Path::new("/a\nquit")),
        Err(Error::UnsafePath(_))
    ));
    let long = format!("/{}", "a".repeat(PIPE_BUF));
    assert!(matches!(
        load_core(Path::new(&long)),
        Err(Error::UnsafePath(_))
    ));
}

fn mkfifo(path: &Path) {
    rustix::fs::mknodat(
        rustix::fs::CWD,
        path,
        rustix::fs::FileType::Fifo,
        Mode::from_raw_mode(0o600),
        0,
    )
    .expect("mkfifo");
}

#[test]
fn fifo_sink_maps_an_absent_path() {
    let dir = fresh("fifo-absent");
    let sink = FifoSink::new(dir.join("MiSTer_cmd"));
    assert!(!sink.present());
    assert!(matches!(sink.send("x\n"), Err(Error::CommandAbsent)));
    let under_file = FifoSink::new(dir.join("MiSTer_cmd/below"));
    assert!(matches!(under_file.send("x\n"), Err(Error::CommandAbsent)));
}

#[test]
fn fifo_sink_refuses_a_regular_file() {
    let dir = fresh("fifo-regular");
    let path = dir.join("MiSTer_cmd");
    std::fs::write(&path, b"").expect("write");
    let sink = FifoSink::new(&path);
    assert!(!sink.present());
    assert!(matches!(sink.send("x\n"), Err(Error::CommandAbsent)));
    assert!(std::fs::read(&path).expect("read").is_empty());
}

#[test]
fn fifo_without_a_reader_is_not_listening() {
    let dir = fresh("fifo-noreader");
    let path = dir.join("MiSTer_cmd");
    mkfifo(&path);
    let sink = FifoSink::new(&path);
    assert!(sink.present());
    assert!(matches!(sink.send("x\n"), Err(Error::NotListening)));
}

#[test]
fn fifo_with_a_reader_receives_the_line() {
    let dir = fresh("fifo-reader");
    let path = dir.join("MiSTer_cmd");
    mkfifo(&path);
    let fd = rustix::fs::open(&path, OFlags::RDONLY | OFlags::NONBLOCK, Mode::empty())
        .expect("open reader");
    let mut reader = File::from(fd);
    let sink = FifoSink::new(&path);
    sink.send("load_core /tmp/mistarr.mgl\n").expect("send");
    let mut got = String::new();
    reader.read_to_string(&mut got).expect("read");
    assert_eq!(got, "load_core /tmp/mistarr.mgl\n");
}

#[test]
fn recording_sink_outcomes() {
    let sink = RecordingSink::new();
    assert!(sink.present());
    sink.send("a\n").expect("send");
    sink.set_outcome(FakeOutcome::NotListening);
    assert!(sink.present());
    assert!(matches!(sink.send("b\n"), Err(Error::NotListening)));
    sink.set_outcome(FakeOutcome::Absent);
    assert!(!sink.present());
    assert!(matches!(sink.send("c\n"), Err(Error::CommandAbsent)));
    assert_eq!(sink.lines(), ["a\n"]);
}

#[test]
fn fifo_sink_path_accessor() {
    assert_eq!(
        FifoSink::new(COMMAND_PATH).path(),
        Path::new("/dev/MiSTer_cmd")
    );
}
