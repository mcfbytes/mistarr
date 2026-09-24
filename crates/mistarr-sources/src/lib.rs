//! User-supplied sources: the watched `sources/` directory, `.torrent` and
//! `.magnet` parsing, and binding a torrent's file list to a platform by
//! overlap with loaded DATs.
//!
//! This crate never fetches anything. See `docs/PRINCIPLES.md` and
//! `docs/WORKPLAN.md` WP-05.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub use mistarr_core::PlatformId;
