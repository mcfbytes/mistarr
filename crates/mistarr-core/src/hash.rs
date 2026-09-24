//! One-pass CRC32/MD5/SHA1 hashing and the platform header rules of
//! `docs/PLATFORMS.md` "Header rules", plus a zip central-directory
//! pre-check. See `docs/VERIFICATION.md` "Hashing".

use std::io::{self, Read, Seek};

use md5::Digest as _;

use crate::HashSet;

/// Streaming buffer size, matching the "Hashing buffer" budget in
/// `docs/ARCHITECTURE.md`.
const BUF_SIZE: usize = 256 * 1024;

/// Header handling applied while hashing, one variant per row of the
/// "Header rules" table in `docs/PLATFORMS.md`.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderRule {
    /// Hash the whole file.
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

struct Hashers {
    crc: crc32fast::Hasher,
    md5: md5::Md5,
    sha1: sha1::Sha1,
    len: u64,
}

impl Hashers {
    fn new() -> Self {
        Self {
            crc: crc32fast::Hasher::new(),
            md5: md5::Md5::new(),
            sha1: sha1::Sha1::new(),
            len: 0,
        }
    }

    fn update(&mut self, buf: &[u8]) {
        self.crc.update(buf);
        self.md5.update(buf);
        self.sha1.update(buf);
        self.len += buf.len() as u64;
    }

    fn finish(self) -> HashSet {
        HashSet {
            size: self.len,
            crc32: format!("{:08x}", self.crc.finalize()),
            md5: hex(&self.md5.finalize()),
            sha1: hex(&self.sha1.finalize()),
        }
    }
}

fn hex(bytes: &[u8]) -> String {
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
/// # Errors
///
/// Returns an error if reading from `r` fails.
///
/// ```
/// use mistarr_core::hash::{hash_reader, HeaderRule};
/// use std::io::Cursor;
///
/// let hashes = hash_reader(Cursor::new(b"abc"), HeaderRule::None).unwrap();
/// assert_eq!(hashes.sha1, "a9993e364706816aba3e25717850c26c9cd0d89d");
/// ```
pub fn hash_reader<R: Read>(r: R, rule: HeaderRule) -> io::Result<HashSet> {
    match rule {
        HeaderRule::None => hash_stream(r, &[]),
        HeaderRule::Ines => hash_with_magic_skip(r, 4, 16, |p| p == b"NES\x1a"),
        HeaderRule::Smc => hash_smc(r),
        HeaderRule::A78 => {
            hash_with_magic_skip(r, 10, 128, |p| p.len() >= 10 && &p[1..10] == b"ATARI7800")
        }
        HeaderRule::Lnx => hash_with_magic_skip(r, 4, 64, |p| p == b"LYNX"),
        HeaderRule::N64 => hash_n64(r),
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

fn hash_with_magic_skip<R: Read>(
    mut r: R,
    probe_len: usize,
    skip_len: usize,
    matches: impl Fn(&[u8]) -> bool,
) -> io::Result<HashSet> {
    let probe = read_probe(&mut r, probe_len)?;
    if matches(&probe) {
        discard(&mut r, skip_len - probe.len())?;
        hash_stream(r, &[])
    } else {
        hash_stream(r, &probe)
    }
}

// Runs whole-file and header-skipped hashers in parallel; the SMC decision
// needs the total size, only known once the stream is exhausted.
fn hash_smc<R: Read>(mut r: R) -> io::Result<HashSet> {
    let mut whole = Hashers::new();
    let mut skipped = Hashers::new();
    let mut buf = vec![0u8; BUF_SIZE];
    let mut total: u64 = 0;
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let chunk = &buf[..n];
        whole.update(chunk);
        let pos_before = total;
        total += n as u64;
        if pos_before >= 512 {
            skipped.update(chunk);
        } else if total > 512 {
            // pos_before < 512 here, so the difference always fits in usize.
            #[allow(clippy::cast_possible_truncation)]
            let split = (512 - pos_before) as usize;
            skipped.update(&chunk[split..]);
        }
    }
    Ok(if total % 1024 == 512 {
        skipped.finish()
    } else {
        whole.finish()
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
            for pair in out.chunks_exact_mut(2) {
                pair.swap(0, 1);
            }
        }
        N64Order::Swap32 => {
            for quad in out.chunks_exact_mut(4) {
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

/// Reads only a zip's central directory and lists every member's name,
/// uncompressed size and stored CRC32.
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
        let file = archive.by_index(i)?;
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
    Ok(hash_reader(file, rule)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

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
        let h = hash_reader(Cursor::new(b""), HeaderRule::None).unwrap();
        assert_eq!(h.size, 0);
        assert_eq!(h.crc32, crc);
        assert_eq!(h.md5, md5);
        assert_eq!(h.sha1, sha1);
    }

    #[test]
    fn reference_vectors_abc() {
        let h = hash_reader(Cursor::new(b"abc"), HeaderRule::None).unwrap();
        assert_eq!(h.size, 3);
        assert_eq!(h.crc32, "352441c2");
        assert_eq!(h.md5, "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(h.sha1, "a9993e364706816aba3e25717850c26c9cd0d89d");
    }

    #[test]
    fn reference_vectors_million_a() {
        let data = vec![b'a'; 1_000_000];
        let h = hash_reader(Cursor::new(data), HeaderRule::None).unwrap();
        assert_eq!(h.size, 1_000_000);
        assert_eq!(h.sha1, "34aa973cd4c4daa4f61eeb2bdbad27316534016f");
        assert_eq!(h.md5, "7707d6ae4e027c70eea2a935c2296f21");
    }

    #[test]
    fn ines_strips_header_when_present() {
        let mut data = b"NES\x1a".to_vec();
        data.extend(std::iter::repeat(0u8).take(12));
        data.extend_from_slice(b"abc");
        let h = hash_reader(Cursor::new(data), HeaderRule::Ines).unwrap();
        assert_eq!(h.size, 3);
        assert_eq!(h.sha1, "a9993e364706816aba3e25717850c26c9cd0d89d");
    }

    #[test]
    fn ines_hashes_whole_file_when_absent() {
        let h = hash_reader(Cursor::new(b"abc"), HeaderRule::Ines).unwrap();
        assert_eq!(h.size, 3);
        assert_eq!(h.sha1, "a9993e364706816aba3e25717850c26c9cd0d89d");
    }

    #[test]
    fn smc_strips_512_when_size_matches() {
        let mut data = vec![0xffu8; 512];
        data.extend(vec![0u8; 1024]);
        let h = hash_reader(Cursor::new(data), HeaderRule::Smc).unwrap();
        assert_eq!(h.size, 1024);
        let expect = hash_reader(Cursor::new(vec![0u8; 1024]), HeaderRule::None).unwrap();
        assert_eq!(h, expect);
    }

    #[test]
    fn smc_hashes_whole_file_when_size_does_not_match() {
        let data = vec![0u8; 1024];
        let h = hash_reader(Cursor::new(data.clone()), HeaderRule::Smc).unwrap();
        let expect = hash_reader(Cursor::new(data), HeaderRule::None).unwrap();
        assert_eq!(h, expect);
    }

    #[test]
    fn a78_strips_header_when_present() {
        let mut data = vec![0x01u8];
        data.extend_from_slice(b"ATARI7800");
        data.extend(vec![0u8; 128 - 10]);
        data.extend_from_slice(b"abc");
        let h = hash_reader(Cursor::new(data), HeaderRule::A78).unwrap();
        assert_eq!(h.size, 3);
        assert_eq!(h.sha1, "a9993e364706816aba3e25717850c26c9cd0d89d");
    }

    #[test]
    fn a78_hashes_whole_file_when_absent() {
        let h = hash_reader(Cursor::new(b"abc"), HeaderRule::A78).unwrap();
        assert_eq!(h.sha1, "a9993e364706816aba3e25717850c26c9cd0d89d");
    }

    #[test]
    fn lnx_strips_header_when_present() {
        let mut data = b"LYNX".to_vec();
        data.extend(vec![0u8; 60]);
        data.extend_from_slice(b"abc");
        let h = hash_reader(Cursor::new(data), HeaderRule::Lnx).unwrap();
        assert_eq!(h.size, 3);
        assert_eq!(h.sha1, "a9993e364706816aba3e25717850c26c9cd0d89d");
    }

    #[test]
    fn lnx_hashes_whole_file_when_absent() {
        let h = hash_reader(Cursor::new(b"abc"), HeaderRule::Lnx).unwrap();
        assert_eq!(h.sha1, "a9993e364706816aba3e25717850c26c9cd0d89d");
    }

    #[test]
    fn n64_byte_orders_match() {
        let payload: Vec<u8> = (0u32..4096).flat_map(u32::to_be_bytes).collect();
        let mut z64 = vec![0x80, 0x37, 0x12, 0x40];
        z64.extend_from_slice(&payload[4..]);

        let mut v64 = vec![0x37, 0x80, 0x40, 0x12];
        for chunk in payload[4..].chunks_exact(2) {
            v64.push(chunk[1]);
            v64.push(chunk[0]);
        }

        let mut n64 = vec![0x40, 0x12, 0x37, 0x80];
        for chunk in payload[4..].chunks_exact(4) {
            n64.extend_from_slice(&[chunk[3], chunk[2], chunk[1], chunk[0]]);
        }

        let z = hash_reader(Cursor::new(z64), HeaderRule::N64).unwrap();
        let v = hash_reader(Cursor::new(v64), HeaderRule::N64).unwrap();
        let n = hash_reader(Cursor::new(n64), HeaderRule::N64).unwrap();
        assert_eq!(z, v);
        assert_eq!(z, n);
    }

    #[test]
    fn n64_unrecognised_header_is_hashed_as_is() {
        let data = vec![0u8; 8];
        let h = hash_reader(Cursor::new(data.clone()), HeaderRule::N64).unwrap();
        let expect = hash_reader(Cursor::new(data), HeaderRule::None).unwrap();
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
}
