use std::os::unix::fs::symlink;
use std::time::Duration;

use super::*;

/// A fake `/proc` entry for `pid` running `name`, holding the listed socket inodes.
fn fake_process(proc: &Path, pid: u32, name: &str, sockets: &[u64]) {
    let dir = proc.join(pid.to_string());
    std::fs::create_dir_all(dir.join("fd")).expect("mkdir");
    std::fs::write(dir.join("cmdline"), format!("/usr/bin/{name}\0-f\0")).expect("cmdline");
    let stat = format!("{pid} ({name} x) S 1 1 1 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 {pid}00 0 0");
    std::fs::write(dir.join("stat"), stat).expect("stat");
    for (i, inode) in sockets.iter().enumerate() {
        symlink(
            format!("socket:[{inode}]"),
            dir.join("fd").join(i.to_string()),
        )
        .expect("fd");
    }
}

fn fake_listener(proc: &Path, port: u16, inode: u64) {
    std::fs::create_dir_all(proc.join("net")).expect("mkdir");
    let head = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n";
    let row = format!("   0: 0100007F:{port:04X} 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 {inode} 1\n");
    std::fs::write(proc.join("net").join("tcp"), format!("{head}{row}")).expect("tcp");
}

#[test]
fn stat_reads_state_and_start_past_a_name_with_spaces() {
    let proc = tempfile::tempdir().expect("tempdir");
    fake_process(proc.path(), 42, "odd ) name", &[]);
    assert_eq!(stat(proc.path(), 42).expect("stat"), ('S', 4200));
    assert!(stat(proc.path(), 43).is_err());
    assert_eq!(process_name(proc.path(), 42).as_deref(), Some("odd ) name"));
    assert!(check_name(proc.path(), 42, "odd ) name").is_ok());
    assert!(matches!(
        check_name(proc.path(), 42, "other"),
        Err(FreezeError::NotFound(_))
    ));
}

#[test]
fn find_takes_the_one_process_or_the_one_on_the_port() {
    let proc = tempfile::tempdir().expect("tempdir");
    let p = proc.path();
    assert!(matches!(
        find(p, "transmission-daemon", None),
        Err(FreezeError::NotFound(_))
    ));
    fake_process(p, 10, "transmission-daemon", &[501]);
    fake_process(p, 11, "sleep", &[]);
    assert_eq!(find(p, "transmission-daemon", Some(9091)).expect("one"), 10);
    fake_process(p, 12, "transmission-daemon", &[777]);
    assert!(matches!(
        find(p, "transmission-daemon", Some(9091)),
        Err(FreezeError::Ambiguous { count: 2, .. })
    ));
    fake_listener(p, 9091, 777);
    assert_eq!(
        find(p, "transmission-daemon", Some(9091)).expect("port"),
        12
    );
    assert!(matches!(
        find(p, "transmission-daemon", None),
        Err(FreezeError::Ambiguous { .. })
    ));
}

#[test]
fn the_frozen_file_round_trips_and_is_optional() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("frozen");
    assert_eq!(read_file(&file).expect("read"), None);
    std::fs::write(
        &file,
        Frozen {
            pid: 3,
            starttime: 4,
        }
        .to_line(),
    )
    .expect("write");
    assert_eq!(
        read_file(&file).expect("read"),
        Some(Frozen {
            pid: 3,
            starttime: 4
        })
    );
    remove_file(&file).expect("remove");
    remove_file(&file).expect("again");
    std::fs::write(&file, "junk").expect("write");
    assert_eq!(read_file(&file).expect("read"), None);
}

fn state_of(pid: u32) -> char {
    stat(Path::new("/proc"), pid).expect("stat").0
}

fn wait_state(pid: u32, want: impl Fn(char) -> bool) -> char {
    for _ in 0..200 {
        let s = state_of(pid);
        if want(s) {
            return s;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    state_of(pid)
}

#[test]
fn a_real_process_is_stopped_and_resumed_only_while_it_is_the_same() {
    let proc = Path::new("/proc");
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("frozen");
    let mut child = Command::new("sleep").arg("30").spawn().expect("sleep");
    let pid = child.id();
    let kill = Kill::new(Path::new("kill"));

    let frozen = freeze(proc, &kill, &file, pid).expect("freeze");
    assert_eq!(read_file(&file).expect("read"), Some(frozen));
    assert_eq!(wait_state(pid, |s| s == 'T'), 'T');
    assert!(is_stopped(proc, frozen).expect("same"));

    let wrong = Frozen {
        pid,
        starttime: frozen.starttime + 1,
    };
    assert!(matches!(thaw(proc, &kill, wrong), Err(FreezeError::Reused(p)) if p == pid));
    assert!(matches!(
        is_stopped(proc, wrong),
        Err(FreezeError::Reused(_))
    ));
    assert_eq!(state_of(pid), 'T', "a mismatched start time sends nothing");

    thaw(proc, &kill, frozen).expect("thaw");
    assert_ne!(wait_state(pid, |s| s != 'T'), 'T');
    assert!(!is_stopped(proc, frozen).expect("same"));

    let _ = child.kill();
    let _ = child.wait();
    assert!(matches!(
        thaw(proc, &kill, frozen),
        Err(FreezeError::Gone(_))
    ));
}

#[test]
fn a_failed_stop_leaves_no_record() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("frozen");
    let kill = Kill::new(&dir.path().join("no-such-kill"));
    let err = freeze(Path::new("/proc"), &kill, &file, std::process::id()).expect_err("no kill");
    assert!(matches!(err, FreezeError::Kill(_)), "{err:?}");
    assert!(!file.exists());
    assert!(matches!(
        freeze(Path::new("/proc"), &kill, &file, u32::MAX),
        Err(FreezeError::Gone(_))
    ));
}
