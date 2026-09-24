//! The `/system/status` body and the host measurements it reports.

use std::path::Path;

use serde::Serialize;

use crate::app::AppState;
use crate::db::settings::{self, keys};
use crate::db::system::wizard_counts;
use crate::error::Result;
use crate::jobs::detect_client::ClientStatus;
use crate::jobs::gate::{Override, PauseReason};

/// `GET /system/status` and the `status` event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Status {
    /// Crate version.
    pub version: &'static str,
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
    /// Free bytes on the filesystem holding the data directory.
    pub disk_free_bytes: Option<u64>,
    /// Resident set size of this process.
    pub rss_bytes: Option<u64>,
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
    let data = app.config().paths.data;
    Status {
        version: env!("CARGO_PKG_VERSION"),
        uptime_secs: app.started.elapsed().as_secs(),
        client,
        pause_reason: gate.pause_reason(),
        paused: gate.paused(),
        manual_override: gate.manual,
        corename: gate.corename,
        disk_free_bytes: free_bytes(&data),
        rss_bytes: rss_bytes(),
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
    let st = rustix::fs::statvfs(path).ok()?;
    st.f_bavail.checked_mul(st.f_frsize)
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
    fn host_measurements_exist_on_linux() {
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
    }
}
