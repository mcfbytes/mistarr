//! Idle I/O class while a core runs; see `docs/ARCHITECTURE.md` "Resource budgets".

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::Notify;

use super::gate::Gate;

/// Where Linux lists this process's threads.
pub const TASK_DIR: &str = "/proc/self/task";

/// Where Linux links the calling thread's own `/proc/<pid>/task/<tid>` entry.
const THREAD_SELF: &str = "/proc/thread-self";

/// Listings of the task directory per switch; a pass that finds no new thread ends it.
const MAX_PASSES: usize = 4;

/// How long [`follow`] waits before trying a switch that failed again.
pub const RETRY: Duration = Duration::from_secs(30);

/// The longest [`follow`] waits between retries after repeated failures.
pub const MAX_RETRY: Duration = Duration::from_secs(240);

/// Linux `ETXTBSY`: exec of a file still open for writing somewhere.
const ETXTBSY: i32 = 26;

/// The I/O class a thread runs in, as `ionice -c` numbers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum IoClass {
    /// Class "none": the kernel derives a best-effort level from the thread's `nice`.
    Default,
    /// Class "idle": served only when no other process wants the disk.
    Idle,
}

impl IoClass {
    /// The class for a board with or without a core loaded.
    ///
    /// ```
    /// use mistarr_server::jobs::io_priority::IoClass;
    /// assert_eq!(IoClass::for_core(true), IoClass::Idle);
    /// assert_eq!(IoClass::for_core(false), IoClass::Default);
    /// ```
    #[must_use]
    pub fn for_core(core_running: bool) -> Self {
        if core_running {
            Self::Idle
        } else {
            Self::Default
        }
    }

    /// The `ionice -c` argument, which `BusyBox` and util-linux both accept.
    ///
    /// ```
    /// assert_eq!(mistarr_server::jobs::io_priority::IoClass::Idle.arg(), "3");
    /// ```
    #[must_use]
    pub fn arg(self) -> &'static str {
        match self {
            Self::Default => "0",
            Self::Idle => "3",
        }
    }
}

/// Why a thread's class could not be set.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PriorityError {
    /// The tool is not installed; no later switch can succeed either.
    #[error("ionice is not installed")]
    NoTool,
    /// The thread list could not be read.
    #[error("cannot list threads: {0}")]
    Threads(#[source] std::io::Error),
    /// The tool ran and refused, for example because the thread has exited.
    #[error("ionice failed: {0}")]
    Failed(String),
}

/// Sets one thread's I/O class.
pub trait SetClass: Send + Sync {
    /// Applies `class` to thread `tid`.
    ///
    /// # Errors
    ///
    /// [`PriorityError::NoTool`] when nothing can set it, [`PriorityError::Failed`] otherwise.
    fn set(&self, tid: u32, class: IoClass) -> Result<(), PriorityError>;
}

/// Runs `ionice -c CLASS -p TID`, a form `BusyBox` and util-linux both accept.
#[derive(Debug, Clone)]
pub struct Ionice {
    program: PathBuf,
}

impl Ionice {
    /// Uses `program`, looked up on `PATH` when it has no directory part.
    ///
    /// ```
    /// let _ = mistarr_server::jobs::io_priority::Ionice::new(std::path::Path::new("ionice"));
    /// ```
    #[must_use]
    pub fn new(program: &Path) -> Self {
        Self {
            program: program.to_path_buf(),
        }
    }
}

impl SetClass for Ionice {
    fn set(&self, tid: u32, class: IoClass) -> Result<(), PriorityError> {
        let mut cmd = Command::new(&self.program);
        cmd.args(["-c", class.arg(), "-p", &tid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut tries = 0u64;
        let out = loop {
            match cmd.output() {
                Ok(out) => break out,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Err(PriorityError::NoTool)
                }
                // A script still open for writing elsewhere fails exec with ETXTBSY; it clears.
                Err(e) if e.raw_os_error() == Some(ETXTBSY) && tries < 5 => {
                    tries += 1;
                    std::thread::sleep(Duration::from_millis(20 * tries));
                }
                Err(e) => return Err(PriorityError::Failed(e.to_string())),
            }
        };
        if out.status.success() {
            Ok(())
        } else {
            let why = String::from_utf8_lossy(&out.stderr);
            Err(PriorityError::Failed(format!(
                "{}: {}",
                out.status,
                why.trim()
            )))
        }
    }
}

/// The numeric entries of `task_dir`, which for `/proc/self/task` are this process's thread ids.
///
/// # Errors
///
/// [`PriorityError::Threads`] when the directory cannot be read.
///
/// ```
/// let dir = std::path::Path::new(mistarr_server::jobs::io_priority::TASK_DIR);
/// let tids = mistarr_server::jobs::io_priority::thread_ids(dir).unwrap();
/// assert!(tids.contains(&std::process::id()));
/// ```
pub fn thread_ids(task_dir: &Path) -> Result<Vec<u32>, PriorityError> {
    let mut tids = Vec::new();
    for entry in std::fs::read_dir(task_dir).map_err(PriorityError::Threads)? {
        let entry = entry.map_err(PriorityError::Threads)?;
        if let Some(tid) = entry.file_name().to_str().and_then(|n| n.parse().ok()) {
            tids.push(tid);
        }
    }
    tids.sort_unstable();
    Ok(tids)
}

/// Sets `class` on every thread in `task_dir`, listing again until a pass finds
/// no new thread, so one spawned by a thread not yet switched is caught too.
/// Threads created afterwards inherit the class from their creator. Returns
/// how many threads took the class; a thread the tool refuses is skipped, and
/// a listing that fails after the first ends the passes.
///
/// # Errors
///
/// [`PriorityError::NoTool`], or [`PriorityError::Threads`] when the first listing fails.
pub fn apply(
    setter: &dyn SetClass,
    task_dir: &Path,
    class: IoClass,
) -> Result<usize, PriorityError> {
    let mut seen = HashSet::new();
    let mut set = 0;
    for pass in 0..MAX_PASSES {
        let tids = match thread_ids(task_dir) {
            Ok(tids) => tids,
            Err(e) if pass == 0 => return Err(e),
            Err(e) => {
                tracing::debug!(error = %e, "thread list ended early");
                break;
            }
        };
        let fresh: Vec<u32> = tids.into_iter().filter(|tid| seen.insert(*tid)).collect();
        if fresh.is_empty() {
            break;
        }
        for tid in fresh {
            match setter.set(tid, class) {
                Ok(()) => set += 1,
                Err(PriorityError::NoTool) => return Err(PriorityError::NoTool),
                Err(e) => tracing::debug!(tid, error = %e, "thread kept its I/O class"),
            }
        }
    }
    Ok(set)
}

/// The calling thread's id, read from `/proc/thread-self`; `None` without `/proc`.
///
/// ```
/// let tid = mistarr_server::jobs::io_priority::current_tid();
/// assert!(tid.is_none() || tid.is_some_and(|t| t > 0));
/// ```
#[must_use]
pub fn current_tid() -> Option<u32> {
    let link = std::fs::read_link(THREAD_SELF).ok()?;
    link.file_name()?.to_str()?.parse().ok()
}

/// The daemon's I/O class: the setter, the thread list and the class last set.
/// Switches and default-class launches hold the class lock, so they never interleave;
/// only blocking threads take it.
pub struct IoPriority {
    setter: Arc<dyn SetClass>,
    task_dir: PathBuf,
    current: Mutex<Option<IoClass>>,
    stale: Notify,
}

impl IoPriority {
    /// Starts at [`IoClass::Default`], the class the launcher leaves the daemon in.
    ///
    /// ```
    /// use mistarr_server::jobs::io_priority::{IoPriority, Ionice, TASK_DIR};
    /// let ionice = std::sync::Arc::new(Ionice::new(std::path::Path::new("ionice")));
    /// let _ = IoPriority::new(ionice, std::path::Path::new(TASK_DIR));
    /// ```
    #[must_use]
    pub fn new(setter: Arc<dyn SetClass>, task_dir: &Path) -> Self {
        Self {
            setter,
            task_dir: task_dir.to_path_buf(),
            current: Mutex::new(Some(IoClass::Default)),
            stale: Notify::new(),
        }
    }

    /// The class last recorded, or `None` when a restore after a launch failed
    /// and threads may be in either class. Blocks while a switch or launch runs.
    ///
    /// ```
    /// use mistarr_server::jobs::io_priority::{IoClass, IoPriority, Ionice, TASK_DIR};
    /// let ionice = std::sync::Arc::new(Ionice::new(std::path::Path::new("ionice")));
    /// let priority = IoPriority::new(ionice, std::path::Path::new(TASK_DIR));
    /// assert_eq!(priority.class(), Some(IoClass::Default));
    /// ```
    #[must_use]
    pub fn class(&self) -> Option<IoClass> {
        *self.current.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Sets `class` on every thread through [`apply`] unless it is already the
    /// recorded class, in which case it returns `Ok(None)` without touching a
    /// thread. The switch counts, and `class` is recorded, once at least one
    /// thread takes it; threads the tool refused keep the old class until the
    /// next switch.
    ///
    /// # Errors
    ///
    /// As [`apply`], or [`PriorityError::Failed`] when no thread took the class;
    /// the recorded class stays as it was.
    pub fn switch(&self, class: IoClass) -> Result<Option<usize>, PriorityError> {
        let mut current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
        if *current == Some(class) {
            return Ok(None);
        }
        let set = apply(self.setter.as_ref(), &self.task_dir, class)?;
        if set == 0 {
            return Err(PriorityError::Failed("no thread took the class".into()));
        }
        *current = Some(class);
        Ok(Some(set))
    }

    /// Runs `f` with the calling thread in the default class, so a process it
    /// starts does not inherit the idle class, then sets the recorded class on
    /// every thread again. When no thread takes it, the recorded class becomes
    /// `None` and [`follow`] is woken to switch again.
    pub fn at_default<T>(&self, f: impl FnOnce() -> T) -> T {
        let mut current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
        if *current == Some(IoClass::Default) {
            return f();
        }
        if let Some(tid) = current_tid() {
            if let Err(e) = self.setter.set(tid, IoClass::Default) {
                tracing::debug!(tid, error = %e, "launching in the idle I/O class");
            }
        }
        let out = f();
        let restored = current.map(|class| apply(self.setter.as_ref(), &self.task_dir, class));
        if !matches!(restored, Some(Ok(n)) if n > 0) {
            tracing::debug!(result = ?restored, "I/O class not restored after the launch");
            *current = None;
            self.stale.notify_one();
        }
        out
    }
}

/// Follows `gate`: idle I/O class while a core runs, the default otherwise.
/// A failed switch is tried again after `retry`, doubling after each further
/// failure up to [`MAX_RETRY`], or at once on a gate change or a failed
/// restore; returns when the gate is dropped or `ionice` is absent.
pub async fn follow(gate: Arc<Gate>, priority: Arc<IoPriority>, retry: Duration) {
    let mut rx = gate.subscribe();
    let mut delay = retry;
    let mut logged = None;
    loop {
        let want = IoClass::for_core(rx.borrow_and_update().core_running());
        let switcher = Arc::clone(&priority);
        let attempt = crate::threads::blocking(crate::threads::label::IO_CLASS, move || {
            switcher.switch(want)
        });
        let failed = match attempt.await {
            Ok(Ok(set)) => {
                if let Some(threads) = set {
                    tracing::info!(class = ?want, threads, "I/O class set");
                }
                false
            }
            Ok(Err(PriorityError::NoTool)) => {
                tracing::debug!("ionice is not installed; I/O class stays as launched");
                return;
            }
            Ok(Err(e)) => {
                if logged != Some(delay) {
                    tracing::debug!(error = %e, retry = ?delay, "I/O class switch failed");
                    logged = Some(delay);
                }
                true
            }
            Err(e) => {
                tracing::debug!(error = %e, "I/O class switch did not finish");
                true
            }
        };
        if !failed {
            delay = retry;
            logged = None;
        }
        tokio::select! {
            changed = rx.changed() => {
                if changed.is_err() {
                    return;
                }
                delay = retry;
                logged = None;
            }
            () = priority.stale.notified() => {}
            () = tokio::time::sleep(delay), if failed => {
                delay = delay.saturating_mul(2).min(MAX_RETRY.max(retry));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

    const ALWAYS: usize = usize::MAX;

    /// Records calls; optionally adds a thread entry or removes the thread list
    /// while switching, refuses the next `refuse` calls, or has no tool.
    #[derive(Default)]
    struct Fake {
        calls: Mutex<Vec<(u32, IoClass)>>,
        spawn_into: Option<PathBuf>,
        remove: Option<PathBuf>,
        refuse: AtomicUsize,
        missing: bool,
    }

    impl SetClass for Fake {
        fn set(&self, tid: u32, class: IoClass) -> Result<(), PriorityError> {
            if self.missing {
                return Err(PriorityError::NoTool);
            }
            let refused = self
                .refuse
                .fetch_update(SeqCst, SeqCst, |n| n.checked_sub(1))
                .is_ok();
            if refused {
                return Err(PriorityError::Failed("try again".into()));
            }
            let mut calls = self.calls.lock().expect("lock");
            if let Some(dir) = &self.spawn_into {
                let _ = std::fs::create_dir(dir.join("900"));
            }
            if let Some(dir) = &self.remove {
                let _ = std::fs::remove_dir_all(dir);
            }
            if tid == 13 {
                return Err(PriorityError::Failed("no such process".into()));
            }
            calls.push((tid, class));
            Ok(())
        }
    }

    fn tasks(ids: &[u32]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        for id in ids {
            std::fs::create_dir(dir.path().join(id.to_string())).expect("mkdir");
        }
        std::fs::create_dir(dir.path().join("not-a-tid")).expect("mkdir");
        dir
    }

    #[test]
    fn every_thread_takes_the_class_including_one_spawned_meanwhile() {
        let dir = tasks(&[101, 102, 13]);
        let fake = Fake {
            spawn_into: Some(dir.path().to_path_buf()),
            ..Fake::default()
        };
        assert_eq!(apply(&fake, dir.path(), IoClass::Idle).expect("apply"), 3);
        let calls = fake.calls.lock().expect("lock").clone();
        assert_eq!(
            calls,
            vec![
                (101, IoClass::Idle),
                (102, IoClass::Idle),
                (900, IoClass::Idle)
            ]
        );
    }

    #[test]
    fn a_missing_tool_or_thread_list_stops_the_switch() {
        let dir = tasks(&[1]);
        let fake = Fake {
            missing: true,
            ..Fake::default()
        };
        assert!(matches!(
            apply(&fake, dir.path(), IoClass::Idle),
            Err(PriorityError::NoTool)
        ));
        assert!(matches!(
            apply(&Fake::default(), &dir.path().join("absent"), IoClass::Idle),
            Err(PriorityError::Threads(_))
        ));
    }

    #[test]
    fn a_thread_list_lost_after_the_first_pass_still_records_the_class() {
        let dir = tasks(&[5, 6]);
        let fake = Arc::new(Fake {
            remove: Some(dir.path().to_path_buf()),
            ..Fake::default()
        });
        let priority = IoPriority::new(Arc::clone(&fake) as Arc<dyn SetClass>, dir.path());
        assert_eq!(priority.switch(IoClass::Idle).expect("switch"), Some(2));
        assert_eq!(priority.class(), Some(IoClass::Idle));
        assert!(matches!(
            priority.switch(IoClass::Default),
            Err(PriorityError::Threads(_))
        ));
        assert_eq!(priority.class(), Some(IoClass::Idle));
    }

    #[test]
    fn a_switch_no_thread_takes_keeps_the_recorded_class() {
        let dir = tasks(&[5]);
        let fake = Arc::new(Fake::default());
        fake.refuse.store(ALWAYS, SeqCst);
        let priority = IoPriority::new(Arc::clone(&fake) as Arc<dyn SetClass>, dir.path());
        assert!(matches!(
            priority.switch(IoClass::Idle),
            Err(PriorityError::Failed(_))
        ));
        assert_eq!(priority.class(), Some(IoClass::Default));
    }

    #[tokio::test]
    async fn a_failed_switch_is_retried_and_the_menu_restores() {
        let dir = tasks(&[5]);
        let gate = Arc::new(Gate::new());
        let fake = Arc::new(Fake::default());
        fake.refuse.store(ALWAYS, SeqCst);
        let priority = Arc::new(IoPriority::new(
            Arc::clone(&fake) as Arc<dyn SetClass>,
            dir.path(),
        ));
        let task = tokio::spawn(follow(
            Arc::clone(&gate),
            Arc::clone(&priority),
            Duration::from_millis(20),
        ));
        gate.set_corename(Some("SNES".into()));
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(priority.class(), Some(IoClass::Default));
        fake.refuse.store(0, SeqCst);
        for _ in 0..200 {
            if priority.class() == Some(IoClass::Idle) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(priority.class(), Some(IoClass::Idle));
        gate.set_corename(Some("MENU".into()));
        for _ in 0..200 {
            if priority.class() == Some(IoClass::Default) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let calls = fake.calls.lock().expect("lock").clone();
        assert_eq!(calls, vec![(5, IoClass::Idle), (5, IoClass::Default)]);
        task.abort();
    }

    #[test]
    fn thread_ids_lists_numeric_entries() {
        let dir = tasks(&[7, 3]);
        assert_eq!(thread_ids(dir.path()).expect("list"), vec![3, 7]);
    }

    #[test]
    fn classes_map_to_ionice_numbers() {
        assert_eq!(IoClass::Default.arg(), "0");
        assert_eq!(IoClass::for_core(false), IoClass::Default);
    }

    #[cfg(unix)]
    #[test]
    fn ionice_is_called_with_class_and_thread() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("log");
        let shim = dir.path().join("ionice");
        let script = format!(
            "#!/bin/sh\necho \"$@\" >> '{}'\n[ \"$4\" = 13 ] && exit 1\nexit 0\n",
            log.display()
        );
        std::fs::write(&shim, script).expect("write");
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let ionice = Ionice::new(&shim);
        ionice.set(42, IoClass::Idle).expect("idle");
        ionice.set(42, IoClass::Default).expect("default");
        assert!(matches!(
            ionice.set(13, IoClass::Idle),
            Err(PriorityError::Failed(_))
        ));
        let lines = std::fs::read_to_string(&log).expect("log");
        assert_eq!(lines, "-c 3 -p 42\n-c 0 -p 42\n-c 3 -p 13\n");
        let absent = Ionice::new(&dir.path().join("absent"));
        assert!(matches!(
            absent.set(1, IoClass::Idle),
            Err(PriorityError::NoTool)
        ));
    }

    #[tokio::test]
    async fn follows_the_core_and_restores_at_the_menu() {
        let dir = tasks(&[5, 6]);
        let gate = Arc::new(Gate::new());
        let fake = Arc::new(Fake::default());
        let priority = IoPriority::new(Arc::clone(&fake) as Arc<dyn SetClass>, dir.path());
        let task = tokio::spawn(follow(Arc::clone(&gate), Arc::new(priority), RETRY));
        let wait_for = |n: usize| {
            let fake = Arc::clone(&fake);
            async move {
                for _ in 0..200 {
                    if fake.calls.lock().expect("lock").len() >= n {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                panic!("no switch");
            }
        };
        gate.set_corename(Some("MENU".into()));
        gate.set_corename(Some("SNES".into()));
        wait_for(2).await;
        gate.set_corename(Some("MENU".into()));
        wait_for(4).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        let calls = fake.calls.lock().expect("lock").clone();
        assert_eq!(
            calls,
            vec![
                (5, IoClass::Idle),
                (6, IoClass::Idle),
                (5, IoClass::Default),
                (6, IoClass::Default)
            ]
        );
        task.abort();
    }

    #[test]
    fn a_launch_while_idle_runs_at_default_then_restores_every_thread() {
        let dir = tasks(&[5, 6]);
        let fake = Arc::new(Fake::default());
        let priority = IoPriority::new(Arc::clone(&fake) as Arc<dyn SetClass>, dir.path());
        assert_eq!(priority.at_default(|| 1), 1);
        assert!(fake.calls.lock().expect("lock").is_empty());
        assert_eq!(priority.switch(IoClass::Idle).expect("switch"), Some(2));
        fake.calls.lock().expect("lock").clear();
        let tid = current_tid().expect("thread-self");
        let calls = Arc::clone(&fake);
        let seen = priority.at_default(move || calls.calls.lock().expect("lock").clone());
        assert_eq!(seen, vec![(tid, IoClass::Default)]);
        let calls = fake.calls.lock().expect("lock").clone();
        assert_eq!(
            calls,
            vec![
                (tid, IoClass::Default),
                (5, IoClass::Idle),
                (6, IoClass::Idle)
            ]
        );
    }

    fn follower(
        fake: &Arc<Fake>,
        task_dir: &Path,
        retry: Duration,
    ) -> (Arc<Gate>, Arc<IoPriority>, tokio::task::JoinHandle<()>) {
        let gate = Arc::new(Gate::new());
        let setter = Arc::clone(fake) as Arc<dyn SetClass>;
        let priority = Arc::new(IoPriority::new(setter, task_dir));
        let task = tokio::spawn(follow(Arc::clone(&gate), Arc::clone(&priority), retry));
        (gate, priority, task)
    }

    async fn wait_for_class(priority: &IoPriority, want: IoClass) {
        for _ in 0..200 {
            if priority.class() == Some(want) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("class never became {want:?}");
    }

    #[test]
    fn a_switch_to_the_recorded_class_touches_no_thread() {
        let dir = tasks(&[5]);
        let fake = Arc::new(Fake::default());
        let priority = IoPriority::new(Arc::clone(&fake) as Arc<dyn SetClass>, dir.path());
        assert_eq!(priority.switch(IoClass::Default).expect("switch"), None);
        assert!(fake.calls.lock().expect("lock").is_empty());
    }

    #[tokio::test]
    async fn follow_never_blocks_the_runtime_while_a_launch_holds_the_class() {
        let dir = tasks(&[5]);
        let fake = Arc::new(Fake::default());
        let (gate, priority, task) = follower(&fake, dir.path(), RETRY);
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let launcher = Arc::clone(&priority);
        let launch = std::thread::spawn(move || {
            launcher.at_default(|| {
                entered_tx.send(()).expect("entered");
                let _ = release_rx.recv_timeout(Duration::from_secs(5));
            });
        });
        entered_rx.recv().expect("launch started");
        let start = std::time::Instant::now();
        gate.set_corename(Some("SNES".into()));
        tokio::time::sleep(Duration::from_millis(50)).await;
        tokio::spawn(async {}).await.expect("other task");
        assert!(start.elapsed() < Duration::from_secs(2));
        release_tx.send(()).expect("release");
        launch.join().expect("launch");
        wait_for_class(&priority, IoClass::Idle).await;
        task.abort();
    }

    #[tokio::test]
    async fn follow_returns_when_the_tool_is_missing() {
        let dir = tasks(&[5]);
        let fake = Arc::new(Fake {
            missing: true,
            ..Fake::default()
        });
        let (gate, _priority, task) = follower(&fake, dir.path(), RETRY);
        gate.set_corename(Some("SNES".into()));
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("follow returned")
            .expect("follow ran");
    }

    #[tokio::test]
    async fn follow_keeps_going_until_the_thread_list_appears() {
        let dir = tempfile::tempdir().expect("tempdir");
        let task_dir = dir.path().join("task");
        let fake = Arc::new(Fake::default());
        let (gate, priority, task) = follower(&fake, &task_dir, Duration::from_millis(10));
        gate.set_corename(Some("SNES".into()));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(priority.class(), Some(IoClass::Default));
        assert!(!task.is_finished());
        std::fs::create_dir_all(task_dir.join("5")).expect("mkdir");
        wait_for_class(&priority, IoClass::Idle).await;
        assert_eq!(*fake.calls.lock().expect("lock"), vec![(5, IoClass::Idle)]);
        task.abort();
    }

    #[tokio::test]
    async fn a_gate_change_retries_a_failed_switch_before_the_timer() {
        let dir = tasks(&[5]);
        let fake = Arc::new(Fake::default());
        fake.refuse.store(ALWAYS, SeqCst);
        let (gate, priority, task) = follower(&fake, dir.path(), RETRY);
        gate.set_corename(Some("SNES".into()));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(priority.class(), Some(IoClass::Default));
        fake.refuse.store(0, SeqCst);
        gate.set_corename(Some("NES".into()));
        wait_for_class(&priority, IoClass::Idle).await;
        task.abort();
    }

    #[tokio::test]
    async fn a_failed_restore_after_a_launch_is_switched_again() {
        let dir = tasks(&[5, 6]);
        let fake = Arc::new(Fake::default());
        let (gate, priority, task) = follower(&fake, dir.path(), RETRY);
        gate.set_corename(Some("SNES".into()));
        wait_for_class(&priority, IoClass::Idle).await;
        let launcher = Arc::clone(&priority);
        let refuse = Arc::clone(&fake);
        tokio::task::spawn_blocking(move || {
            launcher.at_default(|| refuse.refuse.store(2, SeqCst));
        })
        .await
        .expect("launch");
        for _ in 0..200 {
            if fake.calls.lock().expect("lock").len() >= 5 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let calls = fake.calls.lock().expect("lock").clone();
        assert_eq!(calls[3..], [(5, IoClass::Idle), (6, IoClass::Idle)]);
        wait_for_class(&priority, IoClass::Idle).await;
        task.abort();
    }

    #[test]
    fn a_launch_whose_restore_fails_records_no_class() {
        let dir = tasks(&[5]);
        let fake = Arc::new(Fake::default());
        let priority = IoPriority::new(Arc::clone(&fake) as Arc<dyn SetClass>, dir.path());
        priority.switch(IoClass::Idle).expect("switch");
        priority.at_default(|| fake.refuse.store(1, SeqCst));
        assert_eq!(priority.class(), None);
        std::fs::remove_dir_all(dir.path()).expect("rm");
        priority.at_default(|| ());
        assert_eq!(priority.class(), None);
    }

    #[test]
    fn current_tid_is_one_of_this_process_threads() {
        let tid = std::thread::spawn(current_tid).join().expect("join");
        let tid = tid.expect("thread-self");
        assert_ne!(tid, std::process::id());
        assert!(tid > 0);
    }
}
