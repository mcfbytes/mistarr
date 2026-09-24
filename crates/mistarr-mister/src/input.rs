//! Minimal descriptions of a DAT entry and a staged download, as adapters see them.

use std::path::PathBuf;

/// One DAT `<game>` as far as placement needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatEntry {
    /// The entry name, used as the title of the placed file or directory.
    pub name: String,
    /// The roms the entry lists, in DAT order.
    pub roms: Vec<DatRom>,
}

/// One DAT `<rom>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatRom {
    /// File name the DAT gives the rom.
    pub name: String,
    /// Size in bytes.
    pub size: u64,
    /// Header bytes from the rom's `header` attribute, when the DAT has one.
    pub header: Option<Vec<u8>>,
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
