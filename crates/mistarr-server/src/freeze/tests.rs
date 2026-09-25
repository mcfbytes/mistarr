use std::os::unix::fs::{symlink, PermissionsExt as _};

use super::fake::FakeClient;
use super::*;

/// A fake `/proc` entry for `pid` running `name`, holding the listed socket inodes.
fn fake_process(proc: &Path, pid: u32, name: &str, sockets: &[u64]) {
    let dir = proc.join(pid.to_string());
    std::fs::create_dir_all(dir.join("fd")).expect("mkdir");
    std::fs::write(dir.join("cmdline"), format!("/usr/bin/{name}\0-f\0")).expect("cmdline");
    symlink(format!("/usr/bin/{name}"), dir.join("exe")).expect("exe");
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

/// A private directory for the record, as mistarr's temporary directory is.
fn private() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let rec = dir.path().join("tmp");
    crate::db::private_dir(&rec).expect("private");
    let file = rec.join(FROZEN_NAME);
    (dir, file)
}

const ONE: Frozen = Frozen {
    pid: 3,
    starttime: 4,
};

#[test]
fn stat_reads_state_and_start_past_a_name_with_spaces() {
    let proc = tempfile::tempdir().expect("tempdir");
    fake_process(proc.path(), 42, "odd ) name", &[]);
    assert_eq!(stat(proc.path(), 42).expect("stat"), ('S', 4200));
    assert!(stat(proc.path(), 43).is_err());
    assert_eq!(process_name(proc.path(), 42).as_deref(), Some("odd ) name"));
}

#[test]
fn only_a_client_executable_passes_the_check() {
    let proc = tempfile::tempdir().expect("tempdir");
    let p = proc.path();
    fake_process(p, 10, "rtorrent", &[]);
    fake_process(p, 11, "transmission-daemon", &[]);
    fake_process(p, 12, "sh", &[]);
    assert!(check_client(p, 10).is_ok());
    assert!(check_client(p, 11).is_ok());
    assert!(matches!(
        check_client(p, 12),
        Err(FreezeError::NotClient(12))
    ));
    assert!(matches!(
        check_client(p, 13),
        Err(FreezeError::NotClient(13))
    ));
    let dir = p.join("14");
    std::fs::create_dir_all(&dir).expect("mkdir");
    symlink("/opt/rtorrent (deleted)", dir.join("exe")).expect("exe");
    assert!(
        check_client(p, 14).is_ok(),
        "an upgraded binary is still the client"
    );
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
fn the_record_round_trips_and_is_optional() {
    let (_dir, file) = private();
    let uid = euid();
    assert_eq!(read_file(&file, uid).expect("read"), None);
    write_file(&file, ONE).expect("write");
    assert_eq!(read_file(&file, uid).expect("read"), Some(ONE));
    let mode = std::fs::metadata(&file).expect("meta").permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    write_file(&file, Frozen { pid: 5, ..ONE }).expect("overwrite");
    assert_eq!(read_file(&file, uid).expect("read").map(|f| f.pid), Some(5));
    remove_file(&file).expect("remove");
    remove_file(&file).expect("again");
    std::fs::write(&file, "junk").expect("write");
    assert_eq!(read_file(&file, uid).expect("read"), None);
}

#[test]
fn a_planted_link_is_never_followed() {
    let (dir, file) = private();
    let target = dir.path().join("victim");
    std::fs::write(&target, "keep").expect("victim");
    symlink(&target, &file).expect("link");
    assert!(matches!(
        read_file(&file, euid()),
        Err(FreezeError::Untrusted(_))
    ));
    let mut new = file.as_os_str().to_owned();
    new.push(".new");
    symlink(&target, PathBuf::from(new)).expect("link");
    write_file(&file, ONE).expect("write");
    assert_eq!(std::fs::read_to_string(&target).expect("victim"), "keep");
    assert!(!std::fs::symlink_metadata(&file)
        .expect("meta")
        .file_type()
        .is_symlink());
    assert_eq!(read_file(&file, euid()).expect("read"), Some(ONE));
}

#[test]
fn a_record_of_another_owner_or_kind_or_place_is_refused() {
    let (dir, file) = private();
    write_file(&file, ONE).expect("write");
    assert!(matches!(
        read_file(&file, euid().wrapping_add(1)),
        Err(FreezeError::Untrusted(_))
    ));
    remove_file(&file).expect("remove");
    std::fs::create_dir(&file).expect("dir");
    assert!(matches!(
        read_file(&file, euid()),
        Err(FreezeError::Untrusted(_))
    ));
    let open = dir.path().join("open");
    std::fs::create_dir(&open).expect("dir");
    std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o777)).expect("chmod");
    std::fs::write(open.join(FROZEN_NAME), ONE.to_line()).expect("write");
    assert!(matches!(
        read_file(&open.join(FROZEN_NAME), euid()),
        Err(FreezeError::Untrusted(_))
    ));
    let linked = dir.path().join("linked");
    symlink(&open, &linked).expect("link");
    assert!(write_file(&linked.join(FROZEN_NAME), ONE).is_err());
}

#[test]
fn a_real_client_is_stopped_and_resumed_only_while_it_is_the_same() {
    let proc = Path::new("/proc");
    let (_dir, file) = private();
    let child = FakeClient::rtorrent();
    let pid = child.pid();
    let kill = Kill::new(Path::new("kill"));

    let frozen = freeze(proc, &kill, &file, pid).expect("freeze");
    assert_eq!(read_file(&file, euid()).expect("read"), Some(frozen));
    assert_eq!(child.wait_state(|s| s == 'T'), 'T');
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
    assert_eq!(child.state(), 'T', "a mismatched start time sends nothing");

    thaw(proc, &kill, frozen).expect("thaw");
    assert_ne!(child.wait_state(|s| s != 'T'), 'T');
    assert!(!is_stopped(proc, frozen).expect("same"));
    stop_again(proc, &kill, frozen).expect("stop again");
    assert_eq!(child.wait_state(|s| s == 'T'), 'T');
    thaw(proc, &kill, frozen).expect("thaw");

    drop(child);
    assert!(matches!(
        thaw(proc, &kill, frozen),
        Err(FreezeError::Gone(_))
    ));
}

#[test]
fn a_process_of_another_name_is_never_signalled() {
    let proc = Path::new("/proc");
    let (_dir, file) = private();
    let other = FakeClient::spawn("helper");
    let kill = Kill::new(Path::new("kill"));
    assert!(matches!(
        freeze(proc, &kill, &file, other.pid()),
        Err(FreezeError::NotClient(_))
    ));
    assert!(!file.exists(), "nothing is recorded for it");
    let (_, starttime) = stat(proc, other.pid()).expect("stat");
    let planted = Frozen {
        pid: other.pid(),
        starttime,
    };
    assert!(matches!(
        thaw(proc, &kill, planted),
        Err(FreezeError::NotClient(_))
    ));
    assert!(matches!(
        stop_again(proc, &kill, planted),
        Err(FreezeError::NotClient(_))
    ));
    assert_ne!(other.state(), 'T');
}

#[test]
fn a_failed_stop_leaves_no_record() {
    let (dir, file) = private();
    let child = FakeClient::rtorrent();
    let kill = Kill::new(&dir.path().join("no-such-kill"));
    let err = freeze(Path::new("/proc"), &kill, &file, child.pid()).expect_err("no kill");
    assert!(matches!(err, FreezeError::Kill(_)), "{err:?}");
    assert!(!file.exists());
    assert!(matches!(
        freeze(Path::new("/proc"), &kill, &file, u32::MAX),
        Err(FreezeError::Gone(_))
    ));
}
