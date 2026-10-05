//! The mistarr server: config, database, jobs, SSE and the HTTP API.
//! See `docs/ARCHITECTURE.md`; the binary in `main.rs` only parses flags and calls [`app::start`].

pub mod app;
pub mod cli;
pub mod config;
mod error;

/// Declares modules that are crate-private, and public with `test-support` so
/// integration tests and doctests reach them.
macro_rules! test_visible {
    ($($name:ident),* $(,)?) => {$(
        #[cfg(feature = "test-support")]
        pub mod $name;
        #[cfg(not(feature = "test-support"))]
        pub(crate) mod $name;
    )*};
}

test_visible!(
    bench,
    client,
    db,
    doctor,
    events,
    freeze,
    http,
    incoming,
    jobs,
    lock,
    logging,
    memory,
    migrating,
    status,
    synth,
    threads,
    version
);

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
