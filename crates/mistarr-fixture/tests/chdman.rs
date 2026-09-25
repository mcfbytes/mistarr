//! Synthetic Redump-style discs encoded by MAME's chdman decode to their source bins.
//!
//! Runs when `chdman` is on `PATH`; with `MISTARR_CHDMAN_REQUIRE=1` a missing chdman fails.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command;

use mistarr_core::chd::{self, ChdError, Decoder, Step, Unidentifiable};
use mistarr_core::hash::{hash_reader, HeaderRule};
use mistarr_core::HashSet;
use mistarr_fixture::chd::{track_bin, write_redump_set, Kind, Spec, TrackSpec};

fn chdman() -> Option<PathBuf> {
    let found = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join("chdman"))
            .find(|p| p.is_file())
    });
    if found.is_none() {
        assert!(
            std::env::var("MISTARR_CHDMAN_REQUIRE").as_deref() != Ok("1"),
            "chdman is required but not on PATH"
        );
        eprintln!("chdman not on PATH; skipping");
    }
    found
}

fn track(kind: Kind, frames: u32, pregap: u32, stored: bool) -> TrackSpec {
    TrackSpec {
        kind,
        frames,
        pregap,
        pregap_stored: stored,
    }
}

/// Mode 1 (odd length), two audio tracks and a Mode 2 track, each after an INDEX 00 pregap.
fn split_disc() -> Spec {
    Spec::new(
        "Synthetic Split Disc",
        vec![
            track(Kind::Mode1Raw, 301, 0, true),
            track(Kind::Audio, 200, 150, true),
            track(Kind::Audio, 181, 150, true),
            track(Kind::Mode2Raw, 120, 150, true),
        ],
    )
}

/// An audio first track with a hidden pregap before INDEX 01, then a Mode 1 track.
fn hidden_pregap_disc() -> Spec {
    Spec::new(
        "Synthetic Hidden Pregap Disc",
        vec![
            track(Kind::Audio, 200, 75, true),
            track(Kind::Mode1Raw, 150, 150, true),
        ],
    )
}

fn create(chdman: &Path, cue: &Path, out: &Path, args: &[&str]) {
    let status = Command::new(chdman)
        .arg("createcd")
        .arg("-i")
        .arg(cue)
        .arg("-o")
        .arg(out)
        .arg("-f")
        .args(args)
        .output()
        .expect("run chdman");
    assert!(
        status.status.success(),
        "chdman {args:?}: {}",
        String::from_utf8_lossy(&status.stderr)
    );
}

fn decode(bytes: &[u8]) -> Result<Vec<HashSet>, ChdError> {
    let h = chd::read_header(bytes)?;
    let mut c = Cursor::new(bytes);
    let layout = chd::read_layout(&mut c, &h)?;
    let mut d = Decoder::new(c, h, layout)?;
    while d.step(64)? != Step::Done {}
    d.finish()
}

fn source_hashes(spec: &Spec) -> Vec<HashSet> {
    (0..spec.tracks.len())
        .map(|t| hash_reader(track_bin(spec, t), HeaderRule::None, None).expect("hash"))
        .collect()
}

/// Track starts per chdman: each track after the previous one padded to 4 frames.
fn expected_starts(spec: &Spec) -> Vec<(u64, u32)> {
    let mut start = 0u64;
    spec.tracks
        .iter()
        .map(|t| {
            let frames = t.frames + if t.pregap_stored { t.pregap } else { 0 };
            let here = (start, frames);
            start += u64::from(frames + (4 - frames % 4) % 4);
            here
        })
        .collect()
}

const RUNS: [&[&str]; 6] = [
    &[],
    &["-c", "cdzs"],
    &["-c", "cdfl"],
    &["-c", "cdzl"],
    &["-hs", "2448"],
    &["-hs", "66096"],
];

#[test]
fn chdman_images_decode_to_the_source_bins() {
    let Some(chdman) = chdman() else { return };
    let dir = tempfile::tempdir().expect("tmp");
    let discs = [
        (split_disc(), false, "split"),
        (hidden_pregap_disc(), false, "hidden"),
        (split_disc(), true, "single"),
    ];
    for (spec, single, name) in discs {
        let cue = write_redump_set(&spec, &dir.path().join(name), single).expect("set");
        let want = source_hashes(&spec);
        for (i, args) in RUNS.iter().enumerate() {
            let out = dir.path().join(format!("{name}-{i}.chd"));
            create(&chdman, &cue, &out, args);
            let bytes = std::fs::read(&out).expect("read");
            let got = decode(&bytes).unwrap_or_else(|e| panic!("{name} {args:?}: {e}"));
            assert_eq!(got, want, "{name} {args:?}");
            let h = chd::read_header(&bytes[..]).expect("header");
            let layout = chd::read_layout(&mut Cursor::new(&bytes), &h).expect("layout");
            let starts: Vec<(u64, u32)> = layout
                .tracks()
                .iter()
                .map(|t| (t.start_frame, t.frames))
                .collect();
            assert_eq!(starts, expected_starts(&spec), "{name} {args:?}");
            std::fs::remove_file(&out).expect("rm");
        }
    }
}

/// chdman leaves both SHA1s of an uncompressed image zero, so its header is rejected.
#[test]
fn chdman_uncompressed_images_carry_no_checksum() {
    let Some(chdman) = chdman() else { return };
    let dir = tempfile::tempdir().expect("tmp");
    for (spec, name) in [(split_disc(), "split"), (hidden_pregap_disc(), "hidden")] {
        let cue = write_redump_set(&spec, &dir.path().join(name), false).expect("set");
        let out = dir.path().join(format!("{name}-none.chd"));
        create(&chdman, &cue, &out, &["-c", "none"]);
        let bytes = std::fs::read(&out).expect("read");
        assert!(
            bytes[64..104].iter().all(|&b| b == 0),
            "{name}: SHA1s are zero"
        );
        let got = chd::read_header(&bytes[..]).err().and_then(|e| e.reason());
        assert_eq!(got, Some(Unidentifiable::NoChecksum), "{name}");
    }
}

#[test]
fn chdman_cooked_and_virtual_pregap_images_are_not_identified() {
    let Some(chdman) = chdman() else { return };
    let dir = tempfile::tempdir().expect("tmp");
    let cooked = Spec::new(
        "Synthetic Cooked Disc",
        vec![
            track(Kind::Mode1Cooked, 100, 0, true),
            track(Kind::Audio, 50, 150, true),
        ],
    );
    let pregap = Spec::new(
        "Synthetic Virtual Pregap Disc",
        vec![
            track(Kind::Mode1Raw, 100, 0, true),
            track(Kind::Audio, 50, 150, false),
        ],
    );
    for (spec, want) in [
        (cooked, Unidentifiable::Cooked),
        (pregap, Unidentifiable::PregapMissing),
    ] {
        let cue = write_redump_set(&spec, &dir.path().join(&spec.label), false).expect("set");
        let out = dir.path().join(format!("{}.chd", spec.label));
        create(&chdman, &cue, &out, &[]);
        let bytes = std::fs::read(&out).expect("read");
        let h = chd::read_header(&bytes[..]).expect("header");
        let got = chd::read_layout(&mut Cursor::new(&bytes), &h)
            .err()
            .and_then(|e| e.reason());
        assert_eq!(got, Some(want), "{}", spec.label);
    }
}
