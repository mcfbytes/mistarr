use std::os::fd::AsRawFd as _;

use proptest::prelude::*;

use super::*;

#[test]
fn a_passwd_entry_gives_its_uid_and_gid() {
    let passwd = "root:x:0:0:root:/root:/bin/sh\n\
                  broken:x:abc:1::/:/bin/false\n\
                  mistarr:x:8420:8421:mistarr:/nonexistent:/bin/false";
    assert_eq!(
        lookup(passwd, "mistarr"),
        Some(Account {
            uid: 8420,
            gid: 8421
        })
    );
    assert_eq!(lookup(passwd, "broken"), None);
    assert_eq!(lookup(passwd, "mist"), None);
    assert_eq!(lookup("", "mistarr"), None);
}

#[test]
fn the_account_spec_resolves_names_ids_and_root() {
    let passwd = "media:x:1001:1002::/:/bin/false\n";
    let account = |uid, gid| RunAs::Account(Account { uid, gid });
    assert_eq!(run_as("media", passwd).unwrap(), account(1001, 1002));
    assert_eq!(
        run_as(" mistarr ", passwd).unwrap(),
        account(DEFAULT_ID, DEFAULT_ID)
    );
    assert_eq!(
        run_as("mistarr", "mistarr:x:900:901::/:/bin/false").unwrap(),
        account(900, 901)
    );
    assert_eq!(run_as("1234", passwd).unwrap(), account(1234, 1234));
    assert_eq!(run_as("1234:99", passwd).unwrap(), account(1234, 99));
    assert_eq!(run_as("root", passwd).unwrap(), RunAs::Root);
    assert_eq!(run_as("0:0", passwd).unwrap(), RunAs::Root);
    assert!(matches!(
        run_as("ghost", passwd),
        Err(HardenError::UnknownUser(_))
    ));
    for bad in ["", "12x", "1:2:3", "a:b", "-1"] {
        assert!(run_as(bad, passwd).is_err(), "{bad:?}");
    }
}

#[test]
fn only_a_low_port_keeps_the_bind_capability() {
    let base = CapabilitySet::DAC_OVERRIDE | CapabilitySet::KILL;
    assert_eq!(kept("0.0.0.0:8420"), base);
    assert_eq!(kept("127.0.0.1:1024"), base);
    assert_eq!(kept("no port"), base);
    assert_eq!(kept("0.0.0.0:443"), base | CapabilitySet::NET_BIND_SERVICE);
}

#[test]
fn the_filter_checks_the_architecture_then_denies_each_listed_call() {
    let p = filter().expect("a filter for the test target");
    assert_eq!((p[0].code, p[0].k), (LD_W_ABS, 4));
    assert_eq!((p[1].code, p[1].k), (JEQ_K, AUDIT_ARCH.unwrap()));
    assert_eq!(p[2].k, libc::SECCOMP_RET_KILL_PROCESS);
    assert_eq!(p.last().map(|i| i.k), Some(libc::SECCOMP_RET_ALLOW));
    for &nr in DENIED {
        let at = p
            .iter()
            .position(|i| i.code == JEQ_K && i.k == u32::try_from(nr).unwrap())
            .unwrap_or_else(|| panic!("syscall {nr} is checked"));
        assert_eq!((p[at].jt, p[at].jf), (0, 1));
        assert_eq!(
            p[at + 1].k & libc::SECCOMP_RET_ERRNO,
            libc::SECCOMP_RET_ERRNO
        );
    }
    for nr in [
        libc::SYS_read,
        libc::SYS_write,
        libc::SYS_clone,
        libc::SYS_execve,
    ] {
        let nr = u32::try_from(nr).unwrap();
        assert!(
            !p.iter().any(|i| i.code == JEQ_K && i.k == nr),
            "{nr} stays allowed"
        );
    }
}

/// Runs [`applied_alone`] in a child test process, since `no_new_privs` and the
/// filter cannot be undone in this one.
#[test]
fn applying_denies_the_listed_calls_and_keeps_the_rest() {
    let module = module_path!().split_once("::").map_or("", |(_, m)| m);
    let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .arg(format!("{module}::applied_alone"))
        .args(["--exact", "--ignored", "--nocapture", "--test-threads=1"])
        .output()
        .expect("run");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success() && text.contains("1 passed"), "{text}");
}

#[test]
#[ignore = "run by applying_denies_the_listed_calls_and_keeps_the_rest in a process of its own"]
fn applied_alone() {
    let dir = tempfile::tempdir().expect("dir");
    let me = rustix::process::pidfd_open(
        rustix::process::getpid(),
        rustix::process::PidfdFlags::empty(),
    )
    .expect("pidfd");
    let probe = || {
        rustix::process::pidfd_getfd(
            &me,
            me.as_raw_fd(),
            rustix::process::PidfdGetfdFlags::empty(),
        )
    };
    probe().expect("allowed before the filter");

    let options = Options {
        keep: kept(""),
        seccomp: true,
        writable: Some(vec![dir.path().to_path_buf()]),
        ram_dir: dir.path().to_path_buf(),
    };
    let report = apply(RunAs::Root, &options).expect("apply");
    assert_eq!(
        report.jail,
        Jail::Off,
        "only a drop from root enters the view"
    );
    assert_eq!(
        report.account, None,
        "nothing is dropped from a non-root or Root run"
    );
    assert!(rustix::thread::no_new_privs().expect("prctl"));
    let status = std::fs::read_to_string("/proc/thread-self/status").expect("status");
    match report.seccomp {
        Seccomp::On => {
            assert!(status.lines().any(|l| l == "Seccomp:\t2"), "{status}");
            assert_eq!(probe().map(drop), Err(Errno::PERM));
        }
        Seccomp::Unavailable(why) => eprintln!("seccomp unavailable here: {why}"),
        Seccomp::Off => panic!("asked for the filter"),
    }
    std::fs::write(dir.path().join("f"), b"x").expect("files still work");
    let out = std::process::Command::new("true")
        .status()
        .expect("programs still start");
    assert!(out.success());
}

proptest! {
    #[test]
    fn lookup_and_run_as_never_panic(text in "[a-z0-9:\n]{0,80}", name in "[a-z0-9:]{0,12}") {
        let _ = lookup(&text, &name);
        let _ = run_as(&name, &text);
    }

    #[test]
    fn a_written_entry_is_found(name in "[a-z][a-z0-9_]{0,15}", uid in 1u32.., gid in any::<u32>()) {
        prop_assume!(name != "other");
        let passwd = format!("other:x:1:1::/:/bin/sh\n{name}:x:{uid}:{gid}::/:/bin/false\n");
        prop_assert_eq!(lookup(&passwd, &name), Some(Account { uid, gid }));
    }
}

#[test]
fn mountinfo_gives_each_mount_point_once_with_escapes_undone() {
    let info = "22 1 0:21 / / rw shared:1 - ext4 /dev/root rw\n\
                30 22 179:1 / /media/fat rw - exfat /dev/mmcblk0p1 rw\n\
                31 30 0:5 / /media/fat/a\\040b\\134c rw - tmpfs t rw\n\
                32 22 0:6 / /media/fat rw - tmpfs t rw\n\
                short line\n";
    let points = jail::mount_points(info);
    let want = ["/", "/media/fat", "/media/fat/a b\\c"].map(std::path::PathBuf::from);
    assert_eq!(points, want);
}

#[test]
fn only_the_mount_a_writable_directory_lives_on_stays_open_above_it() {
    use jail::{place, Place};
    let p = |s: &str| std::path::PathBuf::from(s);
    let mounts = [
        "/",
        "/proc",
        "/dev",
        "/tmp",
        "/media/fat",
        "/media/fat/games/usb",
        "/media/usb0",
    ]
    .map(p);
    let writable = [p("/media/fat/games"), p("/media/fat/mistarr"), p("/tmp")];
    let at = |m: &str| place(&p(m), &writable, &mounts);
    assert_eq!(at("/media/fat"), Place::Above);
    assert_eq!(at("/media/fat/games/usb"), Place::Inside);
    assert_eq!(at("/tmp"), Place::Inside);
    for apart in ["/", "/proc", "/dev", "/media/usb0"] {
        assert_eq!(at(apart), Place::Apart, "{apart}");
    }
    let no_tmp_mount = ["/", "/media/fat"].map(p);
    assert_eq!(place(&p("/"), &writable, &no_tmp_mount), Place::Above);
}

proptest! {
    #[test]
    fn mountinfo_parsing_never_panics(text in "[ -~\n\\\\]{0,200}") {
        let _ = jail::mount_points(&text);
    }
}
