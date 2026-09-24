//! Starting a client that is installed but not running; see
//! `docs/DOWNLOAD-CLIENTS.md` "Starting a stopped client".

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::{ClientError, ClientKind, Result};

/// The directory whose presence makes the `Buildroot_MiSTer` init script start Transmission.
pub const TRANSMISSION_OPT_IN: &str = "/media/fat/linux/transmission";

/// The `Buildroot_MiSTer` init script for `transmission-daemon`.
pub const TRANSMISSION_INIT: &str = "/etc/init.d/S92transmission";

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

    /// Starts `kind` and returns once the start command has returned; the
    /// client may need a few seconds more before it answers. Blocks.
    ///
    /// Transmission: with the init script, creates the opt-in directory and
    /// runs `<init> start`; without it, runs `transmission-daemon` with its
    /// config under `<data>/transmission` and `<data>/staging` as download
    /// directory. rtorrent: writes `<data>/rtorrent.rc` unless it exists,
    /// creates `<data>/rtorrent-session` and starts `rtorrent` detached
    /// under `nice` where the board has it.
    ///
    /// # Errors
    ///
    /// [`ClientError::Launch`] when the client is not installed or its start
    /// command fails, [`ClientError::Io`] when a file cannot be written.
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
                let rc = self.data_dir.join("rtorrent.rc");
                if !rc.exists() {
                    std::fs::write(&rc, rtorrent_rc(&self.data_dir))?;
                }
                std::fs::create_dir_all(self.data_dir.join("rtorrent-session"))?;
                let script = "p=; command -v nice >/dev/null 2>&1 && p='nice -n 10'; \
                              $p rtorrent -n -o import=\"$1\" </dev/null >/dev/null 2>&1 &";
                self.run(Command::new("/bin/sh").args(["-c", script, "sh"]).arg(&rc))
            }
            _ => Err(ClientError::Launch(format!("{kind} is not installed"))),
        }
    }

    /// Runs `cmd` with this launcher's `PATH` and waits for it.
    fn run(&self, cmd: &mut Command) -> Result<()> {
        if let Some(path) = &self.search_path {
            cmd.env("PATH", path);
        }
        let out = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| ClientError::Launch(e.to_string()))?;
        if out.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        let tail = stderr.lines().last().unwrap_or("").trim();
        Err(ClientError::Launch(format!(
            "start command {}: {tail}",
            out.status
        )))
    }
}

/// The rc for an rtorrent mistarr starts, with every path under `data_dir`.
///
/// ```
/// let rc = mistarr_clients::launch::rtorrent_rc("/srv/m".as_ref());
/// assert!(rc.contains("network.scgi.open_local = /srv/m/rtorrent.sock"));
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
    use std::time::Duration;

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
            data_dir: dir.join("data"),
            search_path: Some(bin.into_os_string()),
        }
    }

    fn wait_for(path: &Path) -> String {
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
        let data = dir.path().join("data");
        assert!(args.contains(&format!("--config-dir {}/transmission", data.display())));
        assert!(args.contains(&format!("--download-dir {}/staging", data.display())));
    }

    #[test]
    fn rtorrent_gets_a_generated_rc_and_runs_detached() {
        let dir = tempfile::tempdir().expect("tempdir");
        let l = launcher(dir.path());
        std::fs::create_dir_all(&l.data_dir).expect("mkdir");
        let marker = dir.path().join("args");
        script(
            &dir.path().join("bin"),
            "rtorrent",
            &format!("printf '%s ' \"$@\" > '{}'", marker.display()),
        );
        l.start(ClientKind::Rtorrent).expect("start");
        let args = wait_for(&marker);
        let rc = l.data_dir.join("rtorrent.rc");
        assert!(
            args.contains(&format!("-n -o import={}", rc.display())),
            "{args}"
        );
        let text = std::fs::read_to_string(&rc).expect("rc");
        assert!(text.contains(&format!(
            "session.path.set = {}/rtorrent-session",
            l.data_dir.display()
        )));
        assert!(l.data_dir.join("rtorrent-session").is_dir());
        std::fs::write(&rc, "user's own\n").expect("write");
        l.start(ClientKind::Rtorrent).expect("start");
        assert_eq!(std::fs::read_to_string(&rc).expect("rc"), "user's own\n");
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
