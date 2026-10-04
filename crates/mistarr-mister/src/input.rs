//! Minimal descriptions of a DAT entry and a staged download, as adapters see them.

use std::path::PathBuf;

use mistarr_core::dat::{decode_header, DatRom};

/// One DAT `<game>` as far as placement needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatEntry {
    /// The entry name, used as the title of the placed file or directory.
    pub name: String,
    /// The roms the entry lists, in DAT order.
    pub roms: Vec<PlaceRom>,
}

/// One DAT `<rom>` as far as placement needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaceRom {
    /// File name the DAT gives the rom.
    pub name: String,
    /// Size in bytes.
    pub size: u64,
    /// Header bytes from the rom's `header` attribute, when the DAT has one.
    pub header: Option<Vec<u8>>,
}

impl PlaceRom {
    /// A rom named `name` of `size` bytes, its header bytes decoded from the DAT's
    /// `header` text by [`decode_header`].
    ///
    /// ```
    /// let place = mistarr_mister::PlaceRom::new("q.nes", 4, Some("4E 45 53 1A"));
    /// assert_eq!((place.size, place.header), (4, Some(b"NES\x1a".to_vec())));
    /// ```
    #[must_use]
    pub fn new(name: &str, size: u64, header: Option<&str>) -> Self {
        Self {
            name: name.to_owned(),
            size,
            header: header.and_then(decode_header),
        }
    }
}

impl From<&DatRom> for PlaceRom {
    /// [`PlaceRom::new`] with the rom's name, size and `header` attribute.
    ///
    /// ```
    /// use mistarr_core::dat::{DatRom, RomStatus};
    /// let rom = DatRom { name: "q.nes".into(), size: 4, crc32: None, md5: None, sha1: None,
    ///     status: RomStatus::Good, header: Some("4E 45 53 1A".into()) };
    /// let place = mistarr_mister::PlaceRom::from(&rom);
    /// assert_eq!((place.size, place.header), (4, Some(b"NES\x1a".to_vec())));
    /// ```
    fn from(rom: &DatRom) -> Self {
        Self::new(&rom.name, rom.size, rom.header.as_deref())
    }
}

/// What kind of item sits at [`StagedFile::path`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum StagedKind {
    /// A single plain file.
    File,
    /// A zip archive; [`StagedFile::members`] lists its entries.
    Zip,
    /// A directory; [`StagedFile::members`] lists the files in it.
    Dir,
}

/// A downloaded item in staging that has been hashed and matched to a DAT entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedFile {
    /// Absolute path of the file, archive or directory in staging.
    pub path: PathBuf,
    /// Size in bytes of a plain file or archive; zero for a directory.
    pub size: u64,
    /// Shape of the item.
    pub kind: StagedKind,
    /// First bytes of a plain file (at least 16 when the file is that long).
    pub head: Vec<u8>,
    /// Archive entries or directory files; empty for a plain file.
    pub members: Vec<StagedMember>,
}

/// One entry inside a staged zip or directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedMember {
    /// Path of the member relative to the archive or directory root.
    pub name: String,
    /// Uncompressed size in bytes.
    pub size: u64,
    /// First bytes of the member's uncompressed contents.
    pub head: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use mistarr_core::dat::RomStatus;

    use super::*;

    #[test]
    fn place_rom_takes_header_bytes_from_the_dat_rom() {
        let mut rom = DatRom {
            name: "Example Quest (USA).nes".into(),
            size: 40_976,
            crc32: None,
            md5: None,
            sha1: None,
            status: RomStatus::Good,
            header: Some("4E 45 53 1a 00".into()),
        };
        let place = PlaceRom::from(&rom);
        assert_eq!(place.name, rom.name);
        assert_eq!(place.size, 40_976);
        assert_eq!(place.header, Some(b"NES\x1a\0".to_vec()));
        for bad in [None, Some(String::new()), Some("4E 4".into())] {
            rom.header = bad;
            assert_eq!(PlaceRom::from(&rom).header, None);
        }
    }

    #[test]
    fn place_rom_new_decodes_header_text() {
        let place = PlaceRom::new("a.nes", 2, Some("4e\t45"));
        assert_eq!(
            (place.name.as_str(), place.size, place.header),
            ("a.nes", 2, Some(b"NE".to_vec()))
        );
        for bad in [None, Some(""), Some(" "), Some("4E 4")] {
            assert_eq!(PlaceRom::new("a.nes", 2, bad).header, None, "{bad:?}");
        }
    }
}
