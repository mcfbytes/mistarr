//! The `DownloadClient` trait and its Transmission and rtorrent
//! implementations. mistarr never embeds a torrent client; it drives the one
//! already on the image. See `docs/DOWNLOAD-CLIENTS.md` and
//! `docs/WORKPLAN.md` WP-06 and WP-07.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

/// Seeding policy for one source, chosen by the user (PRINCIPLES.md §4).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SeedPolicy {
    /// Stop as soon as every wanted file has been imported.
    None,
    /// Seed until the client reports this upload ratio, then stop.
    Ratio {
        /// Upload divided by download, e.g. `1.0`.
        ratio: f32,
    },
    /// Leave seeding to the client's own configuration.
    Client,
}
