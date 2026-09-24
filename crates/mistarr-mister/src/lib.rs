//! MiSTer specifics: the DAT-name to `games/<Core>` table, the `CoreAdapter`
//! trait and its implementations, installed-core detection, the
//! `/tmp/CORENAME` watcher and MRA parsing.
//!
//! See `docs/PLATFORMS.md` and `docs/WORKPLAN.md` WP-04 and WP-19.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]

pub mod adapter;
pub mod corename;
pub mod input;
pub mod platforms;

pub use adapter::{adapter_for, ByteOrder, CoreAdapter, PlacementPlan, Step};
pub use input::{DatEntry, DatRom, StagedFile, StagedKind, StagedMember};
pub use mistarr_core::PlatformId;
pub use platforms::{bind_dat_name, Kind, Platform};

/// Path of the file MiSTer writes the running core's name to. `MENU` means
/// no core is running.
pub const CORENAME_PATH: &str = "/tmp/CORENAME";

/// Failures a caller of this crate can act on.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A NES file has no iNES header and the DAT entry does not supply one.
    #[error("file has no iNES header and the DAT entry does not provide one")]
    MissingHeader,
    /// The first bytes of an N64 image match none of the three byte orders.
    #[error("unrecognised N64 byte order")]
    UnknownByteOrder,
    /// No staged file or archive member corresponds to a rom of the entry.
    #[error("no staged file matches rom `{0}`")]
    MissingRom(String),
    /// The staged item's shape (file, zip, directory) cannot be placed on this platform.
    #[error("staged item cannot be placed: {0}")]
    Unplaceable(&'static str),
    /// A DAT name cannot be turned into a safe file name.
    #[error("name `{0}` cannot be used as a file name")]
    InvalidName(String),
    /// An MRA file is not well-formed XML.
    #[error("MRA is not valid XML: {0}")]
    Mra(String),
    /// An MRA rom uses content the assembler does not implement.
    #[error("MRA content not supported: {0}")]
    MraUnsupported(String),
    /// A named MRA part is in none of the zips it may come from.
    #[error("part `{part}` is not in {zips}")]
    MissingPart {
        /// Member name.
        part: String,
        /// The zips tried, `|`-separated.
        zips: String,
    },
    /// A Neo Geo `romsets.xml` is not well-formed XML.
    #[error("romsets.xml is not valid XML: {0}")]
    Romsets(String),
    /// Reading a board file failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;
