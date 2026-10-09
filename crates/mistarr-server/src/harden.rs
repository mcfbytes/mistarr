//! Dropping root and narrowing what the server may do; see `docs/DEPLOYMENT.md` "Privileges".

use std::path::{Path, PathBuf};

use rustix::fs::{Mode, OFlags};
use rustix::io::Errno;
use rustix::process::{Gid, Uid};
use rustix::thread::{CapabilitySet, CapabilitySets};

/// The account the server runs as when it starts as root.
pub const DEFAULT_USER: &str = "mistarr";

/// The uid and gid of [`DEFAULT_USER`] when `/etc/passwd` has no entry for it.
pub const DEFAULT_ID: u32 = 8420;

/// Names the account: a user name, `uid[:gid]`, or `root` to stay root.
pub const USER_ENV: &str = "MISTARR_USER";

/// Set to `off` to run without the seccomp filter.
pub const SECCOMP_ENV: &str = "MISTARR_SECCOMP";

/// Set to `off` to drop root without the private mount view of [`jail`].
pub const JAIL_ENV: &str = "MISTARR_JAIL";

/// The user database read to resolve [`USER_ENV`].
pub const PASSWD: &str = "/etc/passwd";

/// Kept by the programs the server starts, which write the card as well.
pub const INHERITED: CapabilitySet = CapabilitySet::DAC_OVERRIDE;

/// Why the server could not narrow its privileges.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HardenError {
    /// [`USER_ENV`] names a user that `/etc/passwd` does not list.
    #[error("{USER_ENV}: no user {0} in /etc/passwd")]
    UnknownUser(String),
    /// [`USER_ENV`] is neither a name nor `uid[:gid]`.
    #[error("{USER_ENV}: {0:?} is not a user name or uid[:gid]")]
    BadSpec(String),
    /// Ids are per thread underneath, so a second thread would keep root.
    #[error("cannot drop root with {0} threads running")]
    Threaded(usize),
    /// A step of the drop failed; the process must not go on half-dropped.
    #[error("{step}: {source}")]
    Step {
        /// What was being done.
        step: &'static str,
        /// The failure.
        source: std::io::Error,
    },
}

fn step(step: &'static str) -> impl FnOnce(Errno) -> HardenError {
    move |e| HardenError::Step {
        step,
        source: e.into(),
    }
}

/// A user and group id to run as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Account {
    /// The user id.
    pub uid: u32,
    /// The primary group id; no supplementary groups are kept.
    pub gid: u32,
}

/// Who the server runs as once started as root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunAs {
    /// Stay root with every capability.
    Root,
    /// Switch to this account.
    Account(Account),
}

/// The account `name` has in `passwd`, in `/etc/passwd` format.
///
/// ```
/// use mistarr_server::harden::{lookup, Account};
/// let passwd = "root:x:0:0::/root:/bin/sh\nmistarr:x:8420:8421::/:/bin/false\n";
/// assert_eq!(lookup(passwd, "mistarr"), Some(Account { uid: 8420, gid: 8421 }));
/// assert_eq!(lookup(passwd, "nobody"), None);
/// ```
#[must_use]
pub fn lookup(passwd: &str, name: &str) -> Option<Account> {
    passwd.lines().find_map(|line| {
        let mut f = line.split(':');
        if f.next()? != name {
            return None;
        }
        let uid = f.nth(1)?.parse().ok()?;
        let gid = f.next()?.parse().ok()?;
        Some(Account { uid, gid })
    })
}

/// Resolves a [`USER_ENV`] value against `passwd`: `root` or uid 0 stays root,
/// `uid[:gid]` is taken as given (gid defaulting to uid), and a name is looked
/// up, with [`DEFAULT_USER`] falling back to [`DEFAULT_ID`] when it is missing.
///
/// # Errors
///
/// [`HardenError::BadSpec`] for a malformed value, [`HardenError::UnknownUser`]
/// for a name other than [`DEFAULT_USER`] that `passwd` lacks.
///
/// ```
/// use mistarr_server::harden::{run_as, Account, RunAs, DEFAULT_ID};
/// let id = Account { uid: DEFAULT_ID, gid: DEFAULT_ID };
/// assert_eq!(run_as("mistarr", "").unwrap(), RunAs::Account(id));
/// assert_eq!(run_as("100:50", "").unwrap(), RunAs::Account(Account { uid: 100, gid: 50 }));
/// assert_eq!(run_as("root", "").unwrap(), RunAs::Root);
/// assert!(run_as("someone", "").is_err());
/// ```
pub fn run_as(spec: &str, passwd: &str) -> Result<RunAs, HardenError> {
    let spec = spec.trim();
    let bad = || HardenError::BadSpec(spec.to_owned());
    let account = if spec == "root" {
        return Ok(RunAs::Root);
    } else if spec.starts_with(|c: char| c.is_ascii_digit()) {
        let (uid, gid) = spec.split_once(':').unwrap_or((spec, spec));
        Account {
            uid: uid.parse().map_err(|_| bad())?,
            gid: gid.parse().map_err(|_| bad())?,
        }
    } else if spec.is_empty() || spec.contains([':', '\n']) {
        return Err(bad());
    } else if let Some(account) = lookup(passwd, spec) {
        account
    } else if spec == DEFAULT_USER {
        Account {
            uid: DEFAULT_ID,
            gid: DEFAULT_ID,
        }
    } else {
        return Err(HardenError::UnknownUser(spec.to_owned()));
    };
    Ok(if account.uid == 0 {
        RunAs::Root
    } else {
        RunAs::Account(account)
    })
}

/// The capabilities kept after dropping root: `DAC_OVERRIDE` to write the card,
/// whose exFAT files all belong to root, `KILL` to stop a client run by another
/// user while a core runs, and `NET_BIND_SERVICE` only for a port below 1024.
///
/// ```
/// use mistarr_server::harden::kept;
/// use rustix::thread::CapabilitySet;
/// assert!(!kept("0.0.0.0:8420").contains(CapabilitySet::NET_BIND_SERVICE));
/// assert!(kept("[::]:80").contains(CapabilitySet::NET_BIND_SERVICE));
/// ```
#[must_use]
pub fn kept(listen: &str) -> CapabilitySet {
    let base = CapabilitySet::DAC_OVERRIDE | CapabilitySet::KILL;
    let port = listen
        .rsplit_once(':')
        .and_then(|(_, p)| p.parse::<u16>().ok());
    if port.is_some_and(|p| p < 1024) {
        base | CapabilitySet::NET_BIND_SERVICE
    } else {
        base
    }
}

/// Whether the seccomp filter went in.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Seccomp {
    /// The filter is installed.
    On,
    /// [`SECCOMP_ENV`] turned it off.
    Off,
    /// The kernel or architecture has no seccomp filters; why.
    Unavailable(String),
}

/// Whether the server runs in the private mount view of [`jail`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Jail {
    /// It does; the mounts or entries that could not be narrowed, if any.
    On(Vec<String>),
    /// Not asked for, or the server kept its ids.
    Off,
    /// The kernel refused the mount namespace; why.
    Unavailable(String),
}

/// What [`apply`] did, logged once logging is up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// The account switched to; `None` when the process kept its ids.
    pub account: Option<Account>,
    /// The effective capabilities afterwards.
    pub capabilities: CapabilitySet,
    /// Capabilities programs the server starts inherit.
    pub inherited: CapabilitySet,
    /// The seccomp filter's state.
    pub seccomp: Seccomp,
    /// The mount view's state.
    pub jail: Jail,
}

impl Report {
    /// Logs the outcome at info, or warns when the server still runs as root.
    pub fn log(&self) {
        let names = |set: CapabilitySet| {
            set.iter_names()
                .map(|(n, _)| n)
                .collect::<Vec<_>>()
                .join(",")
        };
        let seccomp = match &self.seccomp {
            Seccomp::On => "on".to_owned(),
            Seccomp::Off => "off".to_owned(),
            Seccomp::Unavailable(why) => format!("unavailable ({why})"),
        };
        let jail = match &self.jail {
            Jail::On(warnings) => {
                for w in warnings {
                    tracing::warn!(what = %w, "left writable in the mount view");
                }
                "on".to_owned()
            }
            Jail::Off => "off".to_owned(),
            Jail::Unavailable(why) => format!("unavailable ({why})"),
        };
        match self.account {
            Some(a) => tracing::info!(
                uid = a.uid,
                gid = a.gid,
                capabilities = names(self.capabilities),
                inherited = names(self.inherited),
                jail,
                seccomp,
                "dropped root"
            ),
            None if rustix::process::geteuid().is_root() => {
                tracing::warn!(seccomp, "running as root with every capability");
            }
            None => tracing::info!(seccomp, "running as an unprivileged user"),
        }
    }
}

/// What [`apply`] is asked to do besides switching account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// Capabilities to keep, from [`kept`].
    pub keep: CapabilitySet,
    /// Install the seccomp [`filter`].
    pub seccomp: bool,
    /// The only directories left writable in the mount view; `None` for no view.
    pub writable: Option<Vec<PathBuf>>,
    /// The RAM directory a root run may have left, handed to the account.
    pub ram_dir: PathBuf,
}

/// Resolves the account from the environment and [`PASSWD`], then calls
/// [`apply`] keeping what [`kept`] gives for `listen`, with `writable` (the
/// RAM directory among them) the only directories that can be written.
///
/// # Errors
///
/// As [`run_as`] and [`apply`].
pub fn for_server(
    listen: &str,
    ram_dir: &Path,
    writable: Vec<PathBuf>,
) -> Result<Report, HardenError> {
    let spec = std::env::var(USER_ENV).unwrap_or_else(|_| DEFAULT_USER.to_owned());
    let passwd = std::fs::read_to_string(PASSWD).unwrap_or_default();
    let off = |var| std::env::var(var).is_ok_and(|v| v == "off");
    let options = Options {
        keep: kept(listen),
        seccomp: !off(SECCOMP_ENV),
        writable: (!off(JAIL_ENV)).then_some(writable),
        ram_dir: ram_dir.to_path_buf(),
    };
    apply(run_as(&spec, &passwd)?, &options)
}

/// Started as root with an account to run as: hands the RAM directory over to
/// it, enters the [`jail`] view when asked, drops to the account keeping only
/// the capabilities asked for, and marks the process not dumpable. Then, as any
/// user, sets `no_new_privs` and installs [`filter`] when asked and the kernel
/// supports it. Call it before any thread starts.
///
/// # Errors
///
/// [`HardenError::Threaded`] when other threads run, [`HardenError::Step`] when
/// a step of the drop or `no_new_privs` fails.
pub fn apply(run_as: RunAs, options: &Options) -> Result<Report, HardenError> {
    let (account, jail) = match run_as {
        RunAs::Account(a) if rustix::process::geteuid().is_root() => {
            (Some(a), drop_root(a, options)?)
        }
        _ => (None, Jail::Off),
    };
    rustix::thread::set_no_new_privs(true).map_err(step("no_new_privs"))?;
    let seccomp = if options.seccomp {
        install_filter()
    } else {
        Seccomp::Off
    };
    let capabilities =
        rustix::thread::capabilities(None).map_or(CapabilitySet::empty(), |c| c.effective);
    let inherited = (0..64)
        .map(|bit| CapabilitySet::from_bits_retain(1 << bit))
        .filter(|c| rustix::thread::capability_is_in_ambient_set(*c).unwrap_or(false))
        .collect();
    Ok(Report {
        account,
        capabilities,
        inherited,
        seccomp,
        jail,
    })
}

fn drop_root(a: Account, options: &Options) -> Result<Jail, HardenError> {
    let keep = options.keep;
    let threads = std::fs::read_dir("/proc/self/task")
        .map_err(|source| HardenError::Step {
            step: "count threads",
            source,
        })?
        .count();
    if threads != 1 {
        return Err(HardenError::Threaded(threads));
    }
    let jail = match &options.writable {
        Some(writable) => match jail::enter(writable) {
            Ok(warnings) => Jail::On(warnings),
            Err(why) => Jail::Unavailable(why),
        },
        None => Jail::Off,
    };
    // After the view, which may create it; a failure shows when the directory is refused as another user's.
    let _ = hand_over(&options.ram_dir, a);
    for bit in 0..64 {
        let cap = CapabilitySet::from_bits_retain(1 << bit);
        if keep.contains(cap) {
            continue;
        }
        match rustix::thread::remove_capability_from_bounding_set(cap) {
            Ok(()) | Err(Errno::INVAL) => {}
            Err(e) => return Err(step("bounding set")(e)),
        }
    }
    rustix::thread::set_keep_capabilities(true).map_err(step("keep capabilities"))?;
    rustix::thread::set_thread_groups(&[]).map_err(step("setgroups"))?;
    let gid = Gid::from_raw(a.gid);
    rustix::thread::set_thread_res_gid(gid, gid, gid).map_err(step("setresgid"))?;
    let uid = Uid::from_raw(a.uid);
    rustix::thread::set_thread_res_uid(uid, uid, uid).map_err(step("setresuid"))?;
    let sets = CapabilitySets {
        effective: keep,
        permitted: keep,
        inheritable: keep & INHERITED,
    };
    rustix::thread::set_capabilities(None, sets).map_err(step("capset"))?;
    for cap in (keep & INHERITED).iter() {
        // Without ambient capabilities (before Linux 4.3) the started programs only lose card writes.
        let _ = rustix::thread::configure_capability_in_ambient_set(cap, true);
    }
    rustix::thread::set_keep_capabilities(false).map_err(step("keep capabilities"))?;
    rustix::process::set_dumpable_behavior(rustix::process::DumpableBehavior::NotDumpable)
        .map_err(step("dumpable"))?;
    Ok(jail)
}

/// Gives `dir` and the regular files directly in it to `a` when root owns them,
/// following no link. Absent is fine.
fn hand_over(dir: &Path, a: Account) -> rustix::io::Result<()> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let fd = match rustix::fs::open(dir, flags, Mode::empty()) {
        Err(Errno::NOENT) => return Ok(()),
        other => other?,
    };
    let (uid, gid) = (Some(Uid::from_raw(a.uid)), Some(Gid::from_raw(a.gid)));
    if rustix::fs::fstat(&fd)?.st_uid != 0 {
        return Ok(());
    }
    rustix::fs::fchown(&fd, uid, gid)?;
    for entry in rustix::fs::Dir::read_from(&fd)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == c"." || name == c".." {
            continue;
        }
        let file_flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        let Ok(file) = rustix::fs::openat(&fd, name, file_flags, Mode::empty()) else {
            continue;
        };
        let st = rustix::fs::fstat(&file)?;
        if rustix::fs::FileType::from_raw_mode(st.st_mode) == rustix::fs::FileType::RegularFile
            && st.st_uid == 0
        {
            rustix::fs::fchown(&file, uid, gid)?;
        }
    }
    Ok(())
}

/// Syscalls the filter answers with `EPERM`. None is used by the server or the
/// clients it starts; each is kernel attack surface or needs a dropped capability.
pub const DENIED: &[libc::c_long] = &[
    libc::SYS_io_uring_setup,
    libc::SYS_io_uring_enter,
    libc::SYS_io_uring_register,
    libc::SYS_perf_event_open,
    libc::SYS_bpf,
    libc::SYS_userfaultfd,
    libc::SYS_keyctl,
    libc::SYS_add_key,
    libc::SYS_request_key,
    libc::SYS_ptrace,
    libc::SYS_process_vm_readv,
    libc::SYS_process_vm_writev,
    libc::SYS_process_madvise,
    libc::SYS_kcmp,
    libc::SYS_pidfd_getfd,
    libc::SYS_mount,
    libc::SYS_umount2,
    libc::SYS_pivot_root,
    libc::SYS_chroot,
    libc::SYS_mount_setattr,
    libc::SYS_move_mount,
    libc::SYS_open_tree,
    libc::SYS_fsopen,
    libc::SYS_fsconfig,
    libc::SYS_fsmount,
    libc::SYS_fspick,
    libc::SYS_unshare,
    libc::SYS_setns,
    libc::SYS_swapon,
    libc::SYS_swapoff,
    libc::SYS_reboot,
    libc::SYS_kexec_load,
    libc::SYS_init_module,
    libc::SYS_finit_module,
    libc::SYS_delete_module,
    libc::SYS_acct,
    libc::SYS_syslog,
    libc::SYS_settimeofday,
    libc::SYS_adjtimex,
    libc::SYS_name_to_handle_at,
    libc::SYS_open_by_handle_at,
    libc::SYS_fanotify_init,
    libc::SYS_sethostname,
    libc::SYS_setdomainname,
    libc::SYS_vhangup,
];

/// `AUDIT_ARCH_*` of the build target, which the filter requires of every call:
/// the ELF machine (`EM_ARM`, `EM_X86_64`) with the little-endian and 64-bit flags.
#[cfg(target_arch = "arm")]
const AUDIT_ARCH: Option<u32> = Some(0x28 | 0x4000_0000);
#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: Option<u32> = Some(0x3e | 0x8000_0000 | 0x4000_0000);
#[cfg(not(any(target_arch = "arm", target_arch = "x86_64")))]
const AUDIT_ARCH: Option<u32> = None;

const LD_W_ABS: u16 = 0x20;
const JEQ_K: u16 = 0x15;
const JGE_K: u16 = 0x35;
const RET_K: u16 = 0x06;

fn insn(code: u16, k: u32, jt: u8, jf: u8) -> seccompiler::sock_filter {
    seccompiler::sock_filter { code, jt, jf, k }
}

/// A classic BPF seccomp program: kill a call made under another architecture,
/// answer [`DENIED`] with `EPERM`, allow everything else. `None` on a target
/// it has no architecture number for.
///
/// ```
/// let p = mistarr_server::harden::filter().unwrap();
/// assert!(p.len() > 2 * mistarr_server::harden::DENIED.len());
/// ```
#[must_use]
pub fn filter() -> Option<seccompiler::BpfProgram> {
    let arch = AUDIT_ARCH?;
    let eperm = libc::SECCOMP_RET_ERRNO | libc::EPERM.unsigned_abs();
    // seccomp_data: nr at offset 0, arch at offset 4.
    let mut p = vec![
        insn(LD_W_ABS, 4, 0, 0),
        insn(JEQ_K, arch, 1, 0),
        insn(RET_K, libc::SECCOMP_RET_KILL_PROCESS, 0, 0),
        insn(LD_W_ABS, 0, 0, 0),
    ];
    if cfg!(target_arch = "x86_64") {
        // x32 calls carry bit 30 under the same architecture number.
        p.extend([insn(JGE_K, 0x4000_0000, 0, 1), insn(RET_K, eperm, 0, 0)]);
    }
    for &nr in DENIED {
        let nr = u32::try_from(nr).ok()?;
        p.extend([insn(JEQ_K, nr, 0, 1), insn(RET_K, eperm, 0, 0)]);
    }
    p.push(insn(RET_K, libc::SECCOMP_RET_ALLOW, 0, 0));
    Some(p)
}

fn install_filter() -> Seccomp {
    let Some(program) = filter() else {
        return Seccomp::Unavailable("no filter for this architecture".into());
    };
    match seccompiler::apply_filter(&program) {
        Ok(()) => Seccomp::On,
        Err(e) => Seccomp::Unavailable(e.to_string()),
    }
}

pub mod jail;

#[cfg(test)]
mod tests;
