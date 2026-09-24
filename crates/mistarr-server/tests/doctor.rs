//! Runs the built binary's `doctor` subcommand.

use std::process::Command;

#[test]
fn doctor_prints_every_check() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data = dir.path().join("data");
    std::fs::create_dir_all(data.join("staging")).expect("mkdir");
    std::fs::create_dir_all(dir.path().join("_Console")).expect("mkdir");
    std::fs::write(dir.path().join("_Console/SNES_20240101.rbf"), b"").expect("write");
    let config = format!(
        "[paths]\nroot = {root:?}\ngames = {games:?}\n[client]\nkind = \"rtorrent\"\nurl = \"127.0.0.1:1\"\n",
        root = dir.path(),
        games = dir.path().join("games"),
    );
    std::fs::write(data.join("mistarr.toml"), config).expect("write");

    let out = Command::new(env!("CARGO_BIN_EXE_mistarr"))
        .args(["doctor", "--hash-mib", "1", "--data"])
        .arg(&data)
        .output()
        .expect("run");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).expect("utf8");
    let data_line = format!("path data {}: writable", data.display());
    for want in [
        "mistarr ",
        "binary: ",
        data_line.as_str(),
        "path games ",
        "path staging ",
        "free space data: ",
        "free space games: ",
        "client: rtorrent version unknown at 127.0.0.1:1, not reachable",
        "rtorrent on PATH: ",
        "cores: 1 installed (SNES)",
        "corename: ",
        "memory available: ",
        "hash: 1 MiB of zeros in ",
    ] {
        assert!(
            text.lines().any(|l| l.starts_with(want)),
            "missing {want:?} in\n{text}"
        );
    }
    assert!(text.contains("path games") && text.contains("not writable"));
}

#[test]
fn missing_explicit_config_is_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = Command::new(env!("CARGO_BIN_EXE_mistarr"))
        .args(["doctor", "--config"])
        .arg(dir.path().join("absent.toml"))
        .output()
        .expect("run");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("absent.toml"));
}
