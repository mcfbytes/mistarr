//! `mistarr doctor`: the checks listed in `docs/DEPLOYMENT.md` "Runtime checks on the board".

use std::io::{self, Read, Write};
use std::path::Path;
use std::time::Instant;

use mistarr_core::hash::{hash_reader, HeaderRule};

use crate::config::Config;
use crate::jobs::{corename, detect_client};
use crate::status::{free_bytes, mem_available_bytes};

/// Default size of the hashing benchmark in MiB.
pub const DEFAULT_HASH_MIB: u32 = 64;

const MIB: u64 = 1024 * 1024;

/// ELF program header type naming the dynamic loader.
const PT_INTERP: u64 = 3;

/// Whether an ELF image asks for a dynamic loader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Linkage {
    /// No `PT_INTERP` program header.
    Static,
    /// Loaded through this interpreter.
    Dynamic(String),
}

/// Reads the ELF program headers of `image` and reports its linkage.
///
/// # Errors
///
/// [`io::ErrorKind::InvalidData`] when `image` is not a well-formed ELF file.
///
/// ```
/// let exe = std::fs::read(std::env::current_exe().unwrap()).unwrap();
/// assert!(mistarr_server::doctor::linkage(&exe).is_ok());
/// assert!(mistarr_server::doctor::linkage(b"not elf").is_err());
/// ```
pub fn linkage(image: &[u8]) -> io::Result<Linkage> {
    let bad = || io::Error::new(io::ErrorKind::InvalidData, "not a valid ELF image");
    if image.len() < 0x34 || image[..4] != *b"\x7fELF" {
        return Err(bad());
    }
    let wide = match image[4] {
        1 => false,
        2 => true,
        _ => return Err(bad()),
    };
    let little = image[5] == 1;
    let num = |at: usize, len: usize| -> io::Result<u64> {
        let bytes = image.get(at..at + len).ok_or_else(bad)?;
        let mut v = 0u64;
        for i in 0..len {
            let b = if little { bytes[len - 1 - i] } else { bytes[i] };
            v = (v << 8) | u64::from(b);
        }
        Ok(v)
    };
    let as_usize = |v: u64| usize::try_from(v).map_err(|_| bad());
    let (phoff, phentsize, phnum) = if wide {
        (num(0x20, 8)?, num(0x36, 2)?, num(0x38, 2)?)
    } else {
        (num(0x1c, 4)?, num(0x2a, 2)?, num(0x2c, 2)?)
    };
    let (phoff, phentsize) = (as_usize(phoff)?, as_usize(phentsize)?);
    for i in 0..as_usize(phnum)? {
        let at = phoff + i * phentsize;
        if num(at, 4)? != PT_INTERP {
            continue;
        }
        let (off, size) = if wide {
            (num(at + 8, 8)?, num(at + 32, 8)?)
        } else {
            (num(at + 4, 4)?, num(at + 16, 4)?)
        };
        let (off, size) = (as_usize(off)?, as_usize(size)?);
        let raw = image.get(off..off + size).ok_or_else(bad)?;
        let interp = String::from_utf8_lossy(raw)
            .trim_end_matches('\0')
            .to_owned();
        return Ok(Linkage::Dynamic(interp));
    }
    Ok(Linkage::Static)
}

/// Creates and removes a probe file in `dir`.
///
/// # Errors
///
/// The error creating or removing the probe.
///
/// ```
/// assert!(mistarr_server::doctor::check_writable(&std::env::temp_dir()).is_ok());
/// ```
pub fn check_writable(dir: &Path) -> io::Result<()> {
    let probe = dir.join(".mistarr-doctor");
    std::fs::write(&probe, b"")?;
    std::fs::remove_file(&probe)
}

/// Hashes `mib` MiB of zeros and returns the seconds it took.
///
/// # Errors
///
/// Only I/O errors from the hasher, which a zero source does not produce.
///
/// ```
/// assert!(mistarr_server::doctor::hash_zeros(1).unwrap() >= 0.0);
/// ```
pub fn hash_zeros(mib: u32) -> io::Result<f64> {
    let len = u64::from(mib) * MIB;
    let start = Instant::now();
    hash_reader(io::repeat(0).take(len), HeaderRule::None, Some(len))?;
    Ok(start.elapsed().as_secs_f64())
}

fn mib(bytes: u64) -> String {
    // Display rounding only; exact values are not needed.
    #[allow(clippy::cast_precision_loss)]
    let v = bytes as f64 / MIB as f64;
    format!("{v:.0} MiB")
}

/// Runs every check and writes one line per item to `out`.
///
/// # Errors
///
/// Only errors writing to `out`; failed checks are reported in the output.
pub async fn run(config: &Config, hash_mib: u32, out: &mut impl Write) -> io::Result<()> {
    writeln!(out, "mistarr {}", env!("CARGO_PKG_VERSION"))?;
    let exe = std::env::current_exe().and_then(std::fs::read);
    match exe.and_then(|image| linkage(&image)) {
        Ok(Linkage::Static) => writeln!(out, "binary: static")?,
        Ok(Linkage::Dynamic(i)) => writeln!(out, "binary: dynamic (interpreter {i})")?,
        Err(e) => writeln!(out, "binary: unknown ({e})")?,
    }

    let paths = &config.paths;
    for (label, dir) in [
        ("data", paths.data.clone()),
        ("games", paths.games.clone()),
        ("dats", paths.dats()),
        ("sources", paths.sources()),
        ("staging", paths.staging()),
    ] {
        match check_writable(&dir) {
            Ok(()) => writeln!(out, "path {label} {}: writable", dir.display())?,
            Err(e) => writeln!(out, "path {label} {}: not writable ({e})", dir.display())?,
        }
    }
    for (label, dir) in [("data", &paths.data), ("games", &paths.games)] {
        match free_bytes(dir) {
            Some(b) => writeln!(out, "free space {label}: {}", mib(b))?,
            None => writeln!(out, "free space {label}: unknown")?,
        }
    }

    let client = detect_client::probe(&config.client).await;
    match (&client.kind, &client.url) {
        (Some(kind), Some(url)) => {
            let version = client.version.as_deref().unwrap_or("version unknown");
            let reach = if client.reachable {
                "reachable"
            } else {
                "not reachable"
            };
            writeln!(out, "client: {kind} {version} at {url}, {reach}")?;
        }
        _ => writeln!(out, "client: none detected")?,
    }
    let rt = if client.rtorrent_on_path { "yes" } else { "no" };
    writeln!(out, "rtorrent on PATH: {rt}")?;

    let cores = mistarr_mister::corename::installed_cores(&paths.root);
    let names: Vec<_> = cores.iter().map(|c| c.name.as_str()).collect();
    writeln!(
        out,
        "cores: {} installed ({})",
        names.len(),
        names.join(", ")
    )?;

    let core = corename::read(Path::new(mistarr_mister::CORENAME_PATH));
    writeln!(
        out,
        "corename: {}",
        core.as_deref().unwrap_or("not present")
    )?;

    match mem_available_bytes() {
        Some(b) => writeln!(out, "memory available: {}", mib(b))?,
        None => writeln!(out, "memory available: unknown")?,
    }

    match hash_zeros(hash_mib) {
        Ok(secs) => {
            let rate = f64::from(hash_mib) / secs.max(f64::EPSILON);
            writeln!(
                out,
                "hash: {hash_mib} MiB of zeros in {secs:.2} s ({rate:.1} MiB/s)"
            )?;
        }
        Err(e) => writeln!(out, "hash: failed ({e})")?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn elf32(interp: Option<&str>) -> Vec<u8> {
        let mut img = vec![0u8; 0x100];
        img[..4].copy_from_slice(b"\x7fELF");
        img[4] = 1;
        img[5] = 1;
        img[0x1c..0x20].copy_from_slice(&0x34u32.to_le_bytes());
        img[0x2a..0x2c].copy_from_slice(&32u16.to_le_bytes());
        img[0x2c..0x2e].copy_from_slice(&1u16.to_le_bytes());
        let ph = 0x34;
        let kind: u32 = if interp.is_some() { 3 } else { 1 };
        img[ph..ph + 4].copy_from_slice(&kind.to_le_bytes());
        if let Some(i) = interp {
            let bytes = format!("{i}\0");
            img[ph + 4..ph + 8].copy_from_slice(&0x80u32.to_le_bytes());
            let len = u32::try_from(bytes.len()).expect("len");
            img[ph + 16..ph + 20].copy_from_slice(&len.to_le_bytes());
            img[0x80..0x80 + bytes.len()].copy_from_slice(bytes.as_bytes());
        }
        img
    }

    #[test]
    fn elf_linkage_from_program_headers() {
        assert_eq!(linkage(&elf32(None)).expect("static"), Linkage::Static);
        assert_eq!(
            linkage(&elf32(Some("/lib/ld-test.so"))).expect("dynamic"),
            Linkage::Dynamic("/lib/ld-test.so".into())
        );
        let mut truncated = elf32(Some("/lib/ld"));
        truncated.truncate(0x40);
        assert!(linkage(&truncated).is_err());
    }

    #[test]
    fn writable_check_reports_failures() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(check_writable(dir.path()).is_ok());
        assert!(check_writable(&dir.path().join("missing")).is_err());
    }

    #[tokio::test]
    async fn report_has_every_item() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = Config::default();
        config.paths.root = dir.path().to_path_buf();
        config.paths.games = dir.path().join("games");
        config.paths.data = dir.path().join("data");
        config.client.kind = crate::config::ClientChoice::Rtorrent;
        config.client.url = "127.0.0.1:1".into();
        let mut out = Vec::new();
        run(&config, 1, &mut out).await.expect("run");
        let text = String::from_utf8(out).expect("utf8");
        for prefix in [
            "mistarr ",
            "binary: ",
            "path data ",
            "path games ",
            "free space data: ",
            "client: rtorrent",
            "rtorrent on PATH: ",
            "cores: 0 installed",
            "corename: ",
            "memory available: ",
            "hash: 1 MiB of zeros",
        ] {
            assert!(
                text.lines().any(|l| l.starts_with(prefix)),
                "{prefix}\n{text}"
            );
        }
        assert_eq!(mib(3 * MIB), "3 MiB");
    }
}
