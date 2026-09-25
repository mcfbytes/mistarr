//! The `/system/status` body and the host measurements it reports.

use std::path::Path;

use serde::Serialize;

use crate::app::AppState;
use crate::db::jobs::{self, JobId, JobRow, JobState};
use crate::db::settings::{self, keys};
use crate::db::system::wizard_counts;
use crate::error::Result;
use crate::jobs::core_limits::ClientHold;
use crate::jobs::detect_client::ClientStatus;
use crate::jobs::gate::{GateState, Override, PauseReason};
use crate::jobs::Lane;

/// `GET /system/status` and the `status` event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Status {
    /// The version string from [`crate::version::version`].
    pub version: &'static str,
    /// The short commit the binary was built from, when the build knew it.
    pub commit: Option<&'static str>,
    /// Whether the binary was built from a release tag.
    pub release: bool,
    /// Seconds since the server started.
    pub uptime_secs: u64,
    /// Last client detection, `None` before the first.
    pub client: Option<ClientStatus>,
    /// Last CORENAME value, `None` when the file does not exist.
    pub corename: Option<String>,
    /// Whether heavy jobs are held.
    pub paused: bool,
    /// Why they are held.
    pub pause_reason: Option<PauseReason>,
    /// The manual override in force.
    #[serde(rename = "override")]
    pub manual_override: Option<Override>,
    /// How the download client is held while a core runs, if it is.
    pub client_hold: Option<ClientHold>,
    /// `transfer.pause_client_while_playing`.
    pub pause_client_while_playing: bool,
    /// Queued and paused jobs on held lanes, heavy first, oldest first;
    /// empty while no lane is held.
    pub waiting: Vec<WaitingJob>,
    /// Free bytes on the filesystem holding the data directory.
    pub disk_free_bytes: Option<u64>,
    /// Size in bytes of the filesystem holding the data directory.
    pub disk_total_bytes: Option<u64>,
    /// The directory watched for DAT files, as configured.
    pub dats_dir: String,
    /// Resident set size of this process.
    pub rss_bytes: Option<u64>,
    /// `MemTotal` from `/proc/meminfo`.
    pub mem_total_bytes: Option<u64>,
    /// `MemAvailable` from `/proc/meminfo`.
    pub mem_available_bytes: Option<u64>,
    /// Whether cores and games can be launched.
    pub launch: LaunchState,
    /// CHD decoding speed measured on the last image, `None` before the first.
    pub chd_decode_bytes_per_sec: Option<u64>,
}

/// Whether the launch routes can start anything, as `/system/status` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LaunchState {
    /// Launching is allowed and MiSTer Main's command interface exists.
    Ready,
    /// `prefs.launch` is off.
    Disabled,
    /// No command interface, as on a machine that is not a MiSTer.
    Unavailable,
}

/// The launch state from `prefs.launch` and the command sink.
pub fn launch_state(app: &AppState) -> LaunchState {
    if !app.config().prefs.launch {
        LaunchState::Disabled
    } else if app.command_sink().present() {
        LaunchState::Ready
    } else {
        LaunchState::Unavailable
    }
}

/// One heavy job held by the gate, as `/system/status` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WaitingJob {
    /// The job's row.
    pub id: JobId,
    /// Its kind.
    pub kind: String,
    /// `queued`, or `paused` when it stopped at a checkpoint.
    pub state: JobState,
    /// The file name or platform the job is about, when its payload names one.
    pub detail: Option<String>,
}

impl WaitingJob {
    /// The summary of a job row.
    ///
    /// ```
    /// use mistarr_server::db::jobs::{JobId, JobRow, JobState};
    /// let row = JobRow { id: JobId(1), kind: "scan".into(), lane: "heavy".into(),
    ///     payload: serde_json::json!({"platform_id": "nes"}), state: JobState::Queued,
    ///     progress: None, created_at: 0, updated_at: 0 };
    /// let w = mistarr_server::status::WaitingJob::from_row(&row);
    /// assert_eq!(w.detail.as_deref(), Some("nes"));
    /// ```
    #[must_use]
    pub fn from_row(row: &JobRow) -> Self {
        Self {
            id: row.id,
            kind: row.kind.clone(),
            state: row.state,
            detail: job_detail(&row.payload),
        }
    }
}

/// The file name in a payload's `path`, else its `source_name`, else its `platform_id`.
///
/// ```
/// let p = serde_json::json!({"path": "/data/dats/a.dat"});
/// assert_eq!(mistarr_server::status::job_detail(&p).as_deref(), Some("a.dat"));
/// let s = serde_json::json!({"source_name": "Set", "platform_id": "nes"});
/// assert_eq!(mistarr_server::status::job_detail(&s).as_deref(), Some("Set"));
/// assert_eq!(mistarr_server::status::job_detail(&serde_json::json!({})), None);
/// ```
#[must_use]
pub fn job_detail(payload: &serde_json::Value) -> Option<String> {
    if let Some(path) = payload.get("path").and_then(serde_json::Value::as_str) {
        let name = Path::new(path).file_name()?;
        return Some(name.to_string_lossy().into_owned());
    }
    if let Some(name) = payload
        .get("source_name")
        .and_then(serde_json::Value::as_str)
    {
        return Some(name.to_owned());
    }
    payload
        .get("platform_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

/// Why a queued or paused job on `lane` is not running, from the gate; `None`
/// while its lane is not held. See [`GateState::hold`].
///
/// ```
/// use mistarr_server::db::jobs::JobState;
/// use mistarr_server::jobs::gate::GateState;
/// let gate = GateState { corename: Some("SNES".into()), manual: None };
/// let why = mistarr_server::status::hold_reason(&gate, "heavy", JobState::Queued);
/// assert_eq!(why.as_deref(), Some("Paused while SNES is running"));
/// assert!(mistarr_server::status::hold_reason(&gate, "background", JobState::Queued).is_none());
/// ```
#[must_use]
pub fn hold_reason(gate: &GateState, lane: &str, state: JobState) -> Option<String> {
    if !matches!(state, JobState::Queued | JobState::Paused) {
        return None;
    }
    let lane = [Lane::Heavy, Lane::Background, Lane::Light]
        .into_iter()
        .find(|l| l.as_str() == lane)?;
    match gate.hold(lane)? {
        PauseReason::Core => Some(format!(
            "Paused while {} is running",
            gate.corename.as_deref().unwrap_or("a core")
        )),
        PauseReason::Manual => Some("Paused by the user".to_owned()),
    }
}

/// Builds the status body from the gate, settings and the host.
pub async fn snapshot(app: &AppState) -> Status {
    let client = app
        .db
        .read(|c| settings::get_json::<ClientStatus>(c, keys::CLIENT_DETECTED))
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "cannot read client status");
            None
        });
    let gate = app.gate.state();
    let mut waiting = Vec::new();
    for lane in [Lane::Heavy, Lane::Background] {
        if gate.hold(lane).is_none() {
            continue;
        }
        let rows = app
            .db
            .read(move |c| jobs::open_in_lane(c, lane.as_str()))
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "cannot list waiting jobs");
                Vec::new()
            });
        waiting.extend(
            rows.iter()
                .filter(|r| r.state != JobState::Running)
                .map(WaitingJob::from_row),
        );
    }
    let data = app.config().paths.data;
    let chd_rate = app
        .db
        .read(|c| settings::get_json::<u64>(c, keys::CHD_RATE))
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "cannot read the CHD decoding speed");
            None
        });
    let mem = meminfo();
    let disk = disk_space(&data);
    Status {
        version: crate::version::version(),
        commit: crate::version::commit(),
        release: crate::version::is_release(),
        uptime_secs: app.started.elapsed().as_secs(),
        client,
        pause_reason: gate.pause_reason(),
        paused: gate.paused(),
        manual_override: gate.manual,
        client_hold: app.client_hold(),
        pause_client_while_playing: app.config().transfer.pause_client_while_playing,
        waiting,
        corename: gate.corename,
        disk_free_bytes: disk.free,
        disk_total_bytes: disk.total,
        dats_dir: app.config().paths.dats().to_string_lossy().into_owned(),
        rss_bytes: rss_bytes(),
        mem_total_bytes: mem.total,
        mem_available_bytes: mem.available,
        launch: launch_state(app),
        chd_decode_bytes_per_sec: chd_rate,
    }
}

/// Which first-run steps are complete, per `docs/API.md` "System".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(clippy::struct_excessive_bools)] // One flag per wizard step is the JSON shape.
pub struct WizardStatus {
    /// The games directory exists.
    pub paths: bool,
    /// Any DAT version was ever loaded.
    pub dats: bool,
    /// Client detection found one.
    pub client: bool,
    /// Any source exists.
    pub sources: bool,
}

impl WizardStatus {
    /// Every step reports done.
    #[must_use]
    pub fn complete(self) -> bool {
        self.paths && self.dats && self.client && self.sources
    }
}

/// Builds the wizard status from the config, `dat_versions`, `sources` and
/// the last client detection.
///
/// # Errors
///
/// [`crate::Error::Db`] or [`crate::Error::Stored`] when settings cannot be read.
pub async fn wizard_status(app: &AppState) -> Result<WizardStatus> {
    let counts = app.db.read(wizard_counts).await?;
    let client = app
        .db
        .read(|c| settings::get_json::<ClientStatus>(c, keys::CLIENT_DETECTED))
        .await?;
    Ok(WizardStatus {
        paths: app.config().paths.games.is_dir(),
        dats: counts.dat_versions > 0,
        client: client.is_some_and(|c| c.kind.is_some()),
        sources: counts.sources > 0,
    })
}

/// Bytes available to unprivileged users on the filesystem holding `path`.
///
/// ```
/// assert!(mistarr_server::status::free_bytes(std::path::Path::new("/")).is_some());
/// ```
#[must_use]
pub fn free_bytes(path: &Path) -> Option<u64> {
    disk_space(path).free
}

/// Free and total bytes of a filesystem, from one `statvfs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DiskSpace {
    /// Bytes available to unprivileged users.
    pub free: Option<u64>,
    /// Size of the filesystem.
    pub total: Option<u64>,
}

/// Reads [`DiskSpace`] for the filesystem holding `path`; both `None` when it cannot.
///
/// ```
/// let s = mistarr_server::status::disk_space(std::path::Path::new("/"));
/// assert!(s.total.is_some_and(|t| s.free.is_some_and(|f| f <= t)));
/// ```
#[must_use]
pub fn disk_space(path: &Path) -> DiskSpace {
    match rustix::fs::statvfs(path) {
        Ok(st) => DiskSpace {
            free: st.f_bavail.checked_mul(st.f_frsize),
            total: st.f_blocks.checked_mul(st.f_frsize),
        },
        Err(_) => DiskSpace::default(),
    }
}

/// Total and available memory, from one read of `/proc/meminfo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MemInfo {
    /// `MemTotal`.
    pub total: Option<u64>,
    /// `MemAvailable`.
    pub available: Option<u64>,
}

/// Reads [`MemInfo`]; both fields are `None` when `/proc/meminfo` cannot be read.
///
/// ```
/// let m = mistarr_server::status::meminfo();
/// assert!(m.total.is_some_and(|t| m.available.is_some_and(|a| a <= t)));
/// ```
#[must_use]
pub fn meminfo() -> MemInfo {
    std::fs::read_to_string("/proc/meminfo")
        .map(|text| meminfo_from(&text))
        .unwrap_or_default()
}

fn meminfo_from(text: &str) -> MemInfo {
    MemInfo {
        total: kib_field(text, "MemTotal:"),
        available: kib_field(text, "MemAvailable:"),
    }
}

/// This process's resident set size from `/proc/self/status`.
///
/// ```
/// assert!(mistarr_server::status::rss_bytes().is_some_and(|b| b > 0));
/// ```
#[must_use]
pub fn rss_bytes() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/self/status").ok()?;
    kib_field(&text, "VmRSS:")
}

/// `MemAvailable` from `/proc/meminfo`.
///
/// ```
/// assert!(mistarr_server::status::mem_available_bytes().is_some());
/// ```
#[must_use]
pub fn mem_available_bytes() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    kib_field(&text, "MemAvailable:")
}

/// Parses a `Name:   123 kB` line into bytes.
fn kib_field(text: &str, name: &str) -> Option<u64> {
    let line = text.lines().find_map(|l| l.strip_prefix(name))?;
    let kib: u64 = line.split_whitespace().next()?.parse().ok()?;
    kib.checked_mul(1024)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::state;

    #[test]
    fn kib_fields_parse() {
        let text = "VmPeak:\t  10 kB\nVmRSS:\t    2048 kB\n";
        assert_eq!(kib_field(text, "VmRSS:"), Some(2 * 1024 * 1024));
        assert_eq!(kib_field(text, "Missing:"), None);
        assert_eq!(kib_field("VmRSS: x kB", "VmRSS:"), None);
    }

    #[test]
    fn meminfo_parses_total_and_available() {
        let text = "MemTotal:  512000 kB\nMemFree: 1 kB\nMemAvailable:  128000 kB\n";
        let m = meminfo_from(text);
        assert_eq!(m.total, Some(512_000 * 1024));
        assert_eq!(m.available, Some(128_000 * 1024));
        assert_eq!(meminfo_from(""), MemInfo::default());
    }

    #[test]
    fn host_measurements_exist_on_linux() {
        assert!(meminfo().total.is_some());
        assert!(disk_space(Path::new("/")).total.is_some());
        assert_eq!(
            disk_space(Path::new("/nonexistent/x")),
            DiskSpace::default()
        );
        assert!(rss_bytes().is_some());
        assert!(mem_available_bytes().is_some());
        assert!(free_bytes(Path::new("/")).is_some());
        assert!(free_bytes(Path::new("/nonexistent/x")).is_none());
    }

    #[tokio::test]
    async fn snapshot_reflects_the_gate() {
        let (_dir, app) = state();
        app.gate.set_corename(Some("SNES".into()));
        let s = snapshot(&app).await;
        assert!(s.paused);
        assert_eq!(s.pause_reason, Some(PauseReason::Core));
        assert_eq!(s.corename.as_deref(), Some("SNES"));
        assert!(s.client.is_none());
        let json = serde_json::to_value(&s).expect("json");
        assert!(json.get("override").is_some());
        assert_eq!(json["launch"], "unavailable");
        assert_eq!(json["version"], crate::version::version());
        assert!(json["mem_total_bytes"].is_u64());
        assert!(json["disk_total_bytes"].is_u64());
    }

    #[tokio::test]
    async fn launch_state_follows_prefs_and_sink() {
        let (_dir, app) = state();
        assert_eq!(launch_state(&app), LaunchState::Unavailable);
        app.set_command_sink(std::sync::Arc::new(
            mistarr_mister::launch::RecordingSink::new(),
        ));
        assert_eq!(launch_state(&app), LaunchState::Ready);
        app.update_config(|c| c.prefs.launch = false);
        assert_eq!(launch_state(&app), LaunchState::Disabled);
    }
}
