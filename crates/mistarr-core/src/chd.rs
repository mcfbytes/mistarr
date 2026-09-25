//! CHD v5 CD images: header, track layout and a streaming track decoder; see `docs/CHD.md`.

mod bits;
mod cdrom;
mod codec;
mod decode;
mod header;
mod huffman;
mod layout;
mod map;

use std::io;

pub use cdrom::{crc16, ecc};
pub use decode::{decode_budget, Decoder, Step};
pub use header::{read_header, ChdId, FourCc, Header, Sha1Digest};
pub use layout::{read_layout, Layout, Track, TrackKind};

/// Length of a v5 header, the only bytes the scan reads with track hashing off.
pub const HEADER_LEN: usize = 124;
/// Bytes in one CD frame as a CHD stores it: sector data plus subcode.
pub const FRAME_BYTES: u32 = 2448;
/// Bytes of sector data in one frame, the unit a track `.bin` is made of.
pub const SECTOR_BYTES: u32 = 2352;
/// Version of this decoder; a stored failure from an older one is retried.
pub const DECODER_VERSION: u32 = 1;
/// Largest hunk accepted: 214 frames, within MAME's 512 KiB limit.
pub const MAX_HUNK_BYTES: u32 = 214 * FRAME_BYTES;
/// Most frames a CD image may hold: 99 minutes plus track padding and a margin.
pub const MAX_FRAMES: u64 = 450_000;
/// Most tracks on a CD.
pub const MAX_TRACKS: usize = 99;
/// Largest zstd window accepted, counted in [`decode_budget`].
pub const ZSTD_MAX_WINDOW: u64 = 8 << 20;

/// Why a CHD cannot be identified by its tracks. Each maps to a stable `files.reason` code.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Unidentifiable {
    /// The file does not start with the CHD magic.
    NotChd,
    /// A CHD version other than 5.
    Version,
    /// The image needs a parent CHD.
    Parent,
    /// Not a CD image (hard disk, DVD, A/V, or no track list).
    NotCd,
    /// More frames than any CD holds.
    TooLarge,
    /// A hunk uses a compression type this decoder does not read.
    Codec,
    /// A GD-ROM image.
    GdRom,
    /// The track list is in a pre-CHT2 format.
    OldLayout,
    /// A track is stored as cooked sectors, so its raw bytes cannot be rebuilt.
    Cooked,
    /// A pregap after track 1 is not stored in the image.
    PregapMissing,
    /// The image is damaged, truncated or inconsistent.
    Corrupt,
    /// The decoded data do not match a checksum the image carries.
    Checksum,
}

const REASONS: [(Unidentifiable, &str); 12] = [
    (Unidentifiable::NotChd, "not_chd"),
    (Unidentifiable::Version, "version"),
    (Unidentifiable::Parent, "parent"),
    (Unidentifiable::NotCd, "not_cd"),
    (Unidentifiable::TooLarge, "too_large"),
    (Unidentifiable::Codec, "codec"),
    (Unidentifiable::GdRom, "gdrom"),
    (Unidentifiable::OldLayout, "old_layout"),
    (Unidentifiable::Cooked, "cooked"),
    (Unidentifiable::PregapMissing, "pregap"),
    (Unidentifiable::Corrupt, "corrupt"),
    (Unidentifiable::Checksum, "checksum"),
];

impl Unidentifiable {
    /// The stable code stored in `files.reason` and `chd_failures.reason`.
    ///
    /// ```
    /// use mistarr_core::chd::Unidentifiable;
    /// assert_eq!(Unidentifiable::PregapMissing.code(), "pregap");
    /// ```
    #[must_use]
    pub fn code(self) -> &'static str {
        REASONS
            .iter()
            .find(|(r, _)| *r == self)
            .map_or("corrupt", |(_, c)| c)
    }

    /// The reason a code stands for, `None` for an unknown code.
    ///
    /// ```
    /// use mistarr_core::chd::Unidentifiable;
    /// assert_eq!(Unidentifiable::from_code("gdrom"), Some(Unidentifiable::GdRom));
    /// assert_eq!(Unidentifiable::from_code("off"), None);
    /// ```
    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        REASONS.iter().find(|(_, c)| *c == code).map(|(r, _)| *r)
    }
}

/// Failure reading or decoding a CHD. The detail names a field, track or hunk, never a digest.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum ChdError {
    /// The underlying reader failed for a reason other than a short file.
    #[error("cannot read the CHD: {0}")]
    Io(io::Error),
    /// The image cannot be identified by its tracks.
    #[error("{reason:?}: {detail}")]
    Unidentifiable {
        /// Why.
        reason: Unidentifiable,
        /// Where: the field, track or hunk index.
        detail: String,
    },
}

impl ChdError {
    /// The reason when the image itself is at fault, `None` for an I/O failure.
    ///
    /// ```
    /// use mistarr_core::chd::{ChdError, Unidentifiable};
    /// let e = ChdError::Unidentifiable { reason: Unidentifiable::Parent, detail: String::new() };
    /// assert_eq!(e.reason(), Some(Unidentifiable::Parent));
    /// ```
    #[must_use]
    pub fn reason(&self) -> Option<Unidentifiable> {
        match self {
            Self::Io(_) => None,
            Self::Unidentifiable { reason, .. } => Some(*reason),
        }
    }
}

impl From<io::Error> for ChdError {
    fn from(e: io::Error) -> Self {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            corrupt("the file ends early")
        } else {
            Self::Io(e)
        }
    }
}

pub(crate) fn fail(reason: Unidentifiable, detail: impl Into<String>) -> ChdError {
    ChdError::Unidentifiable {
        reason,
        detail: detail.into(),
    }
}

pub(crate) fn corrupt(detail: impl Into<String>) -> ChdError {
    fail(Unidentifiable::Corrupt, detail)
}

/// `buf[off..off + len]`, or `Corrupt` naming `what` when it runs past the end.
pub(crate) fn span<'a>(
    buf: &'a [u8],
    off: usize,
    len: usize,
    what: &str,
) -> Result<&'a [u8], ChdError> {
    off.checked_add(len)
        .and_then(|end| buf.get(off..end))
        .ok_or_else(|| corrupt(format!("{what} runs past its data")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reason_round_trips_through_its_code() {
        for (r, c) in REASONS {
            assert_eq!(r.code(), c);
            assert_eq!(Unidentifiable::from_code(c), Some(r));
        }
        assert_eq!(Unidentifiable::from_code(""), None);
    }

    #[test]
    fn a_short_read_is_corrupt_and_other_io_is_io() {
        let eof: ChdError = io::Error::from(io::ErrorKind::UnexpectedEof).into();
        assert_eq!(eof.reason(), Some(Unidentifiable::Corrupt));
        let other: ChdError = io::Error::other("EIO").into();
        assert_eq!(other.reason(), None);
    }

    #[test]
    fn span_checks_bounds_without_panicking() {
        let buf = [1u8, 2, 3];
        assert_eq!(span(&buf, 1, 2, "x").expect("fits"), &[2, 3]);
        assert!(span(&buf, 2, 2, "x").is_err());
        assert!(span(&buf, usize::MAX, 2, "x").is_err());
    }
}
