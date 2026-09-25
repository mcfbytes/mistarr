//! Freezing a download client on the board with SIGSTOP; see `docs/DOWNLOAD-CLIENTS.md` "Core gate".

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Where the frozen client's pid and start time are kept, in RAM so no card
/// write is spent and `mistarr.sh` can resume it if mistarr is killed.
pub const FROZEN_FILE: &str = "/tmp/mistarr-client.frozen";

/// Why a client cannot be frozen or resumed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FreezeError {
    /// No process of that name runs, or the reported one is another program.
    #[error("no {0} process found")]
    NotFound(String),
    /// More than one process fits and none alone holds the client's port.
    #[error("{count} {name} processes and none alone listens on the client's port")]
    Ambiguous {
        /// The executable name.
        name: String,
        /// How many run.
        count: usize,
    },
    /// The recorded process has exited.
    #[error("process {0} is gone")]
    Gone(u32),
    /// The recorded pid now belongs to a process started later.
    #[error("process {0} was replaced by another with the same id")]
    Reused(u32),
    /// `kill` failed or could not be run.
    #[error("kill: {0}")]
    Kill(String),
    /// Reading `/proc` or the frozen file failed.
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// A process mistarr stopped: its pid and its start time in clock ticks since
/// boot, which tells a reused pid apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frozen {
    /// The process id.
    pub pid: u32,
    /// Field 22 of `/proc/<pid>/stat`.
    pub starttime: u64,
}

impl Frozen {
    /// The frozen file's text: `pid starttime`.
    ///
    /// ```
    /// use mistarr_server::freeze::Frozen;
    /// let f = Frozen { pid: 7, starttime: 99 };
    /// assert_eq!(Frozen::parse(&f.to_line()), Some(f));
    /// assert_eq!(Frozen::parse("7"), None);
    /// ```
    #[must_use]
    pub fn to_line(self) -> String {
        format!("{} {}\n", self.pid, self.starttime)
    }

    /// Parses [`Frozen::to_line`]'s text.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split_whitespace();
        let pid = parts.next()?.parse().ok()?;
        let starttime = parts.next()?.parse().ok()?;
        parts.next().is_none().then_some(Self { pid, starttime })
    }
}

/// A process's state letter and start time from `/proc/<pid>/stat`.
///
/// # Errors
///
/// [`io::Error`] when the process does not exist or its line is malformed.
///
/// ```
/// let (state, start) = mistarr_server::freeze::stat(std::path::Path::new("/proc"), std::process::id()).unwrap();
/// assert!(state == 'R' || state == 'S');
/// assert!(start > 0);
/// ```
pub fn stat(proc: &Path, pid: u32) -> io::Result<(char, u64)> {
    let text = std::fs::read_to_string(proc.join(pid.to_string()).join("stat"))?;
    // The name in parentheses may hold spaces; the fields after its last `)` do not.
    let rest = text
        .rsplit_once(')')
        .map(|(_, r)| r)
        .ok_or_else(|| io::Error::other("stat without a name"))?;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let state = fields.first().and_then(|s| s.chars().next());
    let start = fields.get(19).and_then(|s| s.parse().ok());
    match (state, start) {
        (Some(state), Some(start)) => Ok((state, start)),
        _ => Err(io::Error::other("short stat line")),
    }
}

/// The executable name of `pid`: the file its `exe` link names, else the
/// first word of its command line.
///
/// ```
/// let name = mistarr_server::freeze::process_name(std::path::Path::new("/proc"), std::process::id());
/// assert!(name.is_some());
/// ```
#[must_use]
pub fn process_name(proc: &Path, pid: u32) -> Option<String> {
    let dir = proc.join(pid.to_string());
    if let Ok(exe) = std::fs::read_link(dir.join("exe")) {
        return exe.file_name().map(|n| n.to_string_lossy().into_owned());
    }
    let cmdline = std::fs::read(dir.join("cmdline")).ok()?;
    let first = cmdline.split(|b| *b == 0).next()?;
    let path = PathBuf::from(String::from_utf8_lossy(first).into_owned());
    path.file_name().map(|n| n.to_string_lossy().into_owned())
}

/// Checks that `pid` runs the executable `name`.
///
/// # Errors
///
/// [`FreezeError::NotFound`] when it does not.
pub fn check_name(proc: &Path, pid: u32, name: &str) -> Result<(), FreezeError> {
    if process_name(proc, pid).as_deref() == Some(name) {
        Ok(())
    } else {
        Err(FreezeError::NotFound(name.to_owned()))
    }
}

/// The one process running `name`, or of several the one listening on TCP `port`.
///
/// # Errors
///
/// [`FreezeError::NotFound`] when none runs, [`FreezeError::Ambiguous`] when
/// several run and the port does not single one out.
pub fn find(proc: &Path, name: &str, port: Option<u16>) -> Result<u32, FreezeError> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(proc)? {
        let Ok(entry) = entry else { continue };
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        if process_name(proc, pid).as_deref() == Some(name) {
            found.push(pid);
        }
    }
    match found.as_slice() {
        [] => return Err(FreezeError::NotFound(name.to_owned())),
        [one] => return Ok(*one),
        _ => {}
    }
    let inodes = port.map(|p| listening_inodes(proc, p)).unwrap_or_default();
    let holders: Vec<u32> = found
        .iter()
        .copied()
        .filter(|pid| !inodes.is_empty() && holds_socket(proc, *pid, &inodes))
        .collect();
    match holders.as_slice() {
        [one] => Ok(*one),
        _ => Err(FreezeError::Ambiguous {
            name: name.to_owned(),
            count: found.len(),
        }),
    }
}

/// Socket inodes listening on TCP `port`, from `/proc/net/tcp` and `tcp6`.
fn listening_inodes(proc: &Path, port: u16) -> HashSet<u64> {
    let mut out = HashSet::new();
    for table in ["tcp", "tcp6"] {
        let Ok(text) = std::fs::read_to_string(proc.join("net").join(table)) else {
            continue;
        };
        for line in text.lines().skip(1) {
            let f: Vec<&str> = line.split_whitespace().collect();
            let local_port = f
                .get(1)
                .and_then(|a| a.rsplit_once(':'))
                .and_then(|(_, p)| u16::from_str_radix(p, 16).ok());
            // State 0A is LISTEN.
            if local_port == Some(port) && f.get(3) == Some(&"0A") {
                out.extend(f.get(9).and_then(|i| i.parse::<u64>().ok()));
            }
        }
    }
    out
}

fn holds_socket(proc: &Path, pid: u32, inodes: &HashSet<u64>) -> bool {
    let Ok(fds) = std::fs::read_dir(proc.join(pid.to_string()).join("fd")) else {
        return false;
    };
    fds.flatten().any(|fd| {
        std::fs::read_link(fd.path()).is_ok_and(|target| {
            target
                .to_str()
                .and_then(|t| t.strip_prefix("socket:["))
                .and_then(|t| t.strip_suffix(']'))
                .and_then(|i| i.parse::<u64>().ok())
                .is_some_and(|i| inodes.contains(&i))
        })
    })
}

/// A signal mistarr sends to the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// SIGSTOP.
    Stop,
    /// SIGCONT.
    Cont,
}

impl Signal {
    const fn arg(self) -> &'static str {
        match self {
            Self::Stop => "-STOP",
            Self::Cont => "-CONT",
        }
    }
}

/// Sends signals through the `kill` program, which `BusyBox` provides.
#[derive(Debug, Clone)]
pub struct Kill {
    program: PathBuf,
}

impl Kill {
    /// Uses `program`, looked up on `PATH` when it has no directory part.
    ///
    /// ```
    /// let _ = mistarr_server::freeze::Kill::new(std::path::Path::new("kill"));
    /// ```
    #[must_use]
    pub fn new(program: &Path) -> Self {
        Self {
            program: program.to_path_buf(),
        }
    }

    /// Sends `signal` to `pid`.
    ///
    /// # Errors
    ///
    /// [`FreezeError::Kill`] when `kill` cannot run or exits with an error.
    pub fn send(&self, pid: u32, signal: Signal) -> Result<(), FreezeError> {
        let out = Command::new(&self.program)
            .args([signal.arg(), &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| FreezeError::Kill(e.to_string()))?;
        if out.status.success() {
            Ok(())
        } else {
            let text = String::from_utf8_lossy(&out.stderr).trim().to_owned();
            Err(FreezeError::Kill(if text.is_empty() {
                out.status.to_string()
            } else {
                text
            }))
        }
    }
}

/// Records `pid` in `file`, then stops it. The record comes first, so a crash
/// in between still leaves what to resume.
///
/// # Errors
///
/// [`FreezeError`] when the process cannot be read, the file written or the signal sent;
/// the file is then removed again.
pub fn freeze(proc: &Path, kill: &Kill, file: &Path, pid: u32) -> Result<Frozen, FreezeError> {
    let (_, starttime) = stat(proc, pid).map_err(|_| FreezeError::Gone(pid))?;
    let frozen = Frozen { pid, starttime };
    std::fs::write(file, frozen.to_line())?;
    if let Err(e) = kill.send(pid, Signal::Stop) {
        let _ = std::fs::remove_file(file);
        return Err(e);
    }
    Ok(frozen)
}

/// Resumes `frozen` when its pid still names the process that was stopped.
///
/// # Errors
///
/// [`FreezeError::Gone`] or [`FreezeError::Reused`] without sending anything,
/// [`FreezeError::Kill`] when the signal fails.
pub fn thaw(proc: &Path, kill: &Kill, frozen: Frozen) -> Result<(), FreezeError> {
    same_process(proc, frozen)?;
    kill.send(frozen.pid, Signal::Cont)
}

/// Whether `frozen` is still stopped: state `T`, or `t` under a tracer.
///
/// # Errors
///
/// [`FreezeError::Gone`] or [`FreezeError::Reused`] when it is not the same process.
pub fn is_stopped(proc: &Path, frozen: Frozen) -> Result<bool, FreezeError> {
    let state = same_process(proc, frozen)?;
    Ok(matches!(state, 'T' | 't'))
}

fn same_process(proc: &Path, frozen: Frozen) -> Result<char, FreezeError> {
    let (state, starttime) = stat(proc, frozen.pid).map_err(|_| FreezeError::Gone(frozen.pid))?;
    if starttime == frozen.starttime {
        Ok(state)
    } else {
        Err(FreezeError::Reused(frozen.pid))
    }
}

/// Reads the frozen file; `None` when there is none.
///
/// # Errors
///
/// [`io::Error`] when it exists but cannot be read; a malformed one reads as `None`.
pub fn read_file(file: &Path) -> io::Result<Option<Frozen>> {
    match std::fs::read_to_string(file) {
        Ok(text) => Ok(Frozen::parse(&text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Removes the frozen file; an absent one is fine.
///
/// # Errors
///
/// [`io::Error`] when it exists and cannot be removed.
pub fn remove_file(file: &Path) -> io::Result<()> {
    match std::fs::remove_file(file) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests;
