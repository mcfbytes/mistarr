//! Starting cores and games through MiSTer Main's command FIFO; see
//! `docs/PLATFORMS.md` "Launch parameters" and `docs/ARCHITECTURE.md` "Launching".

use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::fs::FileTypeExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, PoisonError};

use rustix::fs::{Mode, OFlags};
use rustix::io::Errno;

use crate::corename::{rbf_files, RbfFile};
use crate::platforms::{Kind, Platform};
use crate::{Error, Result};

/// The FIFO MiSTer Main reads commands from.
pub const COMMAND_PATH: &str = "/dev/MiSTer_cmd";

/// Prefix of the MGL files written for game launches, one per launch.
pub const MGL_PREFIX: &str = "mistarr-";

/// How many launch MGL files are kept; older ones are removed after each write.
pub const MGL_KEEP: usize = 3;

static MGL_SEQ: AtomicU32 = AtomicU32::new(0);

/// Longest command line written in one call; POSIX makes writes up to `PIPE_BUF` atomic.
const PIPE_BUF: usize = 4096;

/// Largest cue sheet read when checking its `FILE` entries.
const CUE_LIMIT: u64 = 64 * 1024;

/// How MiSTer Main hands a game file to a core: the `type` of an MGL `<file>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LoadMode {
    /// `f`: load the file into the core's memory.
    File,
    /// `s`: mount the file as a disk or disc image.
    Mount,
}

impl LoadMode {
    /// The MGL `type` letter.
    ///
    /// ```
    /// assert_eq!(mistarr_mister::launch::LoadMode::Mount.letter(), 's');
    /// ```
    #[must_use]
    pub fn letter(self) -> char {
        match self {
            Self::File => 'f',
            Self::Mount => 's',
        }
    }
}

/// The `<file>` parameters of an MGL for one core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaunchSlot {
    /// Load or mount.
    pub mode: LoadMode,
    /// The core's file-slot index.
    pub index: u8,
    /// Seconds Main waits after loading the core before handing it the file.
    pub delay: u8,
    /// The values must be confirmed against a live board; see `BOARD_LAUNCH` in the platform tests.
    pub verify_on_board: bool,
}

/// A core that can launch a platform's games, with the parameters that belong to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaunchCore {
    /// The `.rbf` name without date suffix, compared case-insensitively.
    pub name: &'static str,
    /// The core may live under `_Arcade`; other rows never pick an `_Arcade` file.
    pub arcade_dir: bool,
    /// The MGL parameters for this core.
    pub slot: LaunchSlot,
}

/// An installed core chosen to launch a platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreFile {
    /// Absolute path of the `.rbf`.
    pub path: PathBuf,
    /// The MGL `<rbf>` value: the path relative to the SD root without date or extension.
    pub mgl_rbf: String,
    /// The MGL parameters of the chosen core.
    pub slot: LaunchSlot,
}

/// The core that launches `platform`: the first of [`Platform::launch`] with an
/// installed `.rbf`, and of those the newest by the `_YYYYMMDD` date in its name.
/// Undated files rank below dated ones and ties go to the shorter path. An
/// `_Arcade` file is chosen only for a core whose row names it with `arcade_dir`.
///
/// ```
/// let root = std::env::temp_dir().join("mistarr-doc-find-core");
/// std::fs::create_dir_all(root.join("_Console")).unwrap();
/// std::fs::write(root.join("_Console/NES_20240101.rbf"), b"").unwrap();
/// let nes = mistarr_mister::platforms::by_id("nes").unwrap();
/// let core = mistarr_mister::launch::find_core(&root, nes).unwrap();
/// assert_eq!(core.mgl_rbf, "_Console/NES");
/// ```
#[must_use]
pub fn find_core(root: &Path, platform: &Platform) -> Option<CoreFile> {
    let files = rbf_files(root);
    platform.launch.iter().find_map(|core| {
        let best = files
            .iter()
            .filter(|f| f.name.eq_ignore_ascii_case(core.name) && (core.arcade_dir || !f.arcade))
            .max_by(|a, b| {
                a.date
                    .cmp(&b.date)
                    .then_with(|| b.path.as_os_str().len().cmp(&a.path.as_os_str().len()))
                    .then_with(|| b.path.cmp(&a.path))
            })?;
        core_file(root, best, core.slot)
    })
}

fn core_file(root: &Path, rbf: &RbfFile, slot: LaunchSlot) -> Option<CoreFile> {
    let dir = rbf.path.parent()?.strip_prefix(root).ok()?;
    let mut mgl_rbf = String::new();
    for part in dir.components() {
        mgl_rbf.push_str(part.as_os_str().to_str()?);
        mgl_rbf.push('/');
    }
    mgl_rbf.push_str(&rbf.name);
    Some(CoreFile {
        path: rbf.path.clone(),
        mgl_rbf,
        slot,
    })
}

/// Splits a `files.rel_path` at the first `.zip#`, compared case-insensitively,
/// into the zip and the member inside it; any other `#` is part of a name.
///
/// ```
/// use mistarr_mister::launch::split_zip_member;
/// assert_eq!(split_zip_member("NES/a.ZIP#b.nes"), ("NES/a.ZIP", Some("b.nes")));
/// assert_eq!(split_zip_member("NES/No #1.nes"), ("NES/No #1.nes", None));
/// ```
#[must_use]
pub fn split_zip_member(rel_path: &str) -> (&str, Option<&str>) {
    let lower = rel_path.to_ascii_lowercase();
    match lower.find(".zip#") {
        Some(at) => (&rel_path[..at + 4], Some(&rel_path[at + 5..])),
        None => (rel_path, None),
    }
}

/// Splits a `files.rel_path` at the first `.chd#`, compared case-insensitively, into the
/// CHD image and the track member (`01`, `cue`) the scan recorded for it.
///
/// ```
/// use mistarr_mister::launch::split_chd_member;
/// assert_eq!(split_chd_member("PSX/G/g.CHD#01"), ("PSX/G/g.CHD", Some("01")));
/// assert_eq!(split_chd_member("PSX/G/g.chd"), ("PSX/G/g.chd", None));
/// ```
#[must_use]
pub fn split_chd_member(rel_path: &str) -> (&str, Option<&str>) {
    let lower = rel_path.to_ascii_lowercase();
    match lower.find(".chd#") {
        Some(at) => (&rel_path[..at + 4], Some(&rel_path[at + 5..])),
        None => (rel_path, None),
    }
}

/// The path, relative to `games/`, that loads a title from its files' `rel_path`s,
/// reading cue sheets under `games`:
/// - a disc loads the first cue sheet whose every `FILE` entry exists beside it,
///   else its `.chd` (a CHD track member stands for its image) or `.iso`;
/// - a romset loads its zip or its set directory, the first folder under the platform's;
/// - anything else loads its first file, a zip member `a.zip#b.nes` as `a.zip/b.nes`,
///   which Main opens inside the zip.
///
/// ```
/// use mistarr_mister::launch::game_path;
/// use mistarr_mister::platforms::Kind;
/// let games = std::path::Path::new("/nonexistent");
/// assert_eq!(game_path(Kind::Cartridge, games, &["NES/a.zip#b.nes"]).as_deref(), Some("NES/a.zip/b.nes"));
/// assert_eq!(game_path(Kind::Romset, games, &["NeoGeo/set"]).as_deref(), Some("NeoGeo/set"));
/// let members = ["PSX/G/g.CHD#01", "PSX/G/g.CHD#cue"];
/// assert_eq!(game_path(Kind::Disc, games, &members).as_deref(), Some("PSX/G/g.CHD"));
/// ```
#[must_use]
pub fn game_path(kind: Kind, games: &Path, rel_paths: &[&str]) -> Option<String> {
    match kind {
        Kind::Disc => {
            let images: Vec<&str> = rel_paths.iter().map(|p| split_chd_member(p).0).collect();
            rel_paths
                .iter()
                .copied()
                .find(|p| {
                    split_chd_member(p).1.is_none()
                        && has_extension(p, "cue")
                        && cue_complete(&games.join(p))
                })
                .or_else(|| {
                    ["chd", "iso"]
                        .iter()
                        .find_map(|ext| images.iter().copied().find(|p| has_extension(p, ext)))
                })
                .map(str::to_owned)
        }
        Kind::Romset => rel_paths.first().map(|p| romset_container(p)),
        _ => rel_paths.first().map(|p| match split_zip_member(p) {
            (zip, Some(member)) => format!("{zip}/{member}"),
            (file, None) => file.to_owned(),
        }),
    }
}

fn has_extension(path: &str, ext: &str) -> bool {
    path.rsplit_once('.')
        .is_some_and(|(_, e)| e.eq_ignore_ascii_case(ext))
}

/// The zip, or the set directory right below the platform's folder: a directory
/// romset is recorded as `NeoGeo/set` and any deeper member path keeps that prefix.
fn romset_container(rel_path: &str) -> String {
    if let (zip, Some(_)) = split_zip_member(rel_path) {
        return zip.to_owned();
    }
    let mut parts = rel_path.splitn(3, '/');
    match (parts.next(), parts.next()) {
        (Some(top), Some(set)) => format!("{top}/{set}"),
        _ => rel_path.to_owned(),
    }
}

/// Whether the cue sheet at `cue` names at least one file and every file it names
/// exists beside it; names that are absolute or leave the directory never count.
fn cue_complete(cue: &Path) -> bool {
    let Some(dir) = cue.parent() else {
        return false;
    };
    let Ok(file) = File::open(cue) else {
        return false;
    };
    let mut text = Vec::new();
    if file.take(CUE_LIMIT).read_to_end(&mut text).is_err() {
        return false;
    }
    let names = cue_files(&String::from_utf8_lossy(&text));
    !names.is_empty()
        && names.iter().all(|name| {
            let rel = Path::new(name);
            rel.components().all(|c| matches!(c, Component::Normal(_))) && dir.join(rel).is_file()
        })
}

/// The file names of a cue sheet's `FILE` lines, quoted or bare.
fn cue_files(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let rest = line.get(..5).filter(|k| k.eq_ignore_ascii_case("FILE "))?;
            let rest = line[rest.len()..].trim_start();
            let name = match rest.strip_prefix('"') {
                Some(quoted) => quoted.split('"').next()?,
                None => rest.split_whitespace().next()?,
            };
            Some(name.to_owned()).filter(|n| !n.is_empty())
        })
        .collect()
}

/// Builds an MGL document that starts core `rbf` (see [`CoreFile::mgl_rbf`]) and
/// hands it the absolute path `game` with that core's `slot` parameters.
///
/// # Errors
///
/// [`Error::UnsafePath`] when `game` is not absolute, not UTF-8 or holds a control
/// character, or `rbf` holds a control character; XML cannot carry those safely.
///
/// ```
/// use mistarr_mister::launch::{mgl, LaunchSlot, LoadMode};
/// let slot = LaunchSlot { mode: LoadMode::File, index: 1, delay: 2, verify_on_board: true };
/// let doc = mgl("_Console/NES", slot, std::path::Path::new("/media/fat/games/NES/A & B.nes")).unwrap();
/// assert!(doc.contains(r#"path="../../../../../media/fat/games/NES/A &amp; B.nes""#));
/// ```
pub fn mgl(rbf: &str, slot: LaunchSlot, game: &Path) -> Result<String> {
    let game_str = game
        .to_str()
        .filter(|g| game.is_absolute() && !has_control(g))
        .ok_or_else(|| Error::UnsafePath(game.display().to_string()))?;
    if has_control(rbf) {
        return Err(Error::UnsafePath(rbf.to_owned()));
    }
    let mut out = String::from("<mistergamedescription>\n  <rbf>");
    out.push_str(&escape(rbf));
    out.push_str("</rbf>\n");
    // Main resolves `path` from the core's games folder; climbing out reaches `/`.
    let _ = writeln!(
        out,
        "  <file delay=\"{}\" type=\"{}\" index=\"{}\" path=\"../../../../..{}\"/>",
        slot.delay,
        slot.mode.letter(),
        slot.index,
        escape(game_str)
    );
    out.push_str("</mistergamedescription>\n");
    Ok(out)
}

fn has_control(s: &str) -> bool {
    s.chars().any(char::is_control)
}

/// Escapes the five XML special characters, for text and double-quoted attributes.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// Writes `doc` to a new `mistarr-<millis>-<seq>.mgl` in `dir`, created exclusively so
/// no earlier launch's file is ever rewritten, then removes all but the newest
/// [`MGL_KEEP`] such files. Returns the new file's path.
///
/// # Errors
///
/// [`Error::Io`] when the directory cannot be written.
///
/// ```
/// let dir = std::env::temp_dir().join("mistarr-doc-mgl");
/// std::fs::create_dir_all(&dir).unwrap();
/// let path = mistarr_mister::launch::write_mgl(&dir, "<mistergamedescription/>").unwrap();
/// assert!(path.extension().is_some_and(|e| e == "mgl"));
/// ```
pub fn write_mgl(dir: &Path, doc: &str) -> Result<PathBuf> {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let (path, mut file) = loop {
        // A process-wide sequence keeps names unique and in creation order within a millisecond.
        let n = MGL_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!("{MGL_PREFIX}{millis:016}-{n:010}.mgl"));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => break (path, file),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    };
    file.write_all(doc.as_bytes())?;
    file.sync_all()?;
    prune_mgl(dir);
    Ok(path)
}

/// Removes launch MGL files beyond the newest [`MGL_KEEP`]; names sort by creation.
fn prune_mgl(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut names: Vec<String> = entries
        .filter_map(std::result::Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with(MGL_PREFIX) && has_extension(n, "mgl"))
        .collect();
    names.sort_unstable();
    let excess = names.len().saturating_sub(MGL_KEEP);
    for old in &names[..excess] {
        // A stale MGL that cannot be removed is harmless; the next launch tries again.
        let _ = std::fs::remove_file(dir.join(old));
    }
}

/// The `load_core` command line for an `.rbf`, `.mra` or `.mgl` at absolute `path`.
///
/// # Errors
///
/// [`Error::UnsafePath`] when `path` is not absolute, not UTF-8, holds a control
/// character or makes a line longer than one atomic FIFO write.
///
/// ```
/// let line = mistarr_mister::launch::load_core(std::path::Path::new("/media/fat/_Console/NES_20240101.rbf")).unwrap();
/// assert_eq!(line, "load_core /media/fat/_Console/NES_20240101.rbf\n");
/// ```
pub fn load_core(path: &Path) -> Result<String> {
    let line = path
        .to_str()
        .filter(|p| path.is_absolute() && !has_control(p))
        .map(|p| format!("load_core {p}\n"))
        .filter(|l| l.len() <= PIPE_BUF)
        .ok_or_else(|| Error::UnsafePath(path.display().to_string()))?;
    Ok(line)
}

/// Where command lines for MiSTer Main go.
pub trait CommandSink: Send + Sync {
    /// Whether the command interface exists; false on a machine that is not a MiSTer.
    fn present(&self) -> bool;
    /// Delivers one newline-terminated command line without blocking.
    ///
    /// # Errors
    ///
    /// [`Error::CommandAbsent`], [`Error::NotListening`] or [`Error::CommandBusy`]
    /// when Main cannot take the command, [`Error::Io`] for other failures.
    fn send(&self, line: &str) -> Result<()>;
}

/// The FIFO Main reads, opened write-only and non-blocking for each command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FifoSink {
    path: PathBuf,
}

impl FifoSink {
    /// A sink writing to the FIFO at `path`, normally [`COMMAND_PATH`].
    ///
    /// ```
    /// let sink = mistarr_mister::launch::FifoSink::new("/nonexistent/cmd");
    /// assert_eq!(sink.path(), std::path::Path::new("/nonexistent/cmd"));
    /// ```
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The FIFO's path.
    ///
    /// ```
    /// assert!(mistarr_mister::launch::FifoSink::new("/x").path().ends_with("x"));
    /// ```
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl CommandSink for FifoSink {
    fn present(&self) -> bool {
        std::fs::metadata(&self.path).is_ok_and(|m| m.file_type().is_fifo())
    }

    fn send(&self, line: &str) -> Result<()> {
        // Without a reader a non-blocking open fails with ENXIO instead of waiting.
        let flags = OFlags::WRONLY | OFlags::NONBLOCK | OFlags::CLOEXEC | OFlags::NOCTTY;
        let fd = rustix::fs::open(&self.path, flags, Mode::empty()).map_err(|e| match e {
            Errno::NOENT | Errno::NOTDIR => Error::CommandAbsent,
            Errno::NXIO => Error::NotListening,
            e => Error::Io(e.into()),
        })?;
        let mut file = File::from(fd);
        if !file.metadata()?.file_type().is_fifo() {
            return Err(Error::CommandAbsent);
        }
        match file.write(line.as_bytes()) {
            Ok(n) if n == line.len() => Ok(()),
            Ok(_) => Err(Error::CommandBusy),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Err(Error::CommandBusy),
            Err(e) => Err(Error::Io(e)),
        }
    }
}

/// What a [`RecordingSink`] does with the next command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FakeOutcome {
    /// Record the line.
    #[default]
    Accept,
    /// The interface is missing: [`CommandSink::present`] is false and sending fails.
    Absent,
    /// The FIFO exists but nothing reads it.
    NotListening,
}

/// A [`CommandSink`] that records lines in memory, for tests.
#[derive(Debug, Default)]
pub struct RecordingSink {
    lines: Mutex<Vec<String>>,
    outcome: Mutex<FakeOutcome>,
}

impl RecordingSink {
    /// A sink that accepts every command.
    ///
    /// ```
    /// use mistarr_mister::launch::{CommandSink, RecordingSink};
    /// let sink = RecordingSink::new();
    /// sink.send("load_core /x.rbf\n").unwrap();
    /// assert_eq!(sink.lines(), ["load_core /x.rbf\n"]);
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Changes what later commands meet.
    ///
    /// ```
    /// use mistarr_mister::launch::{CommandSink, FakeOutcome, RecordingSink};
    /// let sink = RecordingSink::new();
    /// sink.set_outcome(FakeOutcome::Absent);
    /// assert!(!sink.present());
    /// ```
    pub fn set_outcome(&self, outcome: FakeOutcome) {
        *self.outcome.lock().unwrap_or_else(PoisonError::into_inner) = outcome;
    }

    /// The lines recorded so far.
    ///
    /// ```
    /// assert!(mistarr_mister::launch::RecordingSink::new().lines().is_empty());
    /// ```
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        self.lines
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn outcome(&self) -> FakeOutcome {
        *self.outcome.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl CommandSink for RecordingSink {
    fn present(&self) -> bool {
        self.outcome() != FakeOutcome::Absent
    }

    fn send(&self, line: &str) -> Result<()> {
        match self.outcome() {
            FakeOutcome::Accept => {
                self.lines
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(line.to_owned());
                Ok(())
            }
            FakeOutcome::Absent => Err(Error::CommandAbsent),
            FakeOutcome::NotListening => Err(Error::NotListening),
        }
    }
}

#[cfg(test)]
mod tests;
