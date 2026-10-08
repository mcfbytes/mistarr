//! The private mount view entered before dropping root; see `docs/DEPLOYMENT.md` "Privileges".

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt as _;
use std::os::unix::fs::FileTypeExt as _;
use std::path::{Path, PathBuf};

use rustix::mount::{MountFlags, MountPropagationFlags};

/// Character devices that stay usable once every other device node is refused.
pub const DEVICES: &[&str] = &[
    "/dev/null",
    "/dev/zero",
    "/dev/full",
    "/dev/random",
    "/dev/urandom",
    "/dev/tty",
];

/// The mount points `/proc/self/mountinfo` lists, in its order, each once.
///
/// ```
/// use mistarr_server::harden::jail::mount_points;
/// let info = "22 1 0:21 / / rw - ext4 /dev/root rw\n\
///             30 22 179:1 / /media/my\\040card rw - exfat /dev/mmcblk0p1 rw\n";
/// assert_eq!(mount_points(info), ["/", "/media/my card"].map(std::path::PathBuf::from));
/// ```
#[must_use]
pub fn mount_points(mountinfo: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for line in mountinfo.lines() {
        if let Some(field) = line.split(' ').nth(4) {
            let path = PathBuf::from(unescape(field));
            if !out.contains(&path) {
                out.push(path);
            }
        }
    }
    out
}

/// Undoes the `\ooo` octal escapes mountinfo writes for space, tab, newline and backslash.
fn unescape(field: &str) -> OsString {
    let b = field.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let code = b.get(i + 1..i + 4).and_then(|d| {
            let s = std::str::from_utf8(d).ok()?;
            u8::from_str_radix(s, 8).ok()
        });
        match (b[i], code) {
            (b'\\', Some(c)) => {
                out.push(c);
                i += 4;
            }
            (c, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    OsString::from_vec(out)
}

/// Where a mount stands against the directories the server writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// At or below a writable directory: stays writable.
    Inside,
    /// The mount a writable directory lives on: stays writable, and everything
    /// in it off the way to those directories is made read-only.
    Above,
    /// Holds none: read-only.
    Apart,
}

/// Classifies the mount at `mount` against `writable`, given every mount point.
///
/// ```
/// use mistarr_server::harden::jail::{place, Place};
/// let mounts = ["/", "/tmp", "/media/fat", "/sys"].map(std::path::PathBuf::from);
/// let w = ["/media/fat/games".into(), "/tmp".into()];
/// assert_eq!(place("/media/fat".as_ref(), &w, &mounts), Place::Above);
/// assert_eq!(place("/tmp".as_ref(), &w, &mounts), Place::Inside);
/// assert_eq!(place("/".as_ref(), &w, &mounts), Place::Apart);
/// assert_eq!(place("/sys".as_ref(), &w, &mounts), Place::Apart);
/// ```
#[must_use]
pub fn place(mount: &Path, writable: &[PathBuf], mounts: &[PathBuf]) -> Place {
    let owner = |w: &PathBuf| {
        mounts
            .iter()
            .filter(|m| w.starts_with(m))
            .max_by_key(|m| m.components().count())
    };
    if writable.iter().any(|w| mount.starts_with(w)) {
        Place::Inside
    } else if writable
        .iter()
        .any(|w| owner(w).is_some_and(|m| m == mount))
    {
        Place::Above
    } else {
        Place::Apart
    }
}

/// Moves this thread into a mount namespace of its own in which only `writable`
/// (created first) and the mounts below them can be written, and device nodes
/// other than [`DEVICES`] cannot be opened. Needs `CAP_SYS_ADMIN`; call it as
/// root before any thread starts. Returns what could not be narrowed.
///
/// # Errors
///
/// Why the namespace could not be made; the mounts are then unchanged.
pub fn enter(writable: &[PathBuf]) -> Result<Vec<String>, String> {
    let mut warnings = Vec::new();
    let mut dirs = Vec::new();
    for w in writable {
        match std::fs::create_dir_all(w).and_then(|()| std::fs::canonicalize(w)) {
            Ok(dir) => dirs.push(dir),
            Err(e) => warnings.push(format!("{}: {e}", w.display())),
        }
    }
    let info = std::fs::read_to_string("/proc/self/mountinfo").map_err(|e| e.to_string())?;
    #[expect(
        deprecated,
        reason = "NEWNS alone shares no file table, so the safe call is sound; unsafe code is forbidden"
    )]
    rustix::thread::unshare(rustix::thread::UnshareFlags::NEWNS).map_err(|e| e.to_string())?;
    // Nothing done below may reach the host's mounts.
    let propagation = MountPropagationFlags::DOWNSTREAM | MountPropagationFlags::REC;
    if let Err(e) = rustix::mount::mount_change("/", propagation) {
        return Err(format!("cannot make mounts private: {e}"));
    }
    for dev in DEVICES {
        let is_char = std::fs::metadata(dev).is_ok_and(|m| m.file_type().is_char_device());
        if is_char {
            if let Err(e) = rustix::mount::mount_bind(*dev, *dev) {
                warnings.push(format!("{dev}: {e}"));
            }
        }
    }
    let mounts = mount_points(&info);
    for m in &mounts {
        if place(m, &dirs, &mounts) == Place::Above {
            protect_beside(m, &dirs, &mut warnings);
        }
    }
    for m in &mounts {
        let ro = place(m, &dirs, &mounts) == Place::Apart;
        if let Err(e) = narrow(m, ro) {
            warnings.push(format!("{}: {e}", m.display()));
        }
    }
    Ok(warnings)
}

/// Makes every entry of `dir` that is neither in `dirs` nor on the way to one
/// read-only, by binding it over itself, and recurses into those on the way.
fn protect_beside(dir: &Path, dirs: &[PathBuf], warnings: &mut Vec<String>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            warnings.push(format!("{}: {e}", dir.display()));
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if dirs.contains(&path) || kind.is_symlink() {
            continue;
        }
        if kind.is_dir() && dirs.iter().any(|w| w.starts_with(&path)) {
            protect_beside(&path, dirs, warnings);
            continue;
        }
        let done = rustix::mount::mount_bind_recursive(&path, &path)
            .map_err(std::io::Error::from)
            .and_then(|()| narrow(&path, true));
        if let Err(e) = done {
            warnings.push(format!("{}: {e}", path.display()));
        }
    }
}

/// Remounts the top mount at `path` `nosuid,nodev`, and read-only when `ro`,
/// keeping its `noexec`.
fn narrow(path: &Path, ro: bool) -> std::io::Result<()> {
    let mut flags = MountFlags::BIND | MountFlags::NOSUID | MountFlags::NODEV;
    if ro {
        flags |= MountFlags::RDONLY;
    }
    let current = rustix::fs::statvfs(path)?.f_flag;
    if current.contains(rustix::fs::StatVfsMountFlags::NOEXEC) {
        flags |= MountFlags::NOEXEC;
    }
    rustix::mount::mount_remount(path, flags, "")?;
    Ok(())
}
