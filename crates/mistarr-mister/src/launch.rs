//! Starting cores and games through MiSTer Main's command FIFO; see
//! `docs/PLATFORMS.md` "Launch parameters" and `docs/ARCHITECTURE.md` "Launching".

use std::fmt::Write as _;
use std::fs::File;
use std::io::Write as _;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use rustix::fs::{Mode, OFlags};
use rustix::io::Errno;

use crate::corename::{rbf_files, RbfFile};
use crate::platforms::{for_core, Kind, Platform};
use crate::{Error, Result};

/// The FIFO MiSTer Main reads commands from.
pub const COMMAND_PATH: &str = "/dev/MiSTer_cmd";

/// File name of the MGL written for each game launch, overwritten every time.
pub const MGL_FILE: &str = "mistarr.mgl";

/// Longest command line written in one call; POSIX makes writes up to `PIPE_BUF` atomic.
const PIPE_BUF: usize = 4096;

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

/// The per-platform `<file>` parameters of an MGL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaunchSlot {
    /// Load or mount.
    pub mode: LoadMode,
    /// The core's file-slot index.
    pub index: u8,
    /// Seconds Main waits after loading the core before handing it the file.
    pub delay: u8,
    /// The values must be confirmed against a live board.
    pub verify_on_board: bool,
}

/// An installed core chosen to launch a platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreFile {
    /// Absolute path of the `.rbf`.
    pub path: PathBuf,
    /// The MGL `<rbf>` value: the path relative to the SD root without date or extension.
    pub mgl_rbf: String,
}

/// The newest installed core that loads `platform`, by the `_YYYYMMDD` date in its
/// name; undated files rank below dated ones and ties go to the shorter path.
/// Cores under `_Arcade` are never chosen, since arcade games start from their MRA.
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
    let best = rbf_files(root)
        .into_iter()
        .filter(|f| !f.arcade && for_core(&f.name).iter().any(|p| p.id == platform.id))
        .max_by(|a, b| {
            a.date
                .cmp(&b.date)
                .then_with(|| b.path.as_os_str().len().cmp(&a.path.as_os_str().len()))
                .then_with(|| b.path.cmp(&a.path))
        })?;
    core_file(root, best)
}

fn core_file(root: &Path, rbf: RbfFile) -> Option<CoreFile> {
    let dir = rbf.path.parent()?.strip_prefix(root).ok()?;
    let mut mgl_rbf = String::new();
    for part in dir.components() {
        mgl_rbf.push_str(part.as_os_str().to_str()?);
        mgl_rbf.push('/');
    }
    mgl_rbf.push_str(&rbf.name);
    Some(CoreFile {
        path: rbf.path,
        mgl_rbf,
    })
}

/// The path, relative to `games/`, that loads a title from its files' `rel_path`s:
/// the cue sheet or single image of a disc, the zip or directory of a romset, the
/// file of a cartridge. A zip member `a.zip#b.nes` becomes `a.zip/b.nes`, which
/// Main opens inside the zip.
///
/// ```
/// use mistarr_mister::launch::game_path;
/// use mistarr_mister::platforms::Kind;
/// assert_eq!(game_path(Kind::Cartridge, &["NES/a.zip#b.nes"]).as_deref(), Some("NES/a.zip/b.nes"));
/// assert_eq!(game_path(Kind::Disc, &["PSX/G/g (Track 1).bin", "PSX/G/g.cue"]).as_deref(), Some("PSX/G/g.cue"));
/// ```
#[must_use]
pub fn game_path(kind: Kind, rel_paths: &[&str]) -> Option<String> {
    match kind {
        Kind::Disc => ["cue", "chd", "iso"].iter().find_map(|ext| {
            rel_paths
                .iter()
                .find(|p| has_extension(p, ext))
                .map(|p| p.replace('#', "/"))
        }),
        Kind::Romset => rel_paths.first().map(|p| romset_container(p)),
        _ => rel_paths.first().map(|p| p.replace('#', "/")),
    }
}

fn has_extension(path: &str, ext: &str) -> bool {
    path.rsplit_once('.')
        .is_some_and(|(_, e)| e.eq_ignore_ascii_case(ext))
}

/// `NeoGeo/set.zip#member` to `NeoGeo/set.zip`, `NeoGeo/set/member` to `NeoGeo/set`.
fn romset_container(rel_path: &str) -> String {
    if let Some((zip, _)) = rel_path.split_once('#') {
        return zip.to_owned();
    }
    let mut parts = rel_path.splitn(3, '/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(top), Some(set), Some(_)) => format!("{top}/{set}"),
        _ => rel_path.to_owned(),
    }
}

/// Builds an MGL document that starts core `rbf` (see [`CoreFile::mgl_rbf`]) and
/// hands it the absolute path `game` with the platform's `slot` parameters.
///
/// # Errors
///
/// [`Error::UnsafePath`] when `game` is not absolute, not UTF-8 or holds a control
/// character, or `rbf` holds a control character; XML cannot carry those safely.
///
/// ```
/// use mistarr_mister::launch::{mgl, LaunchSlot, LoadMode};
/// let slot = LaunchSlot { mode: LoadMode::File, index: 0, delay: 2, verify_on_board: true };
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

/// Writes `doc` to [`MGL_FILE`] in `dir` through a temporary file and a rename,
/// so Main never reads a half-written document. Returns the file's path.
///
/// # Errors
///
/// [`Error::Io`] when the directory cannot be written.
///
/// ```
/// let dir = std::env::temp_dir().join("mistarr-doc-mgl");
/// std::fs::create_dir_all(&dir).unwrap();
/// let path = mistarr_mister::launch::write_mgl(&dir, "<mistergamedescription/>").unwrap();
/// assert!(path.ends_with("mistarr.mgl"));
/// ```
pub fn write_mgl(dir: &Path, doc: &str) -> Result<PathBuf> {
    let path = dir.join(MGL_FILE);
    let tmp = dir.join(format!("{MGL_FILE}.tmp"));
    std::fs::write(&tmp, doc)?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
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
