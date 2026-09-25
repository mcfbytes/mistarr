//! A stand-in client process for tests: this test binary under a client's name.

use std::path::Path;
use std::process::{Child, Command};
use std::time::Duration;

const FAKE_ENV: &str = "MISTARR_FAKE_CLIENT";

/// Stands in for a client process when the test binary runs as one.
#[test]
#[ignore = "run only as the fake client process"]
fn fake_client_process() {
    if std::env::var_os(FAKE_ENV).is_some() {
        std::thread::sleep(Duration::from_secs(60));
    }
}

/// This test binary linked under `name`, so its `exe` link names it, running
/// [`fake_client_process`]; killed on drop.
pub(crate) struct FakeClient {
    child: Child,
    _dir: tempfile::TempDir,
}

impl FakeClient {
    pub(crate) fn spawn(name: &str) -> Self {
        let me = std::env::current_exe().expect("exe");
        let dir = tempfile::tempdir_in(me.parent().expect("dir")).expect("tempdir");
        let exe = dir.path().join(name);
        if std::fs::hard_link(&me, &exe).is_err() {
            std::fs::copy(&me, &exe).expect("copy");
        }
        let mut tries = 0;
        let child = loop {
            let spawned = Command::new(&exe)
                .args(["--ignored", "--exact", "freeze::fake::fake_client_process"])
                .env(FAKE_ENV, "1")
                .stdout(std::process::Stdio::null())
                .spawn();
            match spawned {
                Ok(child) => break child,
                // A binary just written may still be busy for exec for a moment.
                Err(e) if e.raw_os_error() == Some(26) && tries < 50 => {
                    tries += 1;
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => panic!("spawn: {e}"),
            }
        };
        // A vfork parent may resume a moment before the child's `exe` names the new program.
        let link = format!("/proc/{}/exe", child.id());
        for _ in 0..200 {
            if std::fs::read_link(&link).is_ok_and(|p| p.ends_with(name)) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Self { child, _dir: dir }
    }

    pub(crate) fn rtorrent() -> Self {
        Self::spawn("rtorrent")
    }

    pub(crate) fn pid(&self) -> u32 {
        self.child.id()
    }

    pub(crate) fn state(&self) -> char {
        super::stat(Path::new("/proc"), self.pid()).expect("stat").0
    }

    /// Waits up to two seconds for the process state to satisfy `want`.
    pub(crate) fn wait_state(&self, want: impl Fn(char) -> bool) -> char {
        for _ in 0..200 {
            let s = self.state();
            if want(s) {
                return s;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        self.state()
    }
}

impl Drop for FakeClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
