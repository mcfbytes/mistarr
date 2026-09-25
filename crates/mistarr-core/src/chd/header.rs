//! The 124-byte v5 header; see `docs/CHD.md` "Header".

use std::io::Read;

use super::{
    corrupt, fail, ChdError, Unidentifiable, FRAME_BYTES, HEADER_LEN, MAX_FRAMES, MAX_HUNK_BYTES,
};

/// A four-character code: a codec id or a metadata tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FourCc(pub [u8; 4]);

/// A SHA1 digest as stored in a CHD header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sha1Digest(pub [u8; 20]);

impl Sha1Digest {
    /// The digest as 40 lowercase hex characters.
    ///
    /// ```
    /// use mistarr_core::chd::Sha1Digest;
    /// let d = Sha1Digest([0xab; 20]);
    /// assert_eq!(&d.to_hex()[..4], "abab");
    /// assert_eq!(Sha1Digest::from_hex(&d.to_hex()), Some(d));
    /// ```
    #[must_use]
    pub fn to_hex(&self) -> String {
        crate::hash::hex(&self.0)
    }

    /// Parses 40 hex characters, either case; `None` for anything else.
    ///
    /// ```
    /// use mistarr_core::chd::Sha1Digest;
    /// assert!(Sha1Digest::from_hex("00").is_none());
    /// ```
    #[must_use]
    pub fn from_hex(s: &str) -> Option<Self> {
        let bytes = s.as_bytes();
        if bytes.len() != 40 {
            return None;
        }
        let mut out = [0u8; 20];
        for (o, pair) in out.iter_mut().zip(bytes.as_chunks::<2>().0) {
            let hi = char::from(pair[0]).to_digit(16)?;
            let lo = char::from(pair[1]).to_digit(16)?;
            *o = u8::try_from(hi * 16 + lo).ok()?;
        }
        Some(Self(out))
    }
}

/// Content identity of a CHD: the header's combined SHA1 and the file size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChdId {
    /// Combined SHA1 of the raw data and checksummed metadata (header offset 84).
    pub sha1: Sha1Digest,
    /// File size in bytes.
    pub size: u64,
}

/// A checked v5 header of a CD image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// Codec slots 0 to 3; `None` is an empty slot.
    pub compressors: [Option<FourCc>; 4],
    /// Bytes of decoded data, a whole number of frames.
    pub logical_bytes: u64,
    /// File offset of the hunk map.
    pub map_offset: u64,
    /// File offset of the first metadata entry, 0 when there is none.
    pub meta_offset: u64,
    /// Bytes per hunk, a whole number of frames.
    pub hunk_bytes: u32,
    /// Bytes per unit: always [`FRAME_BYTES`] for a CD.
    pub unit_bytes: u32,
    /// SHA1 of the decoded data.
    pub raw_sha1: Sha1Digest,
    /// SHA1 of the raw SHA1 and the checksummed metadata.
    pub sha1: Sha1Digest,
}

impl Header {
    /// The identity of a file with this header and `file_size` bytes.
    ///
    /// ```
    /// # use mistarr_core::chd::{Header, Sha1Digest};
    /// # let h = Header { compressors: [None; 4], logical_bytes: 2448, map_offset: 124,
    /// #     meta_offset: 0, hunk_bytes: 2448, unit_bytes: 2448,
    /// #     raw_sha1: Sha1Digest([0; 20]), sha1: Sha1Digest([1; 20]) };
    /// assert_eq!(h.id(500).size, 500);
    /// assert_eq!(h.id(500).sha1, h.sha1);
    /// ```
    #[must_use]
    pub fn id(&self, file_size: u64) -> ChdId {
        ChdId {
            sha1: self.sha1,
            size: file_size,
        }
    }

    /// Hunks in the image, the last one possibly partial.
    ///
    /// ```
    /// # use mistarr_core::chd::{Header, Sha1Digest};
    /// # let h = Header { compressors: [None; 4], logical_bytes: 5 * 2448, map_offset: 124,
    /// #     meta_offset: 0, hunk_bytes: 2 * 2448, unit_bytes: 2448,
    /// #     raw_sha1: Sha1Digest([0; 20]), sha1: Sha1Digest([0; 20]) };
    /// assert_eq!((h.hunk_count(), h.frames_per_hunk()), (3, 2));
    /// ```
    #[must_use]
    pub fn hunk_count(&self) -> u64 {
        self.logical_bytes
            .div_ceil(u64::from(self.hunk_bytes.max(1)))
    }

    /// Frames in one hunk.
    #[must_use]
    pub fn frames_per_hunk(&self) -> u32 {
        self.hunk_bytes / FRAME_BYTES
    }

    /// Whether the image stores hunks uncompressed, with a plain offset map.
    pub(crate) fn uncompressed(&self) -> bool {
        self.compressors[0].is_none()
    }
}

fn be32(b: &[u8], at: usize) -> u32 {
    let mut v = [0u8; 4];
    v.copy_from_slice(&b[at..at + 4]);
    u32::from_be_bytes(v)
}

fn be64(b: &[u8], at: usize) -> u64 {
    let mut v = [0u8; 8];
    v.copy_from_slice(&b[at..at + 8]);
    u64::from_be_bytes(v)
}

fn sha(b: &[u8], at: usize) -> Sha1Digest {
    let mut v = [0u8; 20];
    v.copy_from_slice(&b[at..at + 20]);
    Sha1Digest(v)
}

/// Reads until `buf` is full or the reader ends, returning the bytes read.
fn read_fill<R: Read>(r: &mut R, buf: &mut [u8]) -> Result<usize, ChdError> {
    let mut got = 0;
    while got < buf.len() {
        match r.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(got)
}

/// Reads and checks the v5 header; reads 16 bytes, then the rest only for v5 with length 124.
///
/// # Errors
///
/// [`ChdError::Unidentifiable`] with the reason of `docs/CHD.md` "Header checks", or
/// [`ChdError::Io`] when the reader fails.
///
/// ```
/// use mistarr_core::chd::{read_header, Unidentifiable};
/// let e = read_header(&b"not a chd at all"[..]).unwrap_err();
/// assert_eq!(e.reason(), Some(Unidentifiable::NotChd));
/// ```
pub fn read_header<R: Read>(mut r: R) -> Result<Header, ChdError> {
    let mut b = [0u8; HEADER_LEN];
    let got = read_fill(&mut r, &mut b[..16])?;
    if got < 8 || &b[..8] != b"MComprHD" {
        return Err(fail(Unidentifiable::NotChd, "no CHD magic"));
    }
    if got < 16 {
        return Err(corrupt("the header ends early"));
    }
    let version = be32(&b, 12);
    if version != 5 {
        return Err(fail(Unidentifiable::Version, format!("version {version}")));
    }
    if be32(&b, 8) != 124 {
        return Err(corrupt("header length is not 124"));
    }
    if read_fill(&mut r, &mut b[16..])? < HEADER_LEN - 16 {
        return Err(corrupt("the header ends early"));
    }
    if b[104..124].iter().any(|&x| x != 0) {
        return Err(fail(Unidentifiable::Parent, "a parent SHA1 is set"));
    }
    let unit_bytes = be32(&b, 60);
    if unit_bytes != FRAME_BYTES {
        return Err(fail(
            Unidentifiable::NotCd,
            format!("unit size {unit_bytes}"),
        ));
    }
    let hunk_bytes = be32(&b, 56);
    if hunk_bytes == 0 || !hunk_bytes.is_multiple_of(FRAME_BYTES) || hunk_bytes > MAX_HUNK_BYTES {
        return Err(corrupt(format!("hunk size {hunk_bytes}")));
    }
    let logical_bytes = be64(&b, 32);
    if logical_bytes == 0 || !logical_bytes.is_multiple_of(u64::from(FRAME_BYTES)) {
        return Err(corrupt("logical size is not a whole number of frames"));
    }
    if logical_bytes / u64::from(FRAME_BYTES) > MAX_FRAMES {
        return Err(fail(
            Unidentifiable::TooLarge,
            "more frames than a CD holds",
        ));
    }
    if b[84..104].iter().all(|&x| x == 0) {
        return Err(fail(Unidentifiable::NoChecksum, "the image SHA1 is zero"));
    }
    let mut compressors = [None; 4];
    let mut ended = false;
    for (i, slot) in compressors.iter_mut().enumerate() {
        let raw = [b[16 + i * 4], b[17 + i * 4], b[18 + i * 4], b[19 + i * 4]];
        if raw == [0; 4] {
            ended = true;
        } else if ended {
            return Err(corrupt(format!("codec slot {i} follows an empty slot")));
        } else {
            *slot = Some(FourCc(raw));
        }
    }
    Ok(Header {
        compressors,
        logical_bytes,
        map_offset: be64(&b, 40),
        meta_offset: be64(&b, 48),
        hunk_bytes,
        unit_bytes,
        raw_sha1: sha(&b, 64),
        sha1: sha(&b, 84),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use proptest::prelude::*;

    /// A valid header: `frames` frames of `fpb`-frame hunks, codecs cdlz/cdzl.
    pub(crate) fn sample(frames: u64, fpb: u32) -> [u8; HEADER_LEN] {
        let mut b = [0u8; HEADER_LEN];
        b[..8].copy_from_slice(b"MComprHD");
        b[8..12].copy_from_slice(&124u32.to_be_bytes());
        b[12..16].copy_from_slice(&5u32.to_be_bytes());
        b[16..20].copy_from_slice(b"cdlz");
        b[20..24].copy_from_slice(b"cdzl");
        b[32..40].copy_from_slice(&(frames * 2448).to_be_bytes());
        b[40..48].copy_from_slice(&1000u64.to_be_bytes());
        b[48..56].copy_from_slice(&124u64.to_be_bytes());
        b[56..60].copy_from_slice(&(fpb * 2448).to_be_bytes());
        b[60..64].copy_from_slice(&2448u32.to_be_bytes());
        b[64..84].fill(0x11);
        b[84..104].fill(0x22);
        b
    }

    fn reason(b: &[u8]) -> Option<Unidentifiable> {
        read_header(b).err().and_then(|e| e.reason())
    }

    #[test]
    fn parses_a_v5_header() {
        let h = read_header(&sample(10, 8)[..]).expect("valid");
        assert_eq!(h.compressors[0], Some(FourCc(*b"cdlz")));
        assert_eq!(h.compressors[2], None);
        assert_eq!((h.hunk_bytes, h.logical_bytes), (8 * 2448, 10 * 2448));
        assert_eq!((h.hunk_count(), h.frames_per_hunk()), (2, 8));
        assert_eq!((h.map_offset, h.meta_offset), (1000, 124));
        assert_eq!(h.raw_sha1, Sha1Digest([0x11; 20]));
        assert_eq!(
            h.id(77),
            ChdId {
                sha1: Sha1Digest([0x22; 20]),
                size: 77
            }
        );
        assert!(!h.uncompressed());
    }

    #[test]
    fn a_700_mb_one_frame_per_hunk_header_is_accepted() {
        let h = read_header(&sample(300_000, 1)[..]).expect("valid");
        assert_eq!(h.hunk_count(), 300_000);
    }

    #[test]
    fn each_rejection_has_its_reason() {
        let set = |at: usize, v: &[u8]| {
            let mut b = sample(10, 8);
            b[at..at + v.len()].copy_from_slice(v);
            b
        };
        assert_eq!(reason(b"MComprHX"), Some(Unidentifiable::NotChd));
        assert_eq!(reason(b"MCo"), Some(Unidentifiable::NotChd));
        assert_eq!(reason(&sample(10, 8)[..12]), Some(Unidentifiable::Corrupt));
        assert_eq!(
            reason(&set(12, &4u32.to_be_bytes())),
            Some(Unidentifiable::Version)
        );
        assert_eq!(
            reason(&set(8, &120u32.to_be_bytes())),
            Some(Unidentifiable::Corrupt)
        );
        assert_eq!(reason(&set(110, &[1])), Some(Unidentifiable::Parent));
        assert_eq!(
            reason(&set(60, &512u32.to_be_bytes())),
            Some(Unidentifiable::NotCd)
        );
        assert_eq!(
            reason(&set(56, &0u32.to_be_bytes())),
            Some(Unidentifiable::Corrupt)
        );
        assert_eq!(
            reason(&set(56, &2449u32.to_be_bytes())),
            Some(Unidentifiable::Corrupt)
        );
        let over = (MAX_HUNK_BYTES + FRAME_BYTES).to_be_bytes();
        assert_eq!(reason(&set(56, &over)), Some(Unidentifiable::Corrupt));
        assert_eq!(
            reason(&set(32, &0u64.to_be_bytes())),
            Some(Unidentifiable::Corrupt)
        );
        assert_eq!(
            reason(&set(32, &2449u64.to_be_bytes())),
            Some(Unidentifiable::Corrupt)
        );
        let big = ((MAX_FRAMES + 1) * 2448).to_be_bytes();
        assert_eq!(reason(&set(32, &big)), Some(Unidentifiable::TooLarge));
        assert_eq!(reason(&set(20, &[0; 4])[..]), None, "cdlz alone is fine");
        let mut gap = set(20, &[0; 4]);
        gap[24..28].copy_from_slice(b"cdfl");
        assert_eq!(reason(&gap), Some(Unidentifiable::Corrupt));
        assert_eq!(reason(&sample(10, 8)[..100]), Some(Unidentifiable::Corrupt));
        let unchecked = set(84, &[0; 20]);
        assert_eq!(reason(&unchecked), Some(Unidentifiable::NoChecksum));
        assert_eq!(
            reason(&set(16, &[0; 8])[..]),
            None,
            "an uncompressed image with its SHA1 set is read"
        );
    }

    /// Fails every read that would reach past byte 124.
    struct Fence<'a>(&'a [u8], usize);

    impl Read for Fence<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.1 + buf.len() > HEADER_LEN {
                return Err(std::io::Error::other("read past the header"));
            }
            let n = buf.len().min(self.0.len() - self.1);
            buf[..n].copy_from_slice(&self.0[self.1..self.1 + n]);
            self.1 += n;
            Ok(n)
        }
    }

    #[test]
    fn reads_at_most_the_header() {
        let mut file = sample(10, 8).to_vec();
        file.extend_from_slice(&[0xee; 4096]);
        let h = read_header(Fence(&file, 0)).expect("header only");
        assert_eq!(h.hunk_bytes, 8 * 2448);
    }

    proptest! {
        #[test]
        fn random_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..200)) {
            let _ = read_header(&bytes[..]);
            let mut b = sample(10, 8).to_vec();
            for (i, x) in bytes.iter().enumerate() {
                b[i % HEADER_LEN] ^= x;
            }
            let _ = read_header(&b[..]);
        }
    }
}
