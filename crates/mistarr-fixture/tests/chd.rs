//! The synthetic CHD writer against the mistarr-core decoder and the chd-rs oracle.

use std::io::{Cursor, Read};

use mistarr_core::chd::{self, ChdError, Decoder, Step, TrackKind, Unidentifiable};
use mistarr_core::HashSet;
use mistarr_fixture::chd::{to_vec, Codec, Kind, Pick, Spec, TrackSpec, Tree, Written};
use proptest::prelude::*;
use sha1::{Digest as _, Sha1};

fn track(kind: Kind, frames: u32, pregap: u32) -> TrackSpec {
    TrackSpec {
        kind,
        frames,
        pregap,
        pregap_stored: true,
    }
}

/// The three-track disc of the codec round trips.
fn three_tracks() -> Vec<TrackSpec> {
    vec![
        track(Kind::Mode1Raw, 40, 0),
        track(Kind::Audio, 37, 5),
        track(Kind::Mode2Raw, 9, 0),
    ]
}

fn decode_steps(bytes: &[u8], step: u32) -> Result<Vec<HashSet>, ChdError> {
    let h = chd::read_header(bytes)?;
    let mut c = Cursor::new(bytes);
    let layout = chd::read_layout(&mut c, &h)?;
    let mut d = Decoder::new(c, h, layout)?;
    while d.step(step)? != Step::Done {}
    d.finish()
}

fn decode(bytes: &[u8]) -> Result<Vec<HashSet>, ChdError> {
    decode_steps(bytes, 32)
}

fn reason(bytes: &[u8]) -> Option<Unidentifiable> {
    decode(bytes).err().and_then(|e| e.reason())
}

fn used(w: &Written, codec: Codec) -> u32 {
    w.codec_use
        .iter()
        .find(|(c, _)| *c == codec)
        .map_or(0, |(_, n)| *n)
}

/// The decoded data per chd-rs, hunk by hunk, and its CHT2 track list as `(TYPE, FRAMES)`.
fn chd_rs(bytes: &[u8]) -> ([u8; 20], Vec<(String, u32)>) {
    let mut c = ::chd::Chd::open(Cursor::new(bytes.to_vec()), None).expect("chd-rs opens it");
    let logical = c.header().logical_bytes();
    let count = c.header().hunk_count();
    let mut out = c.get_hunksized_buffer();
    let mut comp = Vec::new();
    let mut sha = Sha1::new();
    let mut left = logical;
    for i in 0..count {
        c.hunk(i)
            .expect("hunk")
            .read_hunk_in(&mut comp, &mut out)
            .expect("chd-rs decodes it");
        let n = usize::try_from(left.min(out.len() as u64)).expect("fits");
        sha.update(&out[..n]);
        left -= n as u64;
    }
    let refs: Vec<_> = c.metadata_refs().collect();
    let mut tracks = Vec::new();
    for r in refs {
        let m = r.read(c.inner()).expect("metadata");
        if m.metatag.to_be_bytes() != *b"CHT2" {
            continue;
        }
        let text = String::from_utf8_lossy(&m.value)
            .trim_end_matches('\0')
            .to_owned();
        let field = |k: &str| {
            text.split_whitespace()
                .find_map(|t| t.strip_prefix(&format!("{k}:")))
                .unwrap_or_default()
                .to_owned()
        };
        tracks.push((field("TYPE"), field("FRAMES").parse().expect("frames")));
    }
    (sha.finalize().into(), tracks)
}

/// Our layout as `(TYPE, FRAMES)`, for comparison with chd-rs.
fn our_tracks(bytes: &[u8]) -> Vec<(String, u32)> {
    let h = chd::read_header(bytes).expect("header");
    let l = chd::read_layout(&mut Cursor::new(bytes), &h).expect("layout");
    l.tracks()
        .iter()
        .map(|t| {
            let ty = match t.kind {
                TrackKind::Mode1Raw => "MODE1_RAW",
                TrackKind::Mode2Raw => "MODE2_RAW",
                _ => "AUDIO",
            };
            (ty.to_owned(), t.frames)
        })
        .collect()
}

fn agrees_with_chd_rs(bytes: &[u8], w: &Written) {
    let (raw, tracks) = chd_rs(bytes);
    assert_eq!(
        raw, w.raw_sha1,
        "chd-rs decodes the same bytes, subcode included"
    );
    assert_eq!(tracks, our_tracks(bytes));
}

fn every_codec_images() -> Vec<(Option<Codec>, Vec<u8>, Written)> {
    let codecs = [
        Codec::CdLzma,
        Codec::CdZlib,
        Codec::CdZstd,
        Codec::CdFlac,
        Codec::Zlib,
        Codec::Lzma,
        Codec::Zstd,
        Codec::Flac,
    ];
    let mut out = Vec::new();
    for codec in codecs.map(Some).into_iter().chain([None]) {
        let mut spec = Spec::new(&format!("codec {codec:?}"), three_tracks());
        spec.codecs = codec.into_iter().collect();
        spec.pick = Pick::Cycle;
        let (bytes, w) = to_vec(&spec).expect("write");
        out.push((codec, bytes, w));
    }
    out
}

#[test]
fn every_codec_rebuilds_the_source_tracks() {
    for (codec, bytes, w) in every_codec_images() {
        if let Some(c) = codec {
            assert!(used(&w, c) > 0, "{c:?} is used");
        }
        if codec == Some(Codec::Flac) {
            assert!(w.flac_little > 0, "some flac hunks are little-endian");
            assert!(w.flac_little < used(&w, Codec::Flac), "and some big-endian");
        }
        assert_eq!(decode(&bytes).expect("decode"), w.tracks, "{codec:?}");
        let sizes = our_tracks(&bytes)
            .iter()
            .map(|t| u64::from(t.1) * 2352)
            .collect::<Vec<_>>();
        assert_eq!(sizes, w.tracks.iter().map(|t| t.size).collect::<Vec<_>>());
    }
}

#[test]
fn every_codec_stays_within_the_decode_budget() {
    for (codec, bytes, _) in every_codec_images() {
        let h = chd::read_header(&bytes[..]).expect("header");
        let budget = chd::decode_budget(&h);
        let mut c = Cursor::new(&bytes[..]);
        let layout = chd::read_layout(&mut c, &h).expect("layout");
        let mut d = Decoder::new(c, h, layout).expect("decoder");
        while d.step(1).expect("step") != Step::Done {
            assert!(d.heap_bytes() <= budget, "{codec:?}");
        }
    }
}

fn default_mix(fpb: u32) -> Spec {
    let mut spec = Spec::new(
        &format!("default mix {fpb}"),
        vec![
            track(Kind::Mode1Raw, 61, 0),
            track(Kind::Audio, 70, 60),
            track(Kind::Mode2Raw, 30, 2),
            track(Kind::Audio, 45, 0),
        ],
    );
    spec.frames_per_hunk = fpb;
    spec.subcode = true;
    spec.bad_ecc_every = Some(7);
    spec
}

#[test]
fn chdman_default_mix() {
    for fpb in [8, 27] {
        let (bytes, w) = to_vec(&default_mix(fpb)).expect("write");
        assert_eq!(
            decode(&bytes).expect("decode"),
            w.tracks,
            "{fpb} frames a hunk"
        );
        assert!(
            used(&w, Codec::CdFlac) > 0,
            "FIXED FLAC wins on audio at {fpb}"
        );
        assert!(used(&w, Codec::CdLzma) + used(&w, Codec::CdZlib) > 0);
    }
}

#[test]
fn chd_rs_agrees() {
    for (_, bytes, w) in every_codec_images() {
        agrees_with_chd_rs(&bytes, &w);
    }
    for fpb in [8, 27] {
        let (bytes, w) = to_vec(&default_mix(fpb)).expect("write");
        agrees_with_chd_rs(&bytes, &w);
    }
}

#[test]
fn ruzstd_writes_windowed_frames_without_a_content_size() {
    let data: Vec<u8> = (0..20_000u32).map(|i| (i * 7 % 251) as u8).collect();
    let frame =
        ruzstd::encoding::compress_to_vec(&data[..], ruzstd::encoding::CompressionLevel::Fastest);
    assert_eq!(&frame[..4], &[0x28, 0xb5, 0x2f, 0xfd]);
    assert_eq!(frame[4] & 0xe0, 0, "multi-segment, no content size");
    assert_eq!(frame[5], 7 << 3, "a 128 KiB window");
}

fn arb_kind() -> impl Strategy<Value = Kind> {
    prop_oneof![
        Just(Kind::Mode1Raw),
        Just(Kind::Mode2Raw),
        Just(Kind::Audio)
    ]
}

fn arb_spec() -> impl Strategy<Value = Spec> {
    let tracks = proptest::collection::vec((arb_kind(), 1u32..=40, 0u32..=8), 1..=6);
    let codecs = proptest::sample::subsequence(
        vec![Codec::CdLzma, Codec::CdZlib, Codec::CdZstd, Codec::CdFlac],
        1..=4,
    );
    (tracks, 1u32..=16, codecs, any::<[bool; 5]>(), 0u32..1000).prop_map(
        |(tracks, fpb, codecs, flags, seed)| {
            let tracks = tracks
                .into_iter()
                .map(|(kind, frames, pregap)| track(kind, frames, pregap))
                .collect();
            let mut spec = Spec::new(&format!("prop {seed}"), tracks);
            spec.frames_per_hunk = fpb;
            spec.codecs = codecs;
            spec.pick = if flags[0] {
                Pick::Smallest
            } else {
                Pick::Cycle
            };
            spec.tree = if flags[1] { Tree::Built } else { Tree::Uniform };
            spec.subcode = flags[2];
            spec.self_refs = flags[3];
            spec.rle_types = flags[4];
            spec
        },
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]
    #[test]
    fn track_layouts(spec in arb_spec()) {
        let (bytes, w) = to_vec(&spec).expect("write");
        prop_assert_eq!(decode(&bytes).expect("decode"), w.tracks.clone());
        let h = chd::read_header(&bytes[..]).expect("header");
        let layout = chd::read_layout(&mut Cursor::new(&bytes), &h).expect("layout");
        let want: Vec<u64> = w.tracks.iter().map(|t| t.size).collect();
        prop_assert_eq!(layout.track_sizes(), want);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]
    #[test]
    fn chd_rs_agrees_on_random_layouts(spec in arb_spec()) {
        let (bytes, w) = to_vec(&spec).expect("write");
        agrees_with_chd_rs(&bytes, &w);
    }
}

fn small(label: &str) -> Spec {
    Spec::new(
        label,
        vec![track(Kind::Mode1Raw, 6, 0), track(Kind::Audio, 5, 2)],
    )
}

fn layout_reason(bytes: &[u8]) -> Option<Unidentifiable> {
    chd::read_header(bytes)
        .and_then(|h| chd::read_layout(&mut Cursor::new(bytes), &h))
        .err()
        .and_then(|e| e.reason())
}

#[test]
fn not_identifiable_cases() {
    let mut cooked = small("cooked");
    cooked.tracks[0].kind = Kind::Mode1Cooked;
    assert_eq!(
        layout_reason(&to_vec(&cooked).expect("w").0),
        Some(Unidentifiable::Cooked)
    );

    let mut virtual_pregap = small("virtual");
    virtual_pregap.tracks[1].pregap_stored = false;
    assert_eq!(
        layout_reason(&to_vec(&virtual_pregap).expect("w").0),
        Some(Unidentifiable::PregapMissing)
    );

    let mut parent = small("parent");
    parent.parent = Some([9; 20]);
    assert_eq!(
        reason(&to_vec(&parent).expect("w").0),
        Some(Unidentifiable::Parent)
    );

    let mut huff = small("huff");
    huff.codecs = vec![Codec::CdZlib, Codec::Huff];
    huff.pick = Pick::Cycle;
    huff.self_refs = false;
    assert_eq!(
        reason(&to_vec(&huff).expect("w").0),
        Some(Unidentifiable::Codec)
    );

    for (tag, want) in [
        (b"CHGD", Unidentifiable::GdRom),
        (b"CHTR", Unidentifiable::OldLayout),
    ] {
        let mut spec = small("tag");
        spec.track_tag = *tag;
        assert_eq!(layout_reason(&to_vec(&spec).expect("w").0), Some(want));
    }

    let mut unit = to_vec(&small("unit")).expect("w").0;
    unit[60..64].copy_from_slice(&512u32.to_be_bytes());
    assert_eq!(reason(&unit), Some(Unidentifiable::NotCd));
}

/// Offset of the first stored hunk, from the map header.
fn first_hunk(bytes: &[u8]) -> usize {
    let map =
        usize::try_from(u64::from_be_bytes(bytes[40..48].try_into().expect("8"))).expect("fits");
    let mut first = [0u8; 8];
    first[2..].copy_from_slice(&bytes[map + 4..map + 10]);
    usize::try_from(u64::from_be_bytes(first)).expect("fits")
}

#[test]
fn damaged_images_fail_without_panicking() {
    let mut spec = small("damage");
    spec.codecs = vec![Codec::CdZlib];
    spec.pick = Pick::Cycle;
    let (good, w) = to_vec(&spec).expect("write");
    assert_eq!(decode(&good).expect("decode"), w.tracks);

    let mut complen = good.clone();
    let at = first_hunk(&good) + 1;
    complen[at..at + 2].copy_from_slice(&[0xff, 0xff]);
    assert!(reason(&complen).is_some());

    let mut map_crc = good.clone();
    let map =
        usize::try_from(u64::from_be_bytes(good[40..48].try_into().expect("8"))).expect("fits");
    map_crc[map + 10] ^= 1;
    assert_eq!(reason(&map_crc), Some(Unidentifiable::Corrupt));

    let mut looped = good.clone();
    looped[132..140].copy_from_slice(&124u64.to_be_bytes());
    assert_eq!(layout_reason(&looped), Some(Unidentifiable::Corrupt));

    let mut zs = small("zstd window");
    zs.codecs = vec![Codec::CdZstd];
    zs.pick = Pick::Cycle;
    zs.self_refs = false;
    let (mut wide, _) = to_vec(&zs).expect("write");
    let frame = first_hunk(&wide) + 1 + 2;
    assert_eq!(&wide[frame..frame + 4], &[0x28, 0xb5, 0x2f, 0xfd]);
    assert_eq!(wide[frame + 4] & 0x20, 0, "a windowed frame");
    wide[frame + 5] = 16 << 3;
    assert_eq!(reason(&wide), Some(Unidentifiable::Corrupt));

    for len in (0..good.len()).step_by(997) {
        assert!(decode(&good[..len]).is_err(), "truncated at {len}");
    }

    mutations_fail_or_keep_the_hashes(&good, &w, "cdzl");
}

/// Flips a random byte in each of 256 copies of `good`, one image per codec.
#[test]
fn every_codec_survives_mutations() {
    for (codec, bytes, w) in every_codec_images() {
        mutations_fail_or_keep_the_hashes(&bytes, &w, &format!("{codec:?}"));
    }
}

fn mutations_fail_or_keep_the_hashes(good: &[u8], w: &Written, label: &str) {
    let mut rng = mistarr_fixture::rng::SplitMix::from_label(&format!("mutations {label}"));
    for _ in 0..256 {
        let mut bad = good.to_vec();
        let at = usize::try_from(rng.next_u64() % good.len() as u64).expect("fits");
        bad[at] ^= u8::try_from(rng.next_u64() % 255 + 1).expect("byte");
        if let Ok(tracks) = decode(&bad) {
            assert_eq!(
                tracks, w.tracks,
                "{label}: a mutation at {at} decoded to other data"
            );
        }
    }
}

/// Fails any read that reaches past byte 124.
struct Fence<'a>(&'a [u8], usize);

impl Read for Fence<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.1 + buf.len() > chd::HEADER_LEN {
            return Err(std::io::Error::other("read past the header"));
        }
        buf.copy_from_slice(&self.0[self.1..self.1 + buf.len()]);
        self.1 += buf.len();
        Ok(buf.len())
    }
}

#[test]
fn header_only_reads_124_bytes() {
    let (bytes, w) = to_vec(&small("fence")).expect("write");
    let h = chd::read_header(Fence(&bytes, 0)).expect("header");
    assert_eq!(h.id(w.size).sha1.0, w.sha1);
    assert_eq!(h.raw_sha1.0, w.raw_sha1);
}

#[test]
fn step_size_does_not_change_the_hashes() {
    let (bytes, w) = to_vec(&default_mix(8)).expect("write");
    for k in [1, 3, 1000] {
        assert_eq!(decode_steps(&bytes, k).expect("decode"), w.tracks);
    }
}
