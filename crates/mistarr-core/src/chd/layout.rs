//! The metadata chain and the track rebuild rules; see `docs/CHD.md` "Metadata" and "Tracks".

use std::io::{Read, Seek, SeekFrom};

use sha1::Digest as _;

use super::header::{FourCc, Header};
use super::{corrupt, fail, ChdError, Unidentifiable, FRAME_BYTES, MAX_TRACKS, SECTOR_BYTES};

/// Most metadata entries walked.
const MAX_ENTRIES: usize = 128;
/// Largest payload of a CD track entry.
const MAX_CD_PAYLOAD: u32 = 4096;
/// Most bytes of other metadata hashed for the combined SHA1.
const MAX_OTHER_BYTES: u64 = 64 << 10;

/// How a track's sectors are stored.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackKind {
    /// Mode 1, 2352-byte raw sectors.
    Mode1Raw,
    /// Mode 2, 2352-byte raw sectors.
    Mode2Raw,
    /// CD audio, stored big-endian and rebuilt little-endian.
    Audio,
}

/// One track and where its frames lie in the image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    /// Track number, 1-based.
    pub number: u8,
    /// Sector format.
    pub kind: TrackKind,
    /// First frame of the track in the image, pad frames of earlier tracks included.
    pub start_frame: u64,
    /// Frames in the track, a stored pregap included.
    pub frames: u32,
}

impl Track {
    /// Bytes of the rebuilt `.bin`: 2352 per frame.
    ///
    /// ```
    /// use mistarr_core::chd::{Track, TrackKind};
    /// let t = Track { number: 1, kind: TrackKind::Mode1Raw, start_frame: 0, frames: 2 };
    /// assert_eq!(t.bytes(), 4704);
    /// ```
    #[must_use]
    pub fn bytes(&self) -> u64 {
        u64::from(self.frames) * u64::from(SECTOR_BYTES)
    }
}

/// The tracks of a CD image and the checksummed metadata records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    tracks: Vec<Track>,
    /// `(tag, sha1(payload))` of every metadata entry flagged for the combined SHA1.
    checksums: Vec<[u8; 24]>,
}

impl Layout {
    /// The tracks in order.
    #[must_use]
    pub fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    /// Each track's rebuilt `.bin` size in bytes.
    #[must_use]
    pub fn track_sizes(&self) -> Vec<u64> {
        self.tracks.iter().map(Track::bytes).collect()
    }

    pub(crate) fn checksums(&self) -> &[[u8; 24]] {
        &self.checksums
    }
}

/// Frames of zero padding after a track of `n` frames.
pub(crate) fn pad4(n: u32) -> u32 {
    (4 - n % 4) % 4
}

/// Reads the metadata chain and applies the layout rules of `docs/CHD.md` "Tracks".
///
/// # Errors
///
/// [`ChdError::Unidentifiable`] naming why the tracks cannot be rebuilt, or
/// [`ChdError::Io`] when the reader fails.
pub fn read_layout<R: Read + Seek>(r: &mut R, h: &Header) -> Result<Layout, ChdError> {
    let file_size = r.seek(SeekFrom::End(0))?;
    let mut cd: Vec<(FourCc, Vec<u8>)> = Vec::new();
    let mut checksums = Vec::new();
    let mut seen = Vec::new();
    let mut other = 0u64;
    let mut at = h.meta_offset;
    while at != 0 {
        if seen.len() == MAX_ENTRIES {
            return Err(corrupt("more than 128 metadata entries"));
        }
        if seen.contains(&at) {
            return Err(corrupt("the metadata chain loops"));
        }
        if at.checked_add(16).is_none_or(|end| end > file_size) {
            return Err(corrupt("a metadata entry lies past the end of the file"));
        }
        seen.push(at);
        r.seek(SeekFrom::Start(at))?;
        let mut head = [0u8; 16];
        r.read_exact(&mut head)?;
        let tag = FourCc([head[0], head[1], head[2], head[3]]);
        let flags = head[4];
        let len = u32::from_be_bytes([0, head[5], head[6], head[7]]);
        let next = u64::from_be_bytes([
            head[8], head[9], head[10], head[11], head[12], head[13], head[14], head[15],
        ]);
        let mut sha = sha1::Sha1::new();
        if is_cd_tag(tag) {
            if len > MAX_CD_PAYLOAD {
                return Err(corrupt("a track entry is over 4096 bytes"));
            }
            let mut payload = vec![0u8; len as usize];
            r.read_exact(&mut payload)?;
            sha.update(&payload);
            cd.push((tag, payload));
        } else {
            other += u64::from(len);
            if other > MAX_OTHER_BYTES {
                return Err(corrupt("over 64 KiB of other metadata"));
            }
            let mut left = u64::from(len);
            let mut buf = [0u8; 4096];
            while left > 0 {
                let n = usize::try_from(left.min(4096)).unwrap_or(4096);
                r.read_exact(&mut buf[..n])?;
                sha.update(&buf[..n]);
                left -= n as u64;
            }
            cd.push((tag, Vec::new()));
        }
        if flags & 1 != 0 {
            let mut rec = [0u8; 24];
            rec[..4].copy_from_slice(&tag.0);
            rec[4..].copy_from_slice(&sha.finalize());
            checksums.push(rec);
        }
        at = next;
    }
    let tracks = tracks_from(&cd, h)?;
    Ok(Layout { tracks, checksums })
}

fn is_cd_tag(tag: FourCc) -> bool {
    matches!(
        &tag.0,
        b"CHT2" | b"CHTR" | b"CHCD" | b"CHGD" | b"CHGT" | b"CHSE"
    )
}

/// The tracks the entries describe, or the reason they cannot be rebuilt.
fn tracks_from(entries: &[(FourCc, Vec<u8>)], h: &Header) -> Result<Vec<Track>, ChdError> {
    let has = |t: &[u8; 4]| entries.iter().any(|(tag, _)| &tag.0 == t);
    if has(b"CHGD") || has(b"CHGT") {
        return Err(fail(Unidentifiable::GdRom, "GD-ROM track entries"));
    }
    if !has(b"CHT2") {
        if has(b"CHTR") || has(b"CHCD") {
            return Err(fail(Unidentifiable::OldLayout, "pre-CHT2 track entries"));
        }
        return Err(fail(Unidentifiable::NotCd, "no CD track entries"));
    }
    let texts: Vec<&[u8]> = entries
        .iter()
        .filter(|(tag, _)| &tag.0 == b"CHT2")
        .map(|(_, p)| p.as_slice())
        .collect();
    if texts.len() > MAX_TRACKS {
        return Err(corrupt("more than 99 tracks"));
    }
    let mut tracks = Vec::with_capacity(texts.len());
    let mut start = 0u64;
    for (i, text) in texts.iter().enumerate() {
        let number = u8::try_from(i + 1).map_err(|_| corrupt("track number"))?;
        let t = parse_cht2(text, number)?;
        tracks.push(Track {
            number,
            kind: t.kind,
            start_frame: start,
            frames: t.frames,
        });
        start += u64::from(t.frames) + u64::from(pad4(t.frames));
    }
    if start.checked_mul(u64::from(FRAME_BYTES)) != Some(h.logical_bytes) {
        return Err(corrupt("the tracks do not cover the image"));
    }
    Ok(tracks)
}

struct Cht2 {
    kind: TrackKind,
    frames: u32,
}

/// Parses one CHT2 entry for track `number`, applying the type and pregap rules.
fn parse_cht2(payload: &[u8], number: u8) -> Result<Cht2, ChdError> {
    let body = payload.strip_suffix(&[0]).unwrap_or(payload);
    let text = std::str::from_utf8(body).map_err(|_| corrupt(format!("track {number} text")))?;
    let field = |key: &str| {
        text.split_ascii_whitespace()
            .filter_map(|tok| tok.split_once(':'))
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v)
    };
    let num = |key: &str| -> Result<u32, ChdError> {
        field(key)
            .map_or(Ok(0), str::parse::<u32>)
            .map_err(|_| corrupt(format!("track {number} {key}")))
    };
    if field("TRACK").and_then(|v| v.parse::<u32>().ok()) != Some(u32::from(number)) {
        return Err(corrupt(format!("track {number} is numbered out of order")));
    }
    let kind = match field("TYPE").unwrap_or("") {
        "MODE1_RAW" | "MODE1/2352" => TrackKind::Mode1Raw,
        "MODE2_RAW" | "MODE2/2352" | "CDI/2352" => TrackKind::Mode2Raw,
        "AUDIO" | "CDG" => TrackKind::Audio,
        "MODE1" | "MODE1/2048" | "MODE2_FORM1" | "MODE2/2048" | "MODE2_FORM2" | "MODE2/2324"
        | "MODE2" | "MODE2_FORM_MIX" | "MODE2/2336" => {
            return Err(fail(
                Unidentifiable::Cooked,
                format!("track {number} is cooked"),
            ));
        }
        _ => return Err(corrupt(format!("track {number} has an unknown type"))),
    };
    let frames = num("FRAMES")?;
    if frames == 0 {
        return Err(corrupt(format!("track {number} has no frames")));
    }
    let stored = field("PGTYPE").is_some_and(|v| v.starts_with('V'));
    if number > 1 && num("PREGAP")? > 0 && !stored {
        return Err(fail(
            Unidentifiable::PregapMissing,
            format!("track {number} pregap"),
        ));
    }
    Ok(Cht2 { kind, frames })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::chd::header::tests::sample;
    use crate::chd::read_header;
    use proptest::prelude::*;
    use std::io::Cursor;

    /// A file of a header for `frames` frames, then the entries chained from offset 124.
    pub(crate) fn with_meta(frames: u64, entries: &[(&[u8; 4], u8, &[u8])]) -> (Header, Vec<u8>) {
        let h = read_header(&sample(frames, 8)[..]).expect("header");
        let mut file = sample(frames, 8).to_vec();
        for (i, (tag, flags, payload)) in entries.iter().enumerate() {
            let next = if i + 1 == entries.len() {
                0
            } else {
                file.len() as u64 + 16 + payload.len() as u64
            };
            file.extend_from_slice(*tag);
            file.push(*flags);
            file.extend_from_slice(&u32::try_from(payload.len()).expect("len").to_be_bytes()[1..]);
            file.extend_from_slice(&next.to_be_bytes());
            file.extend_from_slice(payload);
        }
        (h, file)
    }

    fn layout(frames: u64, texts: &[&str]) -> Result<Layout, ChdError> {
        let payloads: Vec<Vec<u8>> = texts
            .iter()
            .map(|t| {
                let mut v = t.as_bytes().to_vec();
                v.push(0);
                v
            })
            .collect();
        let entries: Vec<(&[u8; 4], u8, &[u8])> = payloads
            .iter()
            .map(|p| (b"CHT2", 1u8, p.as_slice()))
            .collect();
        let (h, file) = with_meta(frames, &entries);
        read_layout(&mut Cursor::new(file), &h)
    }

    fn reason(r: Result<Layout, ChdError>) -> Option<Unidentifiable> {
        r.err().and_then(|e| e.reason())
    }

    /// The docs/CHD.md example: a Mode 1 track and an audio track with a stored pregap.
    #[test]
    fn chdman_cht2_example() {
        let l = layout(
            1004 + 1352,
            &[
                "TRACK:1 TYPE:MODE1_RAW SUBTYPE:NONE FRAMES:1001 PREGAP:0 PGTYPE:MODE1 PGSUB:RW POSTGAP:0",
                "TRACK:2 TYPE:AUDIO SUBTYPE:NONE FRAMES:1350 PREGAP:150 PGTYPE:VAUDIO PGSUB:RW POSTGAP:0",
            ],
        )
        .expect("layout");
        assert_eq!(
            l.tracks(),
            [
                Track {
                    number: 1,
                    kind: TrackKind::Mode1Raw,
                    start_frame: 0,
                    frames: 1001
                },
                Track {
                    number: 2,
                    kind: TrackKind::Audio,
                    start_frame: 1004,
                    frames: 1350
                },
            ]
        );
        assert_eq!(l.track_sizes(), [2_354_352, 3_175_200]);
        assert_eq!(l.checksums().len(), 2);
    }

    #[test]
    fn type_aliases_and_padding() {
        let l = layout(
            4 + 4 + 8,
            &[
                "TRACK:1 TYPE:MODE1/2352 FRAMES:3",
                "TRACK:2 TYPE:CDI/2352 FRAMES:4",
                "TRACK:3 TYPE:CDG FRAMES:5",
            ],
        )
        .expect("layout");
        let kinds: Vec<_> = l.tracks().iter().map(|t| (t.kind, t.start_frame)).collect();
        assert_eq!(
            kinds,
            [
                (TrackKind::Mode1Raw, 0),
                (TrackKind::Mode2Raw, 4),
                (TrackKind::Audio, 8)
            ]
        );
        assert_eq!((pad4(1), pad4(4), pad4(7)), (3, 0, 1));
    }

    #[test]
    fn pregap_rules() {
        let track1_virtual = layout(4, &["TRACK:1 TYPE:AUDIO FRAMES:4 PREGAP:150 PGTYPE:AUDIO"]);
        assert!(
            track1_virtual.is_ok(),
            "a virtual track-1 pregap is harmless"
        );
        let track2_virtual = layout(
            8,
            &[
                "TRACK:1 TYPE:MODE1_RAW FRAMES:4",
                "TRACK:2 TYPE:AUDIO FRAMES:4 PREGAP:150 PGTYPE:AUDIO",
            ],
        );
        assert_eq!(reason(track2_virtual), Some(Unidentifiable::PregapMissing));
        let track2_stored = layout(
            8,
            &[
                "TRACK:1 TYPE:MODE1_RAW FRAMES:4",
                "TRACK:2 TYPE:AUDIO FRAMES:4 PREGAP:2 PGTYPE:VAUDIO",
            ],
        );
        assert!(track2_stored.is_ok());
    }

    #[test]
    fn bad_track_lists_have_their_reasons() {
        assert_eq!(
            reason(layout(4, &["TRACK:1 TYPE:MODE1 FRAMES:4"])),
            Some(Unidentifiable::Cooked)
        );
        assert_eq!(
            reason(layout(4, &["TRACK:1 TYPE:MODE2/2336 FRAMES:4"])),
            Some(Unidentifiable::Cooked)
        );
        assert_eq!(
            reason(layout(4, &["TRACK:1 TYPE:WEIRD FRAMES:4"])),
            Some(Unidentifiable::Corrupt)
        );
        assert_eq!(
            reason(layout(8, &["TRACK:1 TYPE:AUDIO FRAMES:4"])),
            Some(Unidentifiable::Corrupt)
        );
        assert_eq!(
            reason(layout(4, &["TRACK:2 TYPE:AUDIO FRAMES:4"])),
            Some(Unidentifiable::Corrupt)
        );
        assert_eq!(
            reason(layout(4, &["TRACK:1 TYPE:AUDIO FRAMES:0"])),
            Some(Unidentifiable::Corrupt)
        );
        assert_eq!(
            reason(layout(4, &["TRACK:1 TYPE:AUDIO FRAMES:x"])),
            Some(Unidentifiable::Corrupt)
        );
    }

    #[test]
    fn tags_decide_the_reason() {
        let one = |tag: &[u8; 4]| {
            let (h, file) = with_meta(4, &[(tag, 1, b"TRACK:1 TYPE:AUDIO FRAMES:4\0")]);
            reason(read_layout(&mut Cursor::new(file), &h))
        };
        assert_eq!(one(b"CHGD"), Some(Unidentifiable::GdRom));
        assert_eq!(one(b"CHGT"), Some(Unidentifiable::GdRom));
        assert_eq!(one(b"CHTR"), Some(Unidentifiable::OldLayout));
        assert_eq!(one(b"CHCD"), Some(Unidentifiable::OldLayout));
        assert_eq!(one(b"GDDD"), Some(Unidentifiable::NotCd));
        assert_eq!(one(b"DVD "), Some(Unidentifiable::NotCd));
        assert_eq!(one(b"CHT2"), None);
        let (h, file) = with_meta(
            4,
            &[
                (b"CHSE", 1, b"SESSION:1\0"),
                (b"CHT2", 1, b"TRACK:1 TYPE:AUDIO FRAMES:4\0"),
                (b"XTRA", 0, b"anything"),
            ],
        );
        let l = read_layout(&mut Cursor::new(file), &h).expect("sessions are accepted");
        assert_eq!((l.tracks().len(), l.checksums().len()), (1, 2));
    }

    #[test]
    fn a_looping_long_or_oversized_chain_is_corrupt() {
        let (h, mut file) = with_meta(4, &[(b"CHT2", 1, b"TRACK:1 TYPE:AUDIO FRAMES:4\0")]);
        file[132..140].copy_from_slice(&124u64.to_be_bytes());
        assert_eq!(
            reason(read_layout(&mut Cursor::new(file), &h)),
            Some(Unidentifiable::Corrupt)
        );

        let text = b"TRACK:1 TYPE:AUDIO FRAMES:4\0";
        let many: Vec<(&[u8; 4], u8, &[u8])> =
            (0..129).map(|_| (b"XTRA", 0u8, &text[..])).collect();
        let (h, file) = with_meta(4, &many);
        assert_eq!(
            reason(read_layout(&mut Cursor::new(file), &h)),
            Some(Unidentifiable::Corrupt)
        );

        let big = vec![b' '; 5000];
        let (h, file) = with_meta(4, &[(b"CHT2", 1, &big)]);
        assert_eq!(
            reason(read_layout(&mut Cursor::new(file), &h)),
            Some(Unidentifiable::Corrupt)
        );

        let (h, mut file) = with_meta(4, &[(b"CHT2", 1, b"TRACK:1 TYPE:AUDIO FRAMES:4\0")]);
        file.truncate(150);
        assert_eq!(
            reason(read_layout(&mut Cursor::new(file), &h)),
            Some(Unidentifiable::Corrupt)
        );
    }

    proptest! {
        #[test]
        fn cht2_text_never_panics(text in "\\PC{0,120}", n in 1u8..5) {
            let _ = parse_cht2(text.as_bytes(), n);
        }

        #[test]
        fn key_value_text_never_panics(frames in any::<u32>(), pregap in any::<u32>(),
                                       ty in "[A-Z0-9_/]{0,14}", pg in "[A-Z]{0,8}") {
            let text = format!("TRACK:2 TYPE:{ty} FRAMES:{frames} PREGAP:{pregap} PGTYPE:{pg}");
            let _ = parse_cht2(text.as_bytes(), 2);
        }
    }
}
