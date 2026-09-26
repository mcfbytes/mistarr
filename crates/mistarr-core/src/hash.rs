//! One-pass CRC32/MD5/SHA1 hashing and the platform header rules of
//! `docs/PLATFORMS.md` "Header rules", plus a zip member pre-check from
//! the central directory. See `docs/VERIFICATION.md` "Hashing".

use std::io::{self, Read, Seek};

use md5::Digest as _;

use crate::HashSet;

/// Streaming buffer size, matching the "Hashing buffer" budget in
/// `docs/ARCHITECTURE.md`.
const BUF_SIZE: usize = 256 * 1024;

/// Header handling applied while hashing, one variant per row of the
/// "Header rules" table in `docs/PLATFORMS.md`.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HeaderRule {
    /// Hash the whole file.
    #[default]
    None,
    /// iNES: skip the 16-byte header when it starts with `NES\x1a`.
    Ines,
    /// SMC copier header: skip 512 bytes when size is `n*1024 + 512`.
    Smc,
    /// Atari 7800: skip the 128-byte header when the `ATARI7800` tag is present.
    A78,
    /// Atari Lynx: skip the 64-byte header when it starts with `LYNX`.
    Lnx,
    /// Nintendo 64: detect byte order from the first four bytes and
    /// normalise to big-endian while hashing.
    N64,
}

impl HeaderRule {
    /// The rule a platform table `header_rule` name stands for; unknown names hash whole files.
    ///
    /// ```
    /// use mistarr_core::hash::HeaderRule;
    /// assert_eq!(HeaderRule::from_name("ines"), HeaderRule::Ines);
    /// assert_eq!(HeaderRule::from_name("none"), HeaderRule::None);
    /// assert_eq!(HeaderRule::from_name("other"), HeaderRule::None);
    /// ```
    #[must_use]
    pub fn from_name(name: &str) -> Self {
        match name {
            "ines" => Self::Ines,
            "smc" => Self::Smc,
            "a78" => Self::A78,
            "lnx" => Self::Lnx,
            "n64" => Self::N64,
            _ => Self::None,
        }
    }

    /// Whether the rule drops a header a headered DAT would include in its hashes.
    ///
    /// ```
    /// use mistarr_core::hash::HeaderRule;
    /// assert!(HeaderRule::Lnx.strips_header());
    /// assert!(!HeaderRule::Smc.strips_header());
    /// ```
    #[must_use]
    pub fn strips_header(self) -> bool {
        matches!(self, Self::Ines | Self::A78 | Self::Lnx)
    }

    /// Bytes of header the rule skips when it finds one, 0 when it skips none.
    ///
    /// ```
    /// use mistarr_core::hash::HeaderRule;
    /// assert_eq!((HeaderRule::Ines.header_len(), HeaderRule::N64.header_len()), (16, 0));
    /// ```
    #[must_use]
    pub fn header_len(self) -> u64 {
        match self {
            Self::Ines => INES_HEADER as u64,
            Self::Smc => SMC_HEADER as u64,
            Self::A78 => A78_HEADER as u64,
            Self::Lnx => LNX_HEADER as u64,
            Self::None | Self::N64 => 0,
        }
    }
}

const INES_HEADER: usize = 16;
const SMC_HEADER: usize = 512;
const A78_HEADER: usize = 128;
const LNX_HEADER: usize = 64;

/// Error reading a zip archive's central directory or one of its members.
#[derive(Debug, thiserror::Error)]
pub enum HashError {
    /// The underlying reader failed.
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    /// The archive is not a valid zip, or the named member is missing.
    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),
}

/// One member of a zip's central directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZipMember {
    /// Member path inside the zip.
    pub name: String,
    /// Uncompressed size in bytes.
    pub size: u64,
    /// Stored CRC32 as 8 lowercase hex characters.
    pub crc32: String,
}

/// CRC32, MD5 and SHA1 fed together, finished into a [`HashSet`].
pub(crate) struct Hashers {
    crc: crc32fast::Hasher,
    md5: md5::Md5,
    sha1: sha1::Sha1,
    len: u64,
}

impl Hashers {
    pub(crate) fn new() -> Self {
        Self {
            crc: crc32fast::Hasher::new(),
            md5: md5::Md5::new(),
            sha1: sha1::Sha1::new(),
            len: 0,
        }
    }

    pub(crate) fn update(&mut self, buf: &[u8]) {
        self.crc.update(buf);
        self.md5.update(buf);
        self.sha1.update(buf);
        self.len += buf.len() as u64;
    }

    pub(crate) fn finish(self) -> HashSet {
        HashSet {
            size: self.len,
            crc32: format!("{:08x}", self.crc.finalize()),
            md5: hex(&self.md5.finalize()),
            sha1: hex(&self.sha1.finalize()),
        }
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// Computes CRC32, MD5 and SHA1 in one streaming pass, applying `rule` to
/// the byte stream first. Never buffers more than a small header probe.
///
/// `size_hint` is the file's total size, when the caller already knows it
/// (filesystem metadata, or a zip member's uncompressed size). `HeaderRule::Smc`
/// uses it to decide in one pass instead of hashing the stream twice.
///
/// # Errors
///
/// Returns an error if reading from `r` fails.
///
/// ```
/// use mistarr_core::hash::{hash_reader, HeaderRule};
/// use std::io::Cursor;
///
/// let hashes = hash_reader(Cursor::new(b"abc"), HeaderRule::None, None).unwrap();
/// assert_eq!(hashes.sha1, "a9993e364706816aba3e25717850c26c9cd0d89d");
/// ```
pub fn hash_reader<R: Read>(r: R, rule: HeaderRule, size_hint: Option<u64>) -> io::Result<HashSet> {
    Ok(hash_under(r, rule, size_hint, false)?.content)
}

/// A payload's hashes in the forms its header rule gives, per
/// `docs/VERIFICATION.md` "Hashing".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderForms {
    /// The content after the rule: header stripped, byte order normalised.
    pub content: HashSet,
    /// The whole payload, header included, when a stripping rule found and
    /// stripped a header; `None` when `content` already is the whole payload
    /// or the rule is not one of [`HeaderRule::strips_header`].
    pub whole: Option<HashSet>,
}

impl HeaderForms {
    /// The whole payload's hashes: [`Self::whole`] when a header was stripped, else [`Self::content`].
    ///
    /// ```
    /// use mistarr_core::hash::{hash_forms, HeaderRule};
    /// let forms = hash_forms(&b"abc"[..], HeaderRule::Ines, None).unwrap();
    /// assert_eq!(forms.whole_or_content(), &forms.content);
    /// ```
    #[must_use]
    pub fn whole_or_content(&self) -> &HashSet {
        self.whole.as_ref().unwrap_or(&self.content)
    }
}

/// [`hash_reader`], also hashing the whole payload in the same pass when a
/// stripping rule finds its header, so a file matches headered and headerless
/// DATs alike. Two hasher sets share the one read buffer.
///
/// # Errors
///
/// Returns an error if reading from `r` fails.
///
/// ```
/// use mistarr_core::hash::{hash_forms, hash_reader, HeaderRule};
///
/// let mut file = b"NES\x1a".to_vec();
/// file.resize(16, 0);
/// file.extend_from_slice(b"abc");
/// let forms = hash_forms(&file[..], HeaderRule::Ines, None).unwrap();
/// assert_eq!(forms.content.sha1, "a9993e364706816aba3e25717850c26c9cd0d89d");
/// let whole = forms.whole.unwrap();
/// assert_eq!(whole, hash_reader(&file[..], HeaderRule::None, None).unwrap());
/// ```
pub fn hash_forms<R: Read>(
    r: R,
    rule: HeaderRule,
    size_hint: Option<u64>,
) -> io::Result<HeaderForms> {
    hash_under(r, rule, size_hint, true)
}

fn hash_under<R: Read>(
    r: R,
    rule: HeaderRule,
    size_hint: Option<u64>,
    both: bool,
) -> io::Result<HeaderForms> {
    let content = match rule {
        HeaderRule::Ines | HeaderRule::A78 | HeaderRule::Lnx => {
            return hash_header_forms(r, rule, both)
        }
        HeaderRule::None => hash_stream(r, &[])?,
        HeaderRule::Smc => hash_smc(r, size_hint)?,
        HeaderRule::N64 => hash_n64(r)?,
    };
    Ok(HeaderForms {
        content,
        whole: None,
    })
}

/// Bytes a stripping rule reads to recognise its header.
fn probe_len(rule: HeaderRule) -> usize {
    match rule {
        HeaderRule::A78 => 10,
        _ => 4,
    }
}

/// Whether `head`, a payload's first bytes, starts with the header `rule` strips.
fn header_found(rule: HeaderRule, head: &[u8]) -> bool {
    match rule {
        HeaderRule::Ines => head.starts_with(b"NES\x1a"),
        HeaderRule::A78 => head.get(1..10) == Some(&b"ATARI7800"[..]),
        HeaderRule::Lnx => head.starts_with(b"LYNX"),
        _ => false,
    }
}

fn hash_stream<R: Read>(mut r: R, prefix: &[u8]) -> io::Result<HashSet> {
    let mut h = Hashers::new();
    h.update(prefix);
    let mut buf = vec![0u8; BUF_SIZE];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finish())
}

fn read_probe<R: Read>(r: &mut R, len: usize) -> io::Result<Vec<u8>> {
    let mut buf = vec![0u8; len];
    let mut read = 0;
    while read < len {
        let n = r.read(&mut buf[read..])?;
        if n == 0 {
            break;
        }
        read += n;
    }
    buf.truncate(read);
    Ok(buf)
}

fn discard<R: Read>(r: &mut R, mut n: usize) -> io::Result<()> {
    let mut buf = [0u8; 4096];
    while n > 0 {
        let want = n.min(buf.len());
        let read = r.read(&mut buf[..want])?;
        if read == 0 {
            break;
        }
        n -= read;
    }
    Ok(())
}

fn hash_header_forms<R: Read>(mut r: R, rule: HeaderRule, both: bool) -> io::Result<HeaderForms> {
    let probe = read_probe(&mut r, probe_len(rule))?;
    let content_only = |content| HeaderForms {
        content,
        whole: None,
    };
    if !header_found(rule, &probe) {
        return hash_stream(r, &probe).map(content_only);
    }
    let header = rule.header_len();
    if !both {
        // Every header is longer than its probe, and at most 128 bytes.
        #[allow(clippy::cast_possible_truncation)]
        discard(&mut r, header as usize - probe.len())?;
        return hash_stream(r, &[]).map(content_only);
    }
    let mut dual = Skipping::new(header);
    dual.update(&probe);
    let mut buf = vec![0u8; BUF_SIZE];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        dual.update(&buf[..n]);
    }
    let (whole, content) = dual.finish();
    Ok(HeaderForms {
        content,
        whole: Some(whole),
    })
}

/// Two hasher sets fed the same bytes: one the whole stream, one all but its first `skip` bytes.
struct Skipping {
    whole: Hashers,
    skipped: Hashers,
    skip: u64,
    pos: u64,
}

impl Skipping {
    fn new(skip: u64) -> Self {
        Self {
            whole: Hashers::new(),
            skipped: Hashers::new(),
            skip,
            pos: 0,
        }
    }

    fn update(&mut self, chunk: &[u8]) {
        self.whole.update(chunk);
        let start = self.pos;
        self.pos += chunk.len() as u64;
        if start >= self.skip {
            self.skipped.update(chunk);
        } else if self.pos > self.skip {
            // start < skip here, so the difference always fits in usize.
            #[allow(clippy::cast_possible_truncation)]
            self.skipped.update(&chunk[(self.skip - start) as usize..]);
        }
    }

    /// The whole stream's hashes, then the skipped stream's.
    fn finish(self) -> (HashSet, HashSet) {
        (self.whole.finish(), self.skipped.finish())
    }
}

fn hash_smc<R: Read>(mut r: R, size_hint: Option<u64>) -> io::Result<HashSet> {
    let header = SMC_HEADER as u64;
    if let Some(size) = size_hint {
        if size % 1024 == header {
            discard(&mut r, SMC_HEADER)?;
        }
        return hash_stream(r, &[]);
    }
    // No size known up front: run whole-file and header-skipped hashers in
    // parallel and pick the right one once the total size is known at EOF.
    let mut dual = Skipping::new(header);
    let mut buf = vec![0u8; BUF_SIZE];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        dual.update(&buf[..n]);
    }
    let total = dual.pos;
    let (whole, skipped) = dual.finish();
    Ok(if total % 1024 == header {
        skipped
    } else {
        whole
    })
}

#[derive(Clone, Copy)]
enum N64Order {
    Big,
    Swap16,
    Swap32,
}

fn n64_variant(probe: &[u8]) -> N64Order {
    match probe {
        [0x37, 0x80, 0x40, 0x12] => N64Order::Swap16,
        [0x40, 0x12, 0x37, 0x80] => N64Order::Swap32,
        // Big-endian z64 magic, and anything unrecognised, pass through unchanged.
        _ => N64Order::Big,
    }
}

fn swap_into(data: &[u8], order: N64Order, out: &mut [u8]) {
    out.copy_from_slice(data);
    match order {
        N64Order::Big => {}
        N64Order::Swap16 => {
            for pair in out.as_chunks_mut::<2>().0 {
                pair.swap(0, 1);
            }
        }
        N64Order::Swap32 => {
            for quad in out.as_chunks_mut::<4>().0 {
                quad.swap(0, 3);
                quad.swap(1, 2);
            }
        }
    }
}

fn hash_n64<R: Read>(mut r: R) -> io::Result<HashSet> {
    let probe = read_probe(&mut r, 4)?;
    let order = n64_variant(&probe);
    if matches!(order, N64Order::Big) {
        return hash_stream(r, &probe);
    }
    let group = if matches!(order, N64Order::Swap16) {
        2
    } else {
        4
    };
    let mut h = Hashers::new();
    let mut carry = probe;
    let mut buf = vec![0u8; BUF_SIZE];
    let mut scratch = vec![0u8; BUF_SIZE + 4];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        carry.extend_from_slice(&buf[..n]);
        let usable = carry.len() - carry.len() % group;
        swap_into(&carry[..usable], order, &mut scratch[..usable]);
        h.update(&scratch[..usable]);
        carry.drain(..usable);
    }
    h.update(&carry);
    Ok(h.finish())
}

/// Lists every member's name, uncompressed size and stored CRC32 from a
/// zip's central directory. Reading a member's data start also requires its
/// local header, so this seeks once per member, but never decompresses or
/// decrypts one: a member with an unsupported compression method or without
/// a password still lists successfully, for the pre-check in
/// `docs/VERIFICATION.md` "Hashing" to mark it `unverified` on CRC alone.
///
/// # Errors
///
/// Returns an error if `r` is not a valid zip archive.
///
/// ```
/// use mistarr_core::hash::zip_members;
/// use std::io::{Cursor, Write};
///
/// let mut buf = Vec::new();
/// let mut zip = zip::ZipWriter::new(Cursor::new(&mut buf));
/// zip.start_file("a.bin", zip::write::SimpleFileOptions::default()).unwrap();
/// zip.write_all(b"abc").unwrap();
/// zip.finish().unwrap();
///
/// let members = zip_members(Cursor::new(buf)).unwrap();
/// assert_eq!(members[0].name, "a.bin");
/// assert_eq!(members[0].size, 3);
/// ```
pub fn zip_members<R: Read + Seek>(r: R) -> Result<Vec<ZipMember>, HashError> {
    let mut archive = zip::ZipArchive::new(r)?;
    let mut members = Vec::with_capacity(archive.len());
    for i in 0..archive.len() {
        // by_index_raw never builds a decompressor or decryptor, so an
        // unsupported method or an encrypted member still yields metadata.
        let file = archive.by_index_raw(i)?;
        members.push(ZipMember {
            name: file.name().to_string(),
            size: file.size(),
            crc32: format!("{:08x}", file.crc32()),
        });
    }
    Ok(members)
}

/// Decompresses one zip member by name and hashes it through [`hash_reader`].
///
/// # Errors
///
/// Returns an error if `r` is not a valid zip archive, `name` is not a
/// member of it, or reading the decompressed data fails.
///
/// ```
/// use mistarr_core::hash::{hash_zip_member, HeaderRule};
/// use std::io::{Cursor, Write};
///
/// let mut buf = Vec::new();
/// let mut zip = zip::ZipWriter::new(Cursor::new(&mut buf));
/// zip.start_file("a.bin", zip::write::SimpleFileOptions::default()).unwrap();
/// zip.write_all(b"abc").unwrap();
/// zip.finish().unwrap();
///
/// let hashes = hash_zip_member(Cursor::new(buf), "a.bin", HeaderRule::None).unwrap();
/// assert_eq!(hashes.sha1, "a9993e364706816aba3e25717850c26c9cd0d89d");
/// ```
pub fn hash_zip_member<R: Read + Seek>(
    r: R,
    name: &str,
    rule: HeaderRule,
) -> Result<HashSet, HashError> {
    let mut archive = zip::ZipArchive::new(r)?;
    let file = archive.by_name(name)?;
    let size_hint = Some(file.size());
    Ok(hash_reader(file, rule, size_hint)?)
}

/// Decompresses one zip member by name and hashes it through [`hash_forms`].
///
/// # Errors
///
/// As [`hash_zip_member`].
///
/// ```
/// use mistarr_core::hash::{hash_zip_member_forms, HeaderRule};
/// use std::io::{Cursor, Write};
///
/// let mut buf = Vec::new();
/// let mut zip = zip::ZipWriter::new(Cursor::new(&mut buf));
/// zip.start_file("a.lnx", zip::write::SimpleFileOptions::default()).unwrap();
/// zip.write_all(b"abc").unwrap();
/// zip.finish().unwrap();
///
/// let forms = hash_zip_member_forms(Cursor::new(buf), "a.lnx", HeaderRule::Lnx).unwrap();
/// assert_eq!((forms.content.size, forms.whole), (3, None));
/// ```
pub fn hash_zip_member_forms<R: Read + Seek>(
    r: R,
    name: &str,
    rule: HeaderRule,
) -> Result<HeaderForms, HashError> {
    let mut archive = zip::ZipArchive::new(r)?;
    let file = archive.by_name(name)?;
    let size_hint = Some(file.size());
    Ok(hash_forms(file, rule, size_hint)?)
}

/// The CRC32 of a zip member's content once `rule` strips its header, from the
/// central directory's whole-member CRC32 and the header bytes alone: only the
/// header is decompressed. `None` when the rule strips nothing or the member
/// does not start with its header, so its content is the whole member.
///
/// # Errors
///
/// As [`hash_zip_member`], or when `member.crc32` is not 8 hex digits.
///
/// ```
/// use mistarr_core::hash::{hash_reader, zip_member_content_crc, zip_members, HeaderRule};
/// use std::io::{Cursor, Write};
///
/// let mut rom = b"LYNX".to_vec();
/// rom.resize(64, 0);
/// rom.extend_from_slice(b"abc");
/// let mut buf = Vec::new();
/// let mut zip = zip::ZipWriter::new(Cursor::new(&mut buf));
/// zip.start_file("a.lnx", zip::write::SimpleFileOptions::default()).unwrap();
/// zip.write_all(&rom).unwrap();
/// zip.finish().unwrap();
///
/// let member = &zip_members(Cursor::new(&buf)).unwrap()[0];
/// let crc = zip_member_content_crc(Cursor::new(&buf), member, HeaderRule::Lnx).unwrap();
/// assert_eq!(crc.as_deref(), Some("352441c2"));
/// ```
pub fn zip_member_content_crc<R: Read + Seek>(
    r: R,
    member: &ZipMember,
    rule: HeaderRule,
) -> Result<Option<String>, HashError> {
    if !rule.strips_header() || member.size < rule.header_len() {
        return Ok(None);
    }
    let whole = u32::from_str_radix(&member.crc32, 16)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "member CRC32 is not hex"))?;
    let mut archive = zip::ZipArchive::new(r)?;
    // At most 128 bytes, so the length always fits in usize.
    #[allow(clippy::cast_possible_truncation)]
    let head = read_probe(
        &mut archive.by_name(&member.name)?,
        rule.header_len() as usize,
    )?;
    if !header_found(rule, &head) || (head.len() as u64) < rule.header_len() {
        return Ok(None);
    }
    Ok(Some(format!(
        "{:08x}",
        strip_crc(whole, &head, member.size - rule.header_len())
    )))
}

/// The CRC32 of the last `rest` bytes of a stream whose CRC32 is `whole` and
/// which starts with `head`: CRC32 is linear, so `crc(head ++ rest)` is
/// `crc(head)` shifted over `rest` bytes, XOR `crc(rest)`.
fn strip_crc(whole: u32, head: &[u8], rest: u64) -> u32 {
    let mut shifted =
        crc32fast::Hasher::new_with_initial_len(crc32fast::hash(head), head.len() as u64);
    shifted.combine(&crc32fast::Hasher::new_with_initial_len(0, rest));
    whole ^ shifted.finalize()
}

/// Incremental MD5 over bytes fed in order, for a digest that spans several inputs.
///
/// ```
/// let mut md5 = mistarr_core::hash::Md5Stream::new();
/// md5.update(b"a");
/// md5.update(b"bc");
/// assert_eq!(md5.finish(), "900150983cd24fb0d6963f7d28e17f72");
/// ```
#[derive(Debug, Clone, Default)]
pub struct Md5Stream(md5::Md5);

impl Md5Stream {
    /// An empty digest.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds the next bytes.
    pub fn update(&mut self, buf: &[u8]) {
        self.0.update(buf);
    }

    /// The digest as 32 lowercase hex characters.
    #[must_use]
    pub fn finish(self) -> String {
        hex(&self.0.finalize())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    #[test]
    fn rules_name_and_measure_their_headers() {
        for (name, rule, len) in [
            ("none", HeaderRule::None, 0),
            ("ines", HeaderRule::Ines, 16),
            ("smc", HeaderRule::Smc, 512),
            ("a78", HeaderRule::A78, 128),
            ("lnx", HeaderRule::Lnx, 64),
            ("n64", HeaderRule::N64, 0),
        ] {
            assert_eq!(HeaderRule::from_name(name), rule);
            assert_eq!(rule.header_len(), len);
        }
    }

    #[test]
    fn md5_stream_matches_one_pass() {
        let mut m = Md5Stream::new();
        for chunk in [&b"ab"[..], b"", b"c"] {
            m.update(chunk);
        }
        let one = hash_reader(Cursor::new(b"abc"), HeaderRule::None, None).expect("hash");
        assert_eq!(m.finish(), one.md5);
        assert_eq!(Md5Stream::new().finish(), empty().1);
    }

    fn empty() -> (&'static str, &'static str, &'static str) {
        (
            "00000000",
            "d41d8cd98f00b204e9800998ecf8427e",
            "da39a3ee5e6b4b0d3255bfef95601890afd80709",
        )
    }

    #[test]
    fn reference_vectors_empty() {
        let (crc, md5, sha1) = empty();
        let h = hash_reader(Cursor::new(b""), HeaderRule::None, None).unwrap();
        assert_eq!(h.size, 0);
        assert_eq!(h.crc32, crc);
        assert_eq!(h.md5, md5);
        assert_eq!(h.sha1, sha1);
    }

    #[test]
    fn reference_vectors_abc() {
        let h = hash_reader(Cursor::new(b"abc"), HeaderRule::None, None).unwrap();
        assert_eq!(h.size, 3);
        assert_eq!(h.crc32, "352441c2");
        assert_eq!(h.md5, "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(h.sha1, "a9993e364706816aba3e25717850c26c9cd0d89d");
    }

    #[test]
    fn reference_vectors_million_a() {
        let data = vec![b'a'; 1_000_000];
        let h = hash_reader(Cursor::new(data), HeaderRule::None, None).unwrap();
        assert_eq!(h.size, 1_000_000);
        assert_eq!(h.sha1, "34aa973cd4c4daa4f61eeb2bdbad27316534016f");
        assert_eq!(h.md5, "7707d6ae4e027c70eea2a935c2296f21");
    }

    #[test]
    fn ines_strips_header_when_present() {
        let mut data = b"NES\x1a".to_vec();
        data.extend(std::iter::repeat_n(0u8, 12));
        data.extend_from_slice(b"abc");
        let h = hash_reader(Cursor::new(data), HeaderRule::Ines, None).unwrap();
        assert_eq!(h.size, 3);
        assert_eq!(h.sha1, "a9993e364706816aba3e25717850c26c9cd0d89d");
    }

    #[test]
    fn ines_hashes_whole_file_when_absent() {
        let h = hash_reader(Cursor::new(b"abc"), HeaderRule::Ines, None).unwrap();
        assert_eq!(h.size, 3);
        assert_eq!(h.sha1, "a9993e364706816aba3e25717850c26c9cd0d89d");
    }

    #[test]
    fn smc_strips_512_when_size_matches_no_hint() {
        let mut data = vec![0xffu8; 512];
        data.extend(vec![0u8; 1024]);
        let h = hash_reader(Cursor::new(data), HeaderRule::Smc, None).unwrap();
        assert_eq!(h.size, 1024);
        let expect = hash_reader(Cursor::new(vec![0u8; 1024]), HeaderRule::None, None).unwrap();
        assert_eq!(h, expect);
    }

    #[test]
    fn smc_hashes_whole_file_when_size_does_not_match_no_hint() {
        let data = vec![0u8; 1024];
        let h = hash_reader(Cursor::new(data.clone()), HeaderRule::Smc, None).unwrap();
        let expect = hash_reader(Cursor::new(data), HeaderRule::None, None).unwrap();
        assert_eq!(h, expect);
    }

    #[test]
    fn smc_strips_512_when_size_hint_matches() {
        let mut data = vec![0xffu8; 512];
        data.extend(vec![0u8; 1024]);
        let size = data.len() as u64;
        let h = hash_reader(Cursor::new(data), HeaderRule::Smc, Some(size)).unwrap();
        assert_eq!(h.size, 1024);
        let expect = hash_reader(Cursor::new(vec![0u8; 1024]), HeaderRule::None, None).unwrap();
        assert_eq!(h, expect);
    }

    #[test]
    fn smc_hashes_whole_file_when_size_hint_does_not_match() {
        let data = vec![0u8; 1024];
        let size = data.len() as u64;
        let h = hash_reader(Cursor::new(data.clone()), HeaderRule::Smc, Some(size)).unwrap();
        let expect = hash_reader(Cursor::new(data), HeaderRule::None, None).unwrap();
        assert_eq!(h, expect);
    }

    #[test]
    fn a78_strips_header_when_present() {
        let mut data = vec![0x01u8];
        data.extend_from_slice(b"ATARI7800");
        data.extend(vec![0u8; 128 - 10]);
        data.extend_from_slice(b"abc");
        let h = hash_reader(Cursor::new(data), HeaderRule::A78, None).unwrap();
        assert_eq!(h.size, 3);
        assert_eq!(h.sha1, "a9993e364706816aba3e25717850c26c9cd0d89d");
    }

    #[test]
    fn a78_hashes_whole_file_when_absent() {
        let h = hash_reader(Cursor::new(b"abc"), HeaderRule::A78, None).unwrap();
        assert_eq!(h.sha1, "a9993e364706816aba3e25717850c26c9cd0d89d");
    }

    #[test]
    fn lnx_strips_header_when_present() {
        let mut data = b"LYNX".to_vec();
        data.extend(vec![0u8; 60]);
        data.extend_from_slice(b"abc");
        let h = hash_reader(Cursor::new(data), HeaderRule::Lnx, None).unwrap();
        assert_eq!(h.size, 3);
        assert_eq!(h.sha1, "a9993e364706816aba3e25717850c26c9cd0d89d");
    }

    #[test]
    fn lnx_hashes_whole_file_when_absent() {
        let h = hash_reader(Cursor::new(b"abc"), HeaderRule::Lnx, None).unwrap();
        assert_eq!(h.sha1, "a9993e364706816aba3e25717850c26c9cd0d89d");
    }

    #[test]
    fn n64_byte_orders_match() {
        let payload: Vec<u8> = (0u32..4096).flat_map(u32::to_be_bytes).collect();
        let mut z64 = vec![0x80, 0x37, 0x12, 0x40];
        z64.extend_from_slice(&payload[4..]);

        let mut v64 = vec![0x37, 0x80, 0x40, 0x12];
        for chunk in payload[4..].as_chunks::<2>().0 {
            v64.push(chunk[1]);
            v64.push(chunk[0]);
        }

        let mut n64 = vec![0x40, 0x12, 0x37, 0x80];
        for chunk in payload[4..].as_chunks::<4>().0 {
            n64.extend_from_slice(&[chunk[3], chunk[2], chunk[1], chunk[0]]);
        }

        let z = hash_reader(Cursor::new(z64), HeaderRule::N64, None).unwrap();
        let v = hash_reader(Cursor::new(v64), HeaderRule::N64, None).unwrap();
        let n = hash_reader(Cursor::new(n64), HeaderRule::N64, None).unwrap();
        assert_eq!(z, v);
        assert_eq!(z, n);
    }

    #[test]
    fn n64_unrecognised_header_is_hashed_as_is() {
        let data = vec![0u8; 8];
        let h = hash_reader(Cursor::new(data.clone()), HeaderRule::N64, None).unwrap();
        let expect = hash_reader(Cursor::new(data), HeaderRule::None, None).unwrap();
        assert_eq!(h, expect);
    }

    fn build_zip() -> Vec<u8> {
        let mut buf = Vec::new();
        let mut zip = zip::ZipWriter::new(Cursor::new(&mut buf));
        let opts = zip::write::SimpleFileOptions::default();
        zip.start_file("rom.bin", opts).unwrap();
        zip.write_all(b"abc").unwrap();
        zip.start_file("other.bin", opts).unwrap();
        zip.write_all(b"xyz").unwrap();
        zip.finish().unwrap();
        buf
    }

    #[test]
    fn zip_members_reads_central_directory() {
        let members = zip_members(Cursor::new(build_zip())).unwrap();
        assert_eq!(members.len(), 2);
        assert_eq!(members[0].name, "rom.bin");
        assert_eq!(members[0].size, 3);
        assert_eq!(members[0].crc32, "352441c2");
    }

    // Hand-assembled: the `zip` crate's writer refuses a compression method
    // it cannot encode, but listing must not decode anything to succeed.
    // Buffers here are a few dozen bytes, so length casts never truncate.
    #[allow(clippy::cast_possible_truncation)]
    fn build_mixed_method_zip() -> Vec<u8> {
        fn entry(
            buf: &mut Vec<u8>,
            offsets: &mut Vec<u32>,
            name: &[u8],
            method: u16,
            data: &[u8],
            crc32: u32,
        ) {
            offsets.push(buf.len() as u32);
            buf.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            buf.extend_from_slice(&20u16.to_le_bytes());
            buf.extend_from_slice(&0u16.to_le_bytes());
            buf.extend_from_slice(&method.to_le_bytes());
            buf.extend_from_slice(&0u16.to_le_bytes());
            buf.extend_from_slice(&0u16.to_le_bytes());
            buf.extend_from_slice(&crc32.to_le_bytes());
            buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
            buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
            buf.extend_from_slice(&(name.len() as u16).to_le_bytes());
            buf.extend_from_slice(&0u16.to_le_bytes());
            buf.extend_from_slice(name);
            buf.extend_from_slice(data);
        }

        fn central(
            buf: &mut Vec<u8>,
            name: &[u8],
            method: u16,
            size: u32,
            crc32: u32,
            offset: u32,
        ) {
            buf.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            buf.extend_from_slice(&20u16.to_le_bytes());
            buf.extend_from_slice(&20u16.to_le_bytes());
            buf.extend_from_slice(&0u16.to_le_bytes());
            buf.extend_from_slice(&method.to_le_bytes());
            buf.extend_from_slice(&0u16.to_le_bytes());
            buf.extend_from_slice(&0u16.to_le_bytes());
            buf.extend_from_slice(&crc32.to_le_bytes());
            buf.extend_from_slice(&size.to_le_bytes());
            buf.extend_from_slice(&size.to_le_bytes());
            buf.extend_from_slice(&(name.len() as u16).to_le_bytes());
            buf.extend_from_slice(&[0u8; 8]);
            buf.extend_from_slice(&0u32.to_le_bytes());
            buf.extend_from_slice(&offset.to_le_bytes());
            buf.extend_from_slice(name);
        }

        let mut buf = Vec::new();
        let mut offsets = Vec::new();
        entry(
            &mut buf,
            &mut offsets,
            b"stored.bin",
            0,
            b"abc",
            0x3524_41c2,
        );
        entry(
            &mut buf,
            &mut offsets,
            b"bzip2.bin",
            12,
            b"garbage",
            0xdead_beef,
        );

        let cd_start = buf.len() as u32;
        central(&mut buf, b"stored.bin", 0, 3, 0x3524_41c2, offsets[0]);
        central(&mut buf, b"bzip2.bin", 12, 7, 0xdead_beef, offsets[1]);
        let cd_size = buf.len() as u32 - cd_start;

        buf.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&2u16.to_le_bytes());
        buf.extend_from_slice(&2u16.to_le_bytes());
        buf.extend_from_slice(&cd_size.to_le_bytes());
        buf.extend_from_slice(&cd_start.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf
    }

    #[test]
    fn zip_members_lists_unsupported_method_without_decoding() {
        let members = zip_members(Cursor::new(build_mixed_method_zip())).unwrap();
        assert_eq!(members.len(), 2);
        assert_eq!(members[0].name, "stored.bin");
        assert_eq!(members[0].size, 3);
        assert_eq!(members[1].name, "bzip2.bin");
        assert_eq!(members[1].size, 7);
        assert_eq!(members[1].crc32, "deadbeef");
    }

    #[test]
    fn hash_zip_member_still_fails_to_decompress_unsupported_method() {
        let err = hash_zip_member(
            Cursor::new(build_mixed_method_zip()),
            "bzip2.bin",
            HeaderRule::None,
        );
        assert!(err.is_err());
    }

    #[test]
    fn hash_zip_member_decompresses_and_hashes() {
        let h = hash_zip_member(Cursor::new(build_zip()), "rom.bin", HeaderRule::None).unwrap();
        assert_eq!(h.sha1, "a9993e364706816aba3e25717850c26c9cd0d89d");
    }

    #[test]
    fn hash_zip_member_missing_name_errors() {
        let err = hash_zip_member(Cursor::new(build_zip()), "missing.bin", HeaderRule::None);
        assert!(err.is_err());
    }

    /// A reader handing out at most `chunk` bytes per read, to move the buffer boundaries.
    struct Chunked<'a> {
        data: &'a [u8],
        chunk: usize,
    }

    impl Read for Chunked<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = self.chunk.min(buf.len()).min(self.data.len());
            buf[..n].copy_from_slice(&self.data[..n]);
            self.data = &self.data[n..];
            Ok(n)
        }
    }

    /// A payload of `len` bytes starting with `rule`'s header when `headered`.
    #[allow(clippy::cast_possible_truncation)] // A byte-filler pattern.
    fn synthetic(rule: HeaderRule, headered: bool, len: usize) -> Vec<u8> {
        let mut data: Vec<u8> = (0..len).map(|i| (i * 7 + 3) as u8).collect();
        let magic: &[u8] = match rule {
            HeaderRule::Ines => b"NES\x1a",
            HeaderRule::A78 => b"\x01ATARI7800",
            _ => b"LYNX",
        };
        if headered && len >= magic.len() {
            data[..magic.len()].copy_from_slice(magic);
        } else if !data.is_empty() {
            data[0] = b'x';
        }
        data
    }

    fn plain(data: &[u8]) -> HashSet {
        hash_reader(data, HeaderRule::None, None).expect("hash")
    }

    #[test]
    fn forms_of_a_headered_file_are_the_whole_file_and_its_content() {
        for rule in [HeaderRule::Ines, HeaderRule::A78, HeaderRule::Lnx] {
            let data = synthetic(rule, true, 1000);
            let forms = hash_forms(&data[..], rule, None).expect("hash");
            let header = usize::try_from(rule.header_len()).expect("len");
            assert_eq!(forms.content, plain(&data[header..]), "{rule:?}");
            assert_eq!(forms.whole.as_ref(), Some(&plain(&data)), "{rule:?}");
            assert_eq!(
                forms.content,
                hash_reader(&data[..], rule, None).expect("hash")
            );
        }
    }

    #[test]
    fn forms_of_a_file_without_a_header_are_one() {
        for rule in [
            HeaderRule::Ines,
            HeaderRule::A78,
            HeaderRule::Lnx,
            HeaderRule::None,
        ] {
            let data = synthetic(rule, false, 1000);
            let forms = hash_forms(&data[..], rule, None).expect("hash");
            assert_eq!(
                (forms.content.clone(), forms.whole.clone()),
                (plain(&data), None)
            );
            assert_eq!(forms.whole_or_content(), &plain(&data));
        }
    }

    #[test]
    fn forms_of_other_rules_keep_only_the_content() {
        let mut smc = vec![0xffu8; 512];
        smc.extend(vec![1u8; 1024]);
        let forms = hash_forms(&smc[..], HeaderRule::Smc, None).expect("hash");
        assert_eq!((forms.content, forms.whole), (plain(&smc[512..]), None));
    }

    #[test]
    fn zip_forms_and_content_crc_agree_with_the_payload() {
        for rule in [HeaderRule::Ines, HeaderRule::A78, HeaderRule::Lnx] {
            for headered in [true, false] {
                let data = synthetic(rule, headered, 700);
                let mut buf = Vec::new();
                let mut zip = zip::ZipWriter::new(Cursor::new(&mut buf));
                let opts = zip::write::SimpleFileOptions::default();
                zip.start_file("a.bin", opts).unwrap();
                zip.write_all(&data).unwrap();
                zip.finish().unwrap();
                let member = &zip_members(Cursor::new(&buf)).unwrap()[0];
                let forms = hash_zip_member_forms(Cursor::new(&buf), "a.bin", rule).unwrap();
                assert_eq!(forms, hash_forms(&data[..], rule, None).unwrap());
                let crc = zip_member_content_crc(Cursor::new(&buf), member, rule).unwrap();
                let expect = headered.then(|| forms.content.crc32.clone());
                assert_eq!(crc, expect, "{rule:?} headered={headered}");
            }
        }
    }

    #[test]
    fn content_crc_is_none_for_rules_that_strip_nothing_or_short_members() {
        let member = ZipMember {
            name: "a.bin".into(),
            size: 3,
            crc32: "352441c2".into(),
        };
        let none = zip_member_content_crc(Cursor::new(build_zip()), &member, HeaderRule::None);
        assert!(none.unwrap().is_none());
        let short = zip_member_content_crc(Cursor::new(build_zip()), &member, HeaderRule::Ines);
        assert!(short.unwrap().is_none());
    }

    proptest::proptest! {
        #[test]
        fn dual_hashing_agrees_with_each_form_alone(
            len in 0usize..700,
            chunk in 1usize..300,
            headered in proptest::bool::ANY,
            which in 0usize..3,
        ) {
            let rule = [HeaderRule::Ines, HeaderRule::A78, HeaderRule::Lnx][which];
            let data = synthetic(rule, headered, len);
            let forms = hash_forms(Chunked { data: &data, chunk }, rule, None).expect("hash");
            let alone = hash_reader(Chunked { data: &data, chunk }, rule, None).expect("hash");
            proptest::prop_assert_eq!(&forms.content, &alone);
            proptest::prop_assert_eq!(forms.whole_or_content(), &plain(&data));
            if forms.whole.is_some() {
                let header = usize::try_from(rule.header_len()).expect("len").min(len);
                proptest::prop_assert_eq!(&forms.content, &plain(&data[header..]));
            }
        }

        #[test]
        fn a_stripped_crc_is_the_crc_of_the_rest(
            head in proptest::collection::vec(proptest::num::u8::ANY, 0..130),
            rest in proptest::collection::vec(proptest::num::u8::ANY, 0..600),
        ) {
            let mut all = head.clone();
            all.extend_from_slice(&rest);
            let got = strip_crc(crc32fast::hash(&all), &head, rest.len() as u64);
            proptest::prop_assert_eq!(got, crc32fast::hash(&rest));
        }
    }
}
