//! Starting a client that is installed but not running; see
//! `docs/DOWNLOAD-CLIENTS.md` "Starting a stopped client".

use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use crate::rtorrent::RC_MARKER;
use crate::{ClientError, ClientKind, Result};

/// The directory whose presence makes the `Buildroot_MiSTer` init script start Transmission.
pub const TRANSMISSION_OPT_IN: &str = "/media/fat/linux/transmission";

/// The `Buildroot_MiSTer` init script for `transmission-daemon`.
pub const TRANSMISSION_INIT: &str = "/etc/init.d/S92transmission";

/// How long a start command may run before it is killed and reported.
pub const START_TIMEOUT: Duration = Duration::from_secs(30);

/// The file under the data directory that start commands write their output to.
pub const START_LOG: &str = "client-start.log";

/// How long a launched rtorrent must stay up to count as started.
const RTORRENT_SETTLE: Duration = Duration::from_secs(2);

/// The data directory the rc from [`crate::rtorrent::recommended_rc`] names.
const RC_DATA_DIR: &str = "/media/fat/mistarr";

/// Which clients are installed on this machine, whether or not they run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // One flag per thing the UI offers.
pub struct Installed {
    /// `transmission-daemon` is an executable on `PATH`.
    pub transmission_on_path: bool,
    /// The `Buildroot_MiSTer` init script for it exists.
    pub transmission_service: bool,
    /// Its opt-in directory exists, so the init script starts it at boot.
    pub transmission_opt_in: bool,
    /// `rtorrent` is an executable on `PATH`.
    pub rtorrent_on_path: bool,
}

/// Where to look for installed clients and how to start them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launcher {
    /// [`TRANSMISSION_OPT_IN`] on the board.
    pub transmission_opt_in: PathBuf,
    /// [`TRANSMISSION_INIT`] on the board.
    pub transmission_init: PathBuf,
    /// mistarr's data directory, holding `staging/` and the rtorrent files.
    pub data_dir: PathBuf,
    /// The executable search path; `None` reads `PATH`.
    pub search_path: Option<OsString>,
    /// How long a start command may run; [`START_TIMEOUT`] on the board.
    pub timeout: Duration,
}

impl Launcher {
    /// The board's locations, with `data_dir` as mistarr's data directory.
    ///
    /// ```
    /// let l = mistarr_clients::launch::Launcher::board("/media/fat/mistarr".as_ref());
    /// assert!(l.transmission_init.ends_with("S92transmission"));
    /// ```
    #[must_use]
    pub fn board(data_dir: &Path) -> Self {
        Self {
            transmission_opt_in: PathBuf::from(TRANSMISSION_OPT_IN),
            transmission_init: PathBuf::from(TRANSMISSION_INIT),
            data_dir: data_dir.to_path_buf(),
            search_path: None,
            timeout: START_TIMEOUT,
        }
    }

    fn search_path(&self) -> Option<OsString> {
        self.search_path
            .clone()
            .or_else(|| std::env::var_os("PATH"))
    }

    /// What is installed.
    ///
    /// ```
    /// use mistarr_clients::launch::Launcher;
    /// let mut l = Launcher::board("/nonexistent".as_ref());
    /// l.search_path = Some("/nonexistent".into());
    /// assert!(!l.installed().rtorrent_on_path);
    /// ```
    #[must_use]
    pub fn installed(&self) -> Installed {
        let path = self.search_path();
        Installed {
            transmission_on_path: on_path("transmission-daemon", path.as_deref()),
            transmission_service: self.transmission_init.is_file(),
            transmission_opt_in: self.transmission_opt_in.is_dir(),
            rtorrent_on_path: on_path("rtorrent", path.as_deref()),
        }
    }

    /// Starts `kind` and returns once it is started; the client may need a
    /// few seconds more before it answers. Blocks for at most `timeout`.
    ///
    /// Transmission: with the init script, creates the opt-in directory and
    /// runs `<init> start`; without it, runs `transmission-daemon` with its
    /// config under `<data>/transmission`. rtorrent: rewrites the managed
    /// `<data>/rtorrent.rc` and runs `rtorrent` in daemon mode in its own
    /// process group. Output goes to `<data>/client-start.log`.
    ///
    /// # Errors
    ///
    /// [`ClientError::Launch`] when the client is not installed, its start
    /// command fails or overruns, or rtorrent exits at once;
    /// [`ClientError::Io`] when a file cannot be written.
    pub fn start(&self, kind: ClientKind) -> Result<()> {
        let installed = self.installed();
        match kind {
            ClientKind::Transmission if installed.transmission_service => {
                std::fs::create_dir_all(&self.transmission_opt_in)?;
                self.run(Command::new(&self.transmission_init).arg("start"))
            }
            ClientKind::Transmission if installed.transmission_on_path => {
                let config = self.data_dir.join("transmission");
                std::fs::create_dir_all(&config)?;
                let mut cmd = Command::new("transmission-daemon");
                cmd.arg("--config-dir")
                    .arg(&config)
                    .arg("--download-dir")
                    .arg(self.data_dir.join("staging"));
                self.run(&mut cmd)
            }
            ClientKind::Rtorrent if installed.rtorrent_on_path => {
                let rc = self.write_rc()?;
                std::fs::create_dir_all(self.data_dir.join("rtorrent-session"))?;
                let nice = on_path("nice", self.search_path().as_deref());
                let mut cmd = if nice {
                    let mut c = Command::new("nice");
                    c.args(["-n", "10", "rtorrent"]);
                    c
                } else {
                    Command::new("rtorrent")
                };
                cmd.args(["-n", "-o", "system.daemon.set=true", "-o"])
                    .arg(format!("import=\"{}\"", rc.display()));
                self.spawn_detached(&mut cmd)
            }
            _ => Err(ClientError::Launch(format!("{kind} is not installed"))),
        }
    }

    /// Writes the managed rc unless `<data>/rtorrent.rc` is the user's own,
    /// meaning it exists without [`RC_MARKER`]. Returns its path.
    fn write_rc(&self) -> Result<PathBuf> {
        let data = self.data_dir.to_string_lossy();
        if data.contains('"') {
            return Err(ClientError::Launch(
                "the data directory's path contains a double quote".into(),
            ));
        }
        std::fs::create_dir_all(&self.data_dir)?;
        let rc = self.data_dir.join("rtorrent.rc");
        let owned = std::fs::read_to_string(&rc).is_ok_and(|t| !t.contains(RC_MARKER));
        if !owned {
            std::fs::write(&rc, rtorrent_rc(&self.data_dir))?;
        }
        Ok(rc)
    }

    /// Opens the start log for appending and returns it with its current length.
    fn open_log(&self) -> Result<(File, u64)> {
        std::fs::create_dir_all(&self.data_dir)?;
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.data_dir.join(START_LOG))?;
        let from = log.metadata()?.len();
        Ok((log, from))
    }

    /// The last line this start wrote to the log, if any.
    fn log_tail(&self, from: u64) -> String {
        let mut text = String::new();
        if let Ok(mut f) = File::open(self.data_dir.join(START_LOG)) {
            if f.seek(SeekFrom::Start(from)).is_ok() {
                let _ = f.take(64 * 1024).read_to_string(&mut text);
            }
        }
        text.lines()
            .rev()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("")
            .to_owned()
    }

    /// Spawns `cmd` with this launcher's `PATH`, no stdin and output to the log.
    fn spawn(&self, cmd: &mut Command) -> Result<(Child, u64)> {
        if let Some(path) = &self.search_path {
            cmd.env("PATH", path);
        }
        let (log, from) = self.open_log()?;
        let child = cmd
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .spawn()
            .map_err(|e| ClientError::Launch(e.to_string()))?;
        Ok((child, from))
    }

    /// Runs `cmd` to completion within `timeout`; a non-zero exit is an error
    /// naming the last line it logged.
    fn run(&self, cmd: &mut Command) -> Result<()> {
        let (mut child, from) = self.spawn(cmd)?;
        match wait_for(&mut child, self.timeout)? {
            Some(status) if status.success() => Ok(()),
            Some(status) => Err(ClientError::Launch(format!(
                "start command {status}: {}",
                self.log_tail(from)
            ))),
            None => {
                let _ = child.kill();
                let _ = child.wait();
                Err(ClientError::Launch(format!(
                    "start command still running after {} s",
                    self.timeout.as_secs()
                )))
            }
        }
    }

    /// Starts `cmd` in its own process group and succeeds when it is still
    /// running after a short settle; a thread reaps it when it ends.
    fn spawn_detached(&self, cmd: &mut Command) -> Result<()> {
        cmd.process_group(0);
        let (mut child, from) = self.spawn(cmd)?;
        if let Some(status) = wait_for(&mut child, self.timeout.min(RTORRENT_SETTLE))? {
            return Err(ClientError::Launch(format!(
                "rtorrent exited ({status}): {}",
                self.log_tail(from)
            )));
        }
        std::thread::spawn(move || child.wait());
        Ok(())
    }
}

/// Waits up to `limit` for `child`; `None` when it is still running.
fn wait_for(child: &mut Child, limit: Duration) -> Result<Option<ExitStatus>> {
    let until = Instant::now() + limit;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= until {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The rc for an rtorrent mistarr starts, with every path under `data_dir`.
///
/// ```
/// let rc = mistarr_clients::launch::rtorrent_rc("/srv/my data".as_ref());
/// assert!(rc.contains("session.path.set = \"/srv/my data/rtorrent-session\""));
/// ```
#[must_use]
pub fn rtorrent_rc(data_dir: &Path) -> String {
    crate::rtorrent::recommended_rc().replace(RC_DATA_DIR, &data_dir.to_string_lossy())
}

/// True if an executable file called `name` is in one of the `path` directories.
///
/// ```
/// use mistarr_clients::launch::on_path;
/// assert!(!on_path("definitely-not-a-program", Some("/nonexistent".as_ref())));
/// ```
#[must_use]
pub fn on_path(name: &str, path: Option<&OsStr>) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.into_iter().flat_map(std::env::split_paths).any(|dir| {
        std::fs::metadata(dir.join(name))
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn script(dir: &Path, name: &str, body: &str) {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    fn launcher(dir: &Path) -> Launcher {
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).expect("mkdir");
        Launcher {
            transmission_opt_in: dir.join("linux/transmission"),
            transmission_init: dir.join("S92transmission"),
            data_dir: dir.join("my data"),
            search_path: Some(bin.into_os_string()),
            timeout: Duration::from_secs(5),
        }
    }

    fn wait_for_file(path: &Path) -> String {
        for _ in 0..200 {
            if let Ok(text) = std::fs::read_to_string(path) {
                if !text.is_empty() {
                    return text;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("{} never written", path.display());
    }

    #[test]
    fn nothing_installed_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let l = launcher(dir.path());
        assert_eq!(l.installed(), Installed::default());
        for kind in [ClientKind::Transmission, ClientKind::Rtorrent] {
            assert!(matches!(l.start(kind), Err(ClientError::Launch(_))));
        }
    }

    #[test]
    fn the_init_script_is_opted_in_and_run() {
        let dir = tempfile::tempdir().expect("tempdir");
        let l = launcher(dir.path());
        let marker = dir.path().join("ran");
        script(
            dir.path(),
            "S92transmission",
            &format!("echo \"$1\" > '{}'", marker.display()),
        );
        script(&dir.path().join("bin"), "transmission-daemon", "exit 0");
        let found = l.installed();
        assert!(found.transmission_on_path && found.transmission_service);
        assert!(!found.transmission_opt_in);
        l.start(ClientKind::Transmission).expect("start");
        assert_eq!(std::fs::read_to_string(&marker).expect("ran"), "start\n");
        assert!(l.installed().transmission_opt_in);
    }

    #[test]
    fn a_failing_start_reports_its_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let l = launcher(dir.path());
        script(
            dir.path(),
            "S92transmission",
            "echo 'no daemon' >&2; exit 3",
        );
        let err = l.start(ClientKind::Transmission).expect_err("fails");
        assert!(err.to_string().contains("no daemon"), "{err}");
    }

    #[test]
    fn a_daemon_holding_the_output_open_does_not_block_the_start() {
        let dir = tempfile::tempdir().expect("tempdir");
        let l = launcher(dir.path());
        script(dir.path(), "S92transmission", "/bin/sleep 5 & exit 0");
        let began = Instant::now();
        l.start(ClientKind::Transmission).expect("start");
        assert!(began.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_hanging_start_is_killed_at_the_timeout() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut l = launcher(dir.path());
        l.timeout = Duration::from_millis(200);
        script(dir.path(), "S92transmission", "exec /bin/sleep 30");
        let err = l.start(ClientKind::Transmission).expect_err("overruns");
        assert!(err.to_string().contains("still running"), "{err}");
    }

    #[test]
    fn a_bare_daemon_gets_mistarrs_directories() {
        let dir = tempfile::tempdir().expect("tempdir");
        let l = launcher(dir.path());
        let marker = dir.path().join("args");
        script(
            &dir.path().join("bin"),
            "transmission-daemon",
            &format!("echo \"$@\" > '{}'", marker.display()),
        );
        l.start(ClientKind::Transmission).expect("start");
        let args = std::fs::read_to_string(&marker).expect("ran");
        let data = &l.data_dir;
        assert!(args.contains(&format!("--config-dir {}/transmission", data.display())));
        assert!(args.contains(&format!("--download-dir {}/staging", data.display())));
    }

    #[test]
    fn rtorrent_gets_a_managed_rc_and_runs_as_a_daemon() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut l = launcher(dir.path());
        l.timeout = Duration::from_millis(300);
        let marker = dir.path().join("args");
        script(
            &dir.path().join("bin"),
            "rtorrent",
            &format!("printf '%s|' \"$@\" > '{}'; exec /bin/sleep 3", marker.display()),
        );
        let rc = l.data_dir.join("rtorrent.rc");
        std::fs::create_dir_all(&l.data_dir).expect("mkdir");
        std::fs::write(&rc, format!("{RC_MARKER}\nstale\n")).expect("write");
        l.start(ClientKind::Rtorrent).expect("start");
        let args = wait_for_file(&marker);
        let want = format!("-n|-o|system.daemon.set=true|-o|import=\"{}\"|", rc.display());
        assert_eq!(args, want);
        let text = std::fs::read_to_string(&rc).expect("rc");
        assert!(text.starts_with(RC_MARKER) && !text.contains("stale"));
        assert!(text.contains(&format!(
            "session.path.set = \"{}/rtorrent-session\"",
            l.data_dir.display()
        )));
        assert!(text.contains("network.scgi.open_port = 127.0.0.1:5000"));
        assert!(l.data_dir.join("rtorrent-session").is_dir());
    }

    #[test]
    fn a_users_own_rc_is_kept() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut l = launcher(dir.path());
        l.timeout = Duration::from_millis(300);
        script(&dir.path().join("bin"), "rtorrent", "exec /bin/sleep 3");
        std::fs::create_dir_all(&l.data_dir).expect("mkdir");
        let rc = l.data_dir.join("rtorrent.rc");
        std::fs::write(&rc, "user's own\n").expect("write");
        l.start(ClientKind::Rtorrent).expect("start");
        assert_eq!(std::fs::read_to_string(&rc).expect("rc"), "user's own\n");
    }

    #[test]
    fn an_rtorrent_that_exits_at_once_is_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let l = launcher(dir.path());
        script(
            &dir.path().join("bin"),
            "rtorrent",
            "echo 'unknown command' >&2; exit 1",
        );
        let err = l.start(ClientKind::Rtorrent).expect_err("exits");
        assert!(err.to_string().contains("unknown command"), "{err}");
    }

    #[test]
    fn a_quote_in_the_data_dir_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut l = launcher(dir.path());
        l.data_dir = dir.path().join("a\"b");
        script(&dir.path().join("bin"), "rtorrent", "exec /bin/sleep 3");
        assert!(matches!(
            l.start(ClientKind::Rtorrent),
            Err(ClientError::Launch(_))
        ));
    }

    #[test]
    fn path_search_finds_executables_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let exe = dir.path().join("tool");
        std::fs::write(&exe, b"").expect("write");
        let path = dir.path().as_os_str();
        assert!(!on_path("tool", Some(path)));
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        assert!(on_path("tool", Some(path)));
        assert!(!on_path("tool", None));
    }
}
