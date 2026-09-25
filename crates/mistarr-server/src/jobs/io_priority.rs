//! Idle I/O class while a core runs; see `docs/ARCHITECTURE.md` "Pausing for the core".

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use super::gate::Gate;

/// Where Linux lists this process's threads.
pub const TASK_DIR: &str = "/proc/self/task";

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

/// Follows `gate`: idle I/O class while a core runs, the default otherwise.
/// Returns when the gate is dropped or when the class cannot be set at all.
pub async fn follow(gate: Arc<Gate>, setter: Arc<dyn SetClass>, task_dir: PathBuf) {
    let mut rx = gate.subscribe();
    let mut current = IoClass::Default;
    loop {
        let want = IoClass::for_core(rx.borrow_and_update().core_running());
        if want != current {
            let (setter, dir) = (Arc::clone(&setter), task_dir.clone());
            let applied =
                tokio::task::spawn_blocking(move || apply(setter.as_ref(), &dir, want)).await;
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
    use std::sync::Mutex;

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
        let task = tokio::spawn(follow(
            Arc::clone(&gate),
            Arc::clone(&fake) as Arc<dyn SetClass>,
            dir.path().to_path_buf(),
        ));
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
}
