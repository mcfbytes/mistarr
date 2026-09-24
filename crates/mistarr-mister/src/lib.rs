//! MiSTer specifics: the DAT-name to `games/<Core>` table, the `CoreAdapter`
//! trait and its implementations, installed-core detection, the
//! `/tmp/CORENAME` watcher and MRA parsing.
//!
//! See `docs/PLATFORMS.md` and `docs/WORKPLAN.md` WP-04 and WP-19.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub use mistarr_core::PlatformId;

/// Path of the file MiSTer writes the running core's name to. `MENU` means
/// no core is running.
pub const CORENAME_PATH: &str = "/tmp/CORENAME";
