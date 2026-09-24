//! Runs the built binary the way `scripts/mistarr.sh` does: output appended to the log.

use std::fs::OpenOptions;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn spawn(data: &Path, log: &Path) -> Child {
    let out = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .expect("log");
    let err = out.try_clone().expect("clone");
    Command::new(env!("CARGO_BIN_EXE_mistarr"))
        .args(["--listen", "127.0.0.1:0", "--data"])
        .arg(data)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .spawn()
        .expect("spawn")
}

fn wait_for_line(log: &Path, needle: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if std::fs::read_to_string(log).is_ok_and(|t| t.contains(needle)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("{needle:?} never logged");
}

#[test]
fn each_event_is_logged_once_and_a_second_start_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).expect("mkdir");
    let config = format!(
        "[paths]\nroot = {root:?}\ngames = {games:?}\n[client]\nkind = \"rtorrent\"\nurl = \"127.0.0.1:1\"\n",
        root = dir.path(),
        games = dir.path().join("games"),
    );
    std::fs::write(data.join("mistarr.toml"), config).expect("config");
    let log = data.join("mistarr.log");

    let mut first = spawn(&data, &log);
    wait_for_line(&log, "mistarr started");
    let second = spawn(&data, &log).wait().expect("second exits");
    assert!(
        !second.success(),
        "a second server on the same data directory"
    );
    wait_for_line(&log, "already running");

    let pid = first.id().to_string();
    let killed = Command::new("kill").arg(&pid).status().expect("kill");
    assert!(killed.success());
    let status = first.wait().expect("first exits");
    assert!(status.success(), "clean shutdown on SIGTERM: {status}");

    let text = std::fs::read_to_string(&log).expect("log");
    for line in ["listening", "mistarr started", "shutting down"] {
        let n = text.lines().filter(|l| l.contains(line)).count();
        assert_eq!(n, 1, "{line:?} logged {n} times in\n{text}");
    }
}
