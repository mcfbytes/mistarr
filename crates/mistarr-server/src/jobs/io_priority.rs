//! Idle I/O class while a core runs; see `docs/ARCHITECTURE.md` "Resource budgets".

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use super::gate::Gate;

/// Where Linux lists this process's threads.
pub const TASK_DIR: &str = "/proc/self/task";

/// Where Linux links the calling thread's own `/proc/<pid>/task/<tid>` entry.
const THREAD_SELF: &str = "/proc/thread-self";

/// Listings of the task directory per switch; a pass that finds no new thread ends it.
const MAX_PASSES: usize = 4;

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
/// how many threads took the class; a thread the tool refuses is skipped.
///
/// # Errors
///
/// [`PriorityError::NoTool`] or [`PriorityError::Threads`], when no switch can succeed.
pub fn apply(
    setter: &dyn SetClass,
    task_dir: &Path,
    class: IoClass,
) -> Result<usize, PriorityError> {
    let mut seen = HashSet::new();
    let mut set = 0;
    for _ in 0..MAX_PASSES {
        let fresh: Vec<u32> = thread_ids(task_dir)?
            .into_iter()
            .filter(|tid| seen.insert(*tid))
            .collect();
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
/// Switches and default-class launches hold the class lock, so they never interleave.
pub struct IoPriority {
    setter: Arc<dyn SetClass>,
    task_dir: PathBuf,
    current: Mutex<IoClass>,
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
            current: Mutex::new(IoClass::Default),
        }
    }

    /// Sets `class` on every thread through [`apply`] and records it.
    ///
    /// # Errors
    ///
    /// As [`apply`]; the recorded class stays as it was.
    pub fn switch(&self, class: IoClass) -> Result<usize, PriorityError> {
        let mut current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
        let set = apply(self.setter.as_ref(), &self.task_dir, class)?;
        *current = class;
        Ok(set)
    }

    /// Runs `f` with the calling thread in the default class, so a process it
    /// starts does not inherit the idle class, then sets the current class on
    /// every thread again, covering any thread `f` left behind.
    pub fn at_default<T>(&self, f: impl FnOnce() -> T) -> T {
        let current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
        if *current == IoClass::Default {
            return f();
        }
        if let Some(tid) = current_tid() {
            if let Err(e) = self.setter.set(tid, IoClass::Default) {
                tracing::debug!(tid, error = %e, "launching in the idle I/O class");
            }
        }
        let out = f();
        if let Err(e) = apply(self.setter.as_ref(), &self.task_dir, *current) {
            tracing::debug!(error = %e, "I/O class not restored after the launch");
        }
        out
    }
}

/// Follows `gate`: idle I/O class while a core runs, the default otherwise.
/// Returns when the gate is dropped or when the class cannot be set at all.
pub async fn follow(gate: Arc<Gate>, priority: Arc<IoPriority>) {
    let mut rx = gate.subscribe();
    let mut current = IoClass::Default;
    loop {
        let want = IoClass::for_core(rx.borrow_and_update().core_running());
        if want != current {
            let priority = Arc::clone(&priority);
            let applied = tokio::task::spawn_blocking(move || priority.switch(want)).await;
            match applied {
                Ok(Ok(threads)) => tracing::info!(class = ?want, threads, "I/O class set"),
                Ok(Err(e)) => {
                    tracing::debug!(error = %e, "I/O class stays as launched");
                    return;
                }
                Err(e) => tracing::debug!(error = %e, "I/O class switch did not finish"),
            }
            current = want;
        }
        if rx.changed().await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records calls; optionally adds a thread entry while switching, or has no tool.
    #[derive(Default)]
    struct Fake {
        calls: Mutex<Vec<(u32, IoClass)>>,
        spawn_into: Option<PathBuf>,
        missing: bool,
    }

    impl SetClass for Fake {
        fn set(&self, tid: u32, class: IoClass) -> Result<(), PriorityError> {
            if self.missing {
                return Err(PriorityError::NoTool);
            }
            let mut calls = self.calls.lock().expect("lock");
            if let Some(dir) = &self.spawn_into {
                let _ = std::fs::create_dir(dir.join("900"));
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
        let task = tokio::spawn(follow(Arc::clone(&gate), Arc::new(priority)));
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
        assert_eq!(priority.switch(IoClass::Idle).expect("switch"), 2);
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

    #[test]
    fn current_tid_is_one_of_this_process_threads() {
        let tid = std::thread::spawn(current_tid).join().expect("join");
        let tid = tid.expect("thread-self");
        assert_ne!(tid, std::process::id());
        assert!(tid > 0);
    }
}
