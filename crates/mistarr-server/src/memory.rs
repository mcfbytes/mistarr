//! The process memory ceiling and runtime sizing; see `docs/ARCHITECTURE.md` "Resource budgets".

use rustix::process::{getrlimit, setrlimit, Resource, Rlimit};

use crate::error::Result;

/// tokio worker threads.
pub const WORKERS: usize = 2;

/// Cap on threads running blocking SQLite, hashing and file work.
pub const BLOCKING_THREADS: usize = 4;

/// Stack reserved per runtime thread; the deepest paths are SQLite and bounded parsers.
pub const THREAD_STACK_BYTES: usize = 1024 * 1024;

/// Smallest limit set, in MiB; below it the runtime cannot start its threads and would
/// hang rather than fail.
pub const MIN_DATA_LIMIT_MIB: u64 = 64;

/// The soft limit in force after asking for `mib` MiB given the inherited limits `now`: the
/// lowest of `mib` raised to [`MIN_DATA_LIMIT_MIB`], the inherited soft limit and the hard
/// limit. An inherited soft limit lower than that is kept, even below the floor; 0 asks for
/// nothing and keeps the inherited soft limit. `None` means unlimited.
///
/// ```
/// use rustix::process::Rlimit;
/// let open = Rlimit { current: None, maximum: None };
/// assert_eq!(mistarr_server::memory::soft_limit(192, open), Some(192 << 20));
/// assert_eq!(mistarr_server::memory::soft_limit(2, open), Some(64 << 20));
/// assert_eq!(mistarr_server::memory::soft_limit(0, open), None);
/// ```
#[must_use]
pub fn soft_limit(mib: u64, now: Rlimit) -> Option<u64> {
    let wanted = (mib > 0)
        .then(|| mib.max(MIN_DATA_LIMIT_MIB))
        .and_then(|m| m.checked_mul(1024 * 1024));
    [wanted, now.current, now.maximum]
        .into_iter()
        .flatten()
        .min()
}

/// Lowers the soft `RLIMIT_DATA` to `mib` MiB, so heap and anonymous mappings past it fail
/// inside this process rather than pushing a board without swap into reclaim. 0 leaves the
/// limit alone. Returns the soft limit in force, `None` for unlimited.
///
/// # Errors
///
/// [`crate::Error::Io`] when the kernel refuses the new limit.
///
/// ```
/// let now = mistarr_server::memory::limit_data(0).unwrap();
/// assert_eq!(now, rustix::process::getrlimit(rustix::process::Resource::Data).current);
/// ```
pub fn limit_data(mib: u64) -> Result<Option<u64>> {
    let now = getrlimit(Resource::Data);
    let soft = soft_limit(mib, now);
    if soft != now.current {
        setrlimit(
            Resource::Data,
            Rlimit {
                current: soft,
                maximum: now.maximum,
            },
        )
        .map_err(std::io::Error::from)?;
    }
    Ok(soft)
}

/// The runtime the binary serves on: [`WORKERS`] workers, at most [`BLOCKING_THREADS`]
/// blocking threads, each with a [`THREAD_STACK_BYTES`] stack and named by
/// [`crate::threads::runtime_thread_name`].
///
/// # Errors
///
/// [`crate::Error::Io`] when the threads cannot be started.
///
/// ```
/// let rt = mistarr_server::memory::runtime().unwrap();
/// assert_eq!(rt.block_on(async { 2 + 2 }), 4);
/// ```
pub fn runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_multi_thread()
        .worker_threads(WORKERS)
        .max_blocking_threads(BLOCKING_THREADS)
        .thread_stack_size(THREAD_STACK_BYTES)
        .thread_name_fn(crate::threads::runtime_thread_name)
        .enable_all()
        .build()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_soft_limit_only_ever_goes_down() {
        let limit = |current, maximum| Rlimit { current, maximum };
        let mib = 1024 * 1024;
        assert_eq!(soft_limit(192, limit(None, None)), Some(192 * mib));
        assert_eq!(soft_limit(192, limit(Some(64 * mib), None)), Some(64 * mib));
        assert_eq!(soft_limit(192, limit(Some(8 * mib), None)), Some(8 * mib));
        assert_eq!(
            soft_limit(192, limit(None, Some(100 * mib))),
            Some(100 * mib)
        );
        assert_eq!(soft_limit(0, limit(Some(7), Some(9))), Some(7));
        assert_eq!(soft_limit(1, limit(None, None)), Some(64 * mib));
        assert_eq!(soft_limit(u64::MAX, limit(None, None)), None);
    }

    #[test]
    fn a_zero_limit_changes_nothing() {
        let before = getrlimit(Resource::Data);
        assert_eq!(limit_data(0).expect("limit"), before.current);
        assert_eq!(getrlimit(Resource::Data), before);
    }

    #[test]
    fn the_runtime_runs_blocking_work() {
        let rt = runtime().expect("runtime");
        let n = rt.block_on(async { tokio::task::spawn_blocking(|| 7).await.expect("join") });
        assert_eq!(n, 7);
    }
}
