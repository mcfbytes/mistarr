//! The mistarr server: config, database, jobs, SSE and the HTTP API.
//! See `docs/ARCHITECTURE.md`; the binary in `main.rs` only parses flags and calls [`app::start`].

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]

pub mod app;
pub mod bench;
pub mod cli;
pub mod client;
pub mod config;
pub mod db;
pub mod doctor;
mod error;
pub mod events;
pub mod http;
pub mod incoming;
pub mod jobs;
pub mod lock;
pub mod logging;
pub mod memory;
pub mod status;
pub mod synth;
pub mod threads;

pub use error::{Error, Result};

/// Seconds since the Unix epoch, 0 if the clock is before it.
///
/// ```
/// assert!(mistarr_server::unix_now() > 1_600_000_000);
/// ```
#[must_use]
pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}
