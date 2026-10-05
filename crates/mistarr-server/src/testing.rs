//! Polling helpers shared by unit tests and integration tests.

use std::future::Future;
use std::time::{Duration, Instant};

/// How long [`eventually`] polls before it fails.
pub const PATIENCE: Duration = Duration::from_secs(10);

const POLL: Duration = Duration::from_millis(10);

/// Polls `f` every 10 ms until it returns true.
///
/// # Panics
///
/// After [`PATIENCE`] without `f` returning true.
pub async fn eventually<F, Fut>(what: &str, f: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    eventually_within(what, PATIENCE, f).await;
}

/// [`eventually`] with its own time limit.
///
/// # Panics
///
/// When `limit` passes before `f` returns true.
pub async fn eventually_within<F, Fut>(what: &str, limit: Duration, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let start = Instant::now();
    while !f().await {
        assert!(
            start.elapsed() < limit,
            "timed out after {limit:?} waiting for {what}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// [`eventually`] for a condition that needs no `await`, sleeping the thread between polls.
///
/// # Panics
///
/// After [`PATIENCE`] without `f` returning true.
pub fn eventually_blocking(what: &str, f: impl FnMut() -> bool) {
    eventually_blocking_within(what, PATIENCE, f);
}

/// [`eventually_blocking`] with its own time limit.
///
/// # Panics
///
/// When `limit` passes before `f` returns true.
pub fn eventually_blocking_within(what: &str, limit: Duration, mut f: impl FnMut() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(
            start.elapsed() < limit,
            "timed out after {limit:?} waiting for {what}"
        );
        std::thread::sleep(POLL);
    }
}

/// The MD5 of `parts` read as one stream.
#[must_use]
pub fn md5_of(parts: &[&[u8]]) -> mistarr_core::Md5 {
    let mut m = mistarr_core::hash::Md5Stream::new();
    for p in parts {
        m.update(p);
    }
    m.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn polls_until_the_condition_holds() {
        let mut calls = 0;
        eventually("the third call", || {
            calls += 1;
            let done = calls == 3;
            async move { done }
        })
        .await;
        assert_eq!(calls, 3);
    }

    #[tokio::test]
    #[should_panic(expected = "timed out")]
    async fn fails_after_its_limit() {
        eventually_within("never", Duration::from_millis(30), || async { false }).await;
    }

    #[test]
    fn blocking_polls_until_the_condition_holds() {
        let mut calls = 0;
        eventually_blocking("the second call", || {
            calls += 1;
            calls == 2
        });
        assert_eq!(calls, 2);
    }

    #[test]
    fn md5_of_joins_its_parts() {
        assert_eq!(md5_of(&[b"a", b"b"]), md5_of(&[b"ab"]));
        assert_ne!(md5_of(&[b"a"]), md5_of(&[b"b"]));
    }
}
