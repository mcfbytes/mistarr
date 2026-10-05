//! Matching hashed payloads to the roms of one DAT entry in memory, by
//! `docs/VERIFICATION.md` "Matching order".

use crate::{Crc32, Hashes, Md5, Sha1};

/// A DAT rom as matching reads it.
pub trait Rom {
    /// The id the catalogue gives the rom.
    type Id: Copy + PartialEq;
    /// The rom's id.
    fn id(&self) -> Self::Id;
    /// The rom's file name in the DAT, possibly with a subdirectory.
    fn name(&self) -> &str;
    /// Size in bytes.
    fn size(&self) -> u64;
    /// CRC32, when the DAT lists it.
    fn crc32(&self) -> Option<Crc32>;
    /// MD5, when the DAT lists it.
    fn md5(&self) -> Option<Md5>;
    /// SHA1, when the DAT lists it.
    fn sha1(&self) -> Option<Sha1>;
}

/// A hashed file or zip member as matching reads it.
pub trait Payload {
    /// The forms the payload matches a rom in, the whole payload first.
    fn forms(&self) -> impl Iterator<Item = &Hashes>;
    /// The zip member name, `None` for a plain file.
    fn member(&self) -> Option<&str>;

    /// Whether the payload is `rom` in any of its forms.
    fn is<R: Rom>(&self, rom: &R) -> bool {
        self.forms().any(|h| rom_matches(rom, h))
    }
}

/// Whether `h` is `rom`: SHA1 when the DAT has it, else MD5, else CRC32 plus size.
///
/// ```
/// use mistarr_core::matching::{rom_matches, Rom};
/// use mistarr_core::{Crc32, Hashes, Md5, Sha1};
/// struct R;
/// impl Rom for R {
///     type Id = u8;
///     fn id(&self) -> u8 { 1 }
///     fn name(&self) -> &str { "a.bin" }
///     fn size(&self) -> u64 { 3 }
///     fn crc32(&self) -> Option<Crc32> { Some(Crc32::from_u32(0x3524_41c2)) }
///     fn md5(&self) -> Option<Md5> { None }
///     fn sha1(&self) -> Option<Sha1> { None }
/// }
/// let crc32 = Crc32::from_u32(0x3524_41c2);
/// let h = Hashes { size: 3, crc32, md5: Md5::from_bytes([0; 16]), sha1: Sha1::from_bytes([0; 20]) };
/// assert!(rom_matches(&R, &h));
/// ```
#[must_use]
pub fn rom_matches<R: Rom + ?Sized>(rom: &R, h: &Hashes) -> bool {
    if let Some(sha1) = rom.sha1() {
        return sha1 == h.sha1;
    }
    if let Some(md5) = rom.md5() {
        return md5 == h.md5;
    }
    rom.crc32() == Some(h.crc32) && rom.size() == h.size
}

/// The last component of a name that may use `/` or `\` as a separator.
///
/// ```
/// assert_eq!(mistarr_core::matching::leaf("a/b\\c.bin"), "c.bin");
/// ```
#[must_use]
pub fn leaf(name: &str) -> &str {
    name.rsplit(['/', '\\']).next().unwrap_or(name)
}

/// The rom of `roms` that `p` is in any form, skipping those in `used`. Among identical
/// roms it prefers `prefer`, then one whose leaf name is `name`'s in any case, then the first.
#[must_use]
pub fn pick_rom<'a, R: Rom, P: Payload + ?Sized>(
    roms: &'a [R],
    p: &P,
    prefer: Option<R::Id>,
    name: Option<&str>,
    used: &[R::Id],
) -> Option<&'a R> {
    let hits: Vec<&R> = roms
        .iter()
        .filter(|r| !used.contains(&r.id()) && p.is(*r))
        .collect();
    hits.iter()
        .find(|r| Some(r.id()) == prefer)
        .or_else(|| {
            hits.iter()
                .find(|r| name.is_some_and(|n| leaf(r.name()).eq_ignore_ascii_case(leaf(n))))
        })
        .or_else(|| hits.first())
        .copied()
}

/// How the members of a zip pair with the roms of a DAT entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetMatch<'a, P, R> {
    /// Each member that is a rom of the entry, with that rom.
    pub pairs: Vec<(&'a P, &'a R)>,
    /// Members that are no rom of the entry, by name.
    pub extra: Vec<String>,
    /// Roms of the entry no member is, by name.
    pub absent: Vec<String>,
}

impl<P, R> SetMatch<'_, P, R> {
    /// Every member is a rom of the entry and every rom is a member.
    #[must_use]
    pub fn is_exact(&self) -> bool {
        self.extra.is_empty() && self.absent.is_empty()
    }
}

/// Pairs each member with a distinct rom of `roms` by hash, preferring the same name.
#[must_use]
pub fn match_members<'a, R: Rom, P: Payload>(
    roms: &'a [R],
    members: &'a [P],
) -> SetMatch<'a, P, R> {
    let mut used = Vec::with_capacity(members.len());
    let mut out = SetMatch {
        pairs: Vec::with_capacity(members.len()),
        extra: Vec::new(),
        absent: Vec::new(),
    };
    for m in members {
        match pick_rom(roms, m, None, m.member(), &used) {
            Some(rom) => {
                used.push(rom.id());
                out.pairs.push((m, rom));
            }
            None => out.extra.push(m.member().unwrap_or_default().to_owned()),
        }
    }
    out.absent = roms
        .iter()
        .filter(|r| !used.contains(&r.id()))
        .map(|r| r.name().to_owned())
        .collect();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::{hash_reader, HeaderRule};
    use std::io::Cursor;

    #[derive(Debug, Clone, PartialEq)]
    struct TestRom {
        id: i64,
        name: String,
        size: u64,
        crc32: Option<Crc32>,
        md5: Option<Md5>,
        sha1: Option<Sha1>,
    }

    impl Rom for TestRom {
        type Id = i64;
        fn id(&self) -> i64 {
            self.id
        }
        fn name(&self) -> &str {
            &self.name
        }
        fn size(&self) -> u64 {
            self.size
        }
        fn crc32(&self) -> Option<Crc32> {
            self.crc32
        }
        fn md5(&self) -> Option<Md5> {
            self.md5
        }
        fn sha1(&self) -> Option<Sha1> {
            self.sha1
        }
    }

    #[derive(Debug, Clone, PartialEq)]
    struct TestPayload {
        member: Option<String>,
        hashes: Hashes,
        whole: Option<Hashes>,
    }

    impl Payload for TestPayload {
        fn forms(&self) -> impl Iterator<Item = &Hashes> {
            self.whole.iter().chain([&self.hashes])
        }
        fn member(&self) -> Option<&str> {
            self.member.as_deref()
        }
    }

    fn rom(id: i64, name: &str, h: &Hashes) -> TestRom {
        TestRom {
            id,
            name: name.into(),
            size: h.size,
            crc32: Some(h.crc32),
            md5: Some(h.md5),
            sha1: Some(h.sha1),
        }
    }

    fn plain(member: Option<&str>, hashes: &Hashes) -> TestPayload {
        TestPayload {
            member: member.map(Into::into),
            hashes: *hashes,
            whole: None,
        }
    }

    fn hash(bytes: &[u8]) -> Hashes {
        hash_reader(Cursor::new(bytes), HeaderRule::None, None).expect("hash")
    }

    #[test]
    fn roms_match_in_order_and_identical_ones_are_told_apart() {
        let h = hash(b"abc");
        let a = rom(1, "Disc (Track 1).bin", &h);
        let b = rom(2, "Disc (Track 2).bin", &h);
        let roms = [a.clone(), b.clone()];
        let p = plain(None, &h);
        let id = |r: Option<&TestRom>| r.map(|r| r.id);
        assert_eq!(id(pick_rom(&roms, &p, None, None, &[])), Some(1));
        assert_eq!(id(pick_rom(&roms, &p, Some(2), None, &[])), Some(2));
        let named = pick_rom(&roms, &p, None, Some("x/disc (track 2).bin"), &[]);
        assert_eq!(id(named), Some(2));
        assert_eq!(id(pick_rom(&roms, &p, Some(1), None, &[1])), Some(2));
        assert!(pick_rom(&roms, &p, None, None, &[1, 2]).is_none());
        let crc_only = TestRom {
            sha1: None,
            md5: None,
            ..a.clone()
        };
        assert!(rom_matches(&crc_only, &h));
        let wrong_size = TestRom {
            size: 4,
            ..crc_only
        };
        assert!(!rom_matches(&wrong_size, &h));
        let md5_only = TestRom { sha1: None, ..b };
        assert!(rom_matches(&md5_only, &h));
        assert!(!rom_matches(&a, &hash(b"abd")));
    }

    #[test]
    fn a_payload_is_the_rom_of_either_form() {
        let (whole, body) = (hash(b"NES\x1abody"), hash(b"body"));
        let p = TestPayload {
            whole: Some(whole),
            ..plain(None, &body)
        };
        assert!(p.is(&rom(1, "a.nes", &whole)), "a headered DAT");
        assert!(p.is(&rom(2, "a.nes", &body)), "a headerless DAT");
        assert!(!p.is(&rom(3, "a.nes", &hash(b"other"))));
    }

    #[test]
    fn members_pair_with_distinct_roms_and_leftovers_are_named() {
        let h = hash(b"abc");
        let other = hash(b"xyz");
        let roms = [rom(1, "a.bin", &h), rom(2, "b.bin", &h)];
        let both = [plain(Some("b.bin"), &h), plain(Some("a.bin"), &h)];
        let set = match_members(&roms, &both);
        assert!(set.is_exact());
        assert_eq!(
            set.pairs.iter().map(|(_, r)| r.id).collect::<Vec<_>>(),
            [2, 1]
        );
        let odd = [plain(Some("a.bin"), &h), plain(Some("c.bin"), &other)];
        let set = match_members(&roms, &odd);
        assert!(!set.is_exact());
        assert_eq!(set.extra, ["c.bin"]);
        assert_eq!(set.absent, ["b.bin"]);
    }

    #[test]
    fn leaves_split_on_either_separator() {
        assert_eq!(leaf("a/b.bin"), "b.bin");
        assert_eq!(leaf("a\\b.bin"), "b.bin");
        assert_eq!(leaf("b.bin"), "b.bin");
    }
}
