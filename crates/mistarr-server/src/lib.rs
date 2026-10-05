//! The mistarr server: config, database, jobs, SSE and the HTTP API.
//! See `docs/ARCHITECTURE.md`; the binary in `main.rs` only parses flags and calls [`cli::run`].

pub mod app;
pub mod cli;
pub mod config;
mod error;

#[cfg(feature = "test-support")]
pub mod bench;
#[cfg(not(feature = "test-support"))]
pub(crate) mod bench;
#[cfg(feature = "test-support")]
pub mod client;
#[cfg(not(feature = "test-support"))]
pub(crate) mod client;
#[cfg(feature = "test-support")]
pub mod db;
#[cfg(not(feature = "test-support"))]
pub(crate) mod db;
#[cfg(feature = "test-support")]
pub mod doctor;
#[cfg(not(feature = "test-support"))]
pub(crate) mod doctor;
#[cfg(feature = "test-support")]
pub mod events;
#[cfg(not(feature = "test-support"))]
pub(crate) mod events;
#[cfg(feature = "test-support")]
pub mod freeze;
#[cfg(not(feature = "test-support"))]
pub(crate) mod freeze;
#[cfg(feature = "test-support")]
pub mod http;
#[cfg(not(feature = "test-support"))]
pub(crate) mod http;
#[cfg(feature = "test-support")]
pub mod incoming;
#[cfg(not(feature = "test-support"))]
pub(crate) mod incoming;
#[cfg(feature = "test-support")]
pub mod jobs;
#[cfg(not(feature = "test-support"))]
pub(crate) mod jobs;
#[cfg(feature = "test-support")]
pub mod lock;
#[cfg(not(feature = "test-support"))]
pub(crate) mod lock;
#[cfg(feature = "test-support")]
pub mod logging;
#[cfg(not(feature = "test-support"))]
pub(crate) mod logging;
#[cfg(feature = "test-support")]
pub mod memory;
#[cfg(not(feature = "test-support"))]
pub(crate) mod memory;
#[cfg(feature = "test-support")]
pub mod migrating;
#[cfg(not(feature = "test-support"))]
pub(crate) mod migrating;
#[cfg(feature = "test-support")]
pub mod status;
#[cfg(not(feature = "test-support"))]
pub(crate) mod status;
#[cfg(feature = "test-support")]
pub mod synth;
#[cfg(not(feature = "test-support"))]
pub(crate) mod synth;
#[cfg(feature = "test-support")]
pub mod threads;
#[cfg(not(feature = "test-support"))]
pub(crate) mod threads;
#[cfg(feature = "test-support")]
pub mod version;
#[cfg(not(feature = "test-support"))]
pub(crate) mod version;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;

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
