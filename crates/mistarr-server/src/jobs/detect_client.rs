//! The `detect_client` job; the order is in `docs/DOWNLOAD-CLIENTS.md` "Detection".

use std::time::Duration;

use async_trait::async_trait;
use mistarr_clients::detect::{self, DetectConfig};
use mistarr_clients::{ClientKind, DownloadClient, Transmission};
use serde::{Deserialize, Serialize};

use super::{Job, JobContext};
use crate::config::ClientConfig;
use crate::db::settings::{self, keys};
use crate::error::Result;
use crate::events::EventKind;

/// Time allowed for each probe.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// What detection found, stored under `client.detected` and shown in `/system/status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientStatus {
    /// The client in use, or `None` when nothing answered.
    pub kind: Option<ClientKind>,
    /// Its RPC URL or SCGI address.
    pub url: Option<String>,
    /// Whether it answered the last probe.
    pub reachable: bool,
    /// The version it reported, when the protocol reports one.
    pub version: Option<String>,
    /// Whether an `rtorrent` executable is on `PATH`, for the "Start rtorrent" offer.
    pub rtorrent_on_path: bool,
    /// Unix seconds of the probe.
    pub checked_at: i64,
}

/// Runs detection for `client` and probes what it finds.
pub async fn probe(client: &ClientConfig) -> ClientStatus {
    let url = Some(client.url.trim().to_owned()).filter(|u| !u.is_empty());
    let cfg = DetectConfig {
        kind: client.kind.kind(),
        url,
        timeout: PROBE_TIMEOUT,
        ..DetectConfig::default()
    };
    let found = detect::detect(&cfg).await;
    let (reachable, version) = match &found {
        Some(d) if d.kind == ClientKind::Transmission => {
            match Transmission::new(&d.url).map(|t| t.with_timeout(PROBE_TIMEOUT)) {
                Ok(t) => match t.probe().await {
                    Ok(info) => (true, Some(info.version)),
                    Err(_) => (false, None),
                },
                Err(_) => (false, None),
            }
        }
        Some(d) => (detect::probe_scgi(&d.url, PROBE_TIMEOUT).await, None),
        None => (false, None),
    };
    ClientStatus {
        kind: found.as_ref().map(|d| d.kind),
        url: found.map(|d| d.url),
        reachable,
        version,
        rtorrent_on_path: on_path("rtorrent", std::env::var_os("PATH").as_deref()),
        checked_at: crate::unix_now(),
    }
}

/// True if an executable file called `name` is in one of the `path` directories.
///
/// ```
/// use mistarr_server::jobs::detect_client::on_path;
/// assert!(!on_path("definitely-not-a-program", Some("/nonexistent".as_ref())));
/// ```
#[must_use]
pub fn on_path(name: &str, path: Option<&std::ffi::OsStr>) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.into_iter().flat_map(std::env::split_paths).any(|dir| {
        std::fs::metadata(dir.join(name))
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    })
}

/// Detects the download client, stores the result in settings and publishes `status`.
#[derive(Debug, Clone, Copy, Default)]
pub struct DetectClient;

/// The `jobs.kind` of [`DetectClient`].
pub const KIND: &str = "detect_client";

#[async_trait]
impl Job for DetectClient {
    fn kind(&self) -> &'static str {
        KIND
    }

    async fn run(&self, ctx: &JobContext) -> Result<()> {
        let client = ctx.app.config().client;
        let status = probe(&client).await;
        tracing::info!(
            kind = status.kind.map_or("none", ClientKind::as_str),
            reachable = status.reachable,
            "download client detection"
        );
        let stored = status.clone();
        ctx.app
            .db
            .write(move |c| settings::set_json(c, keys::CLIENT_DETECTED, &stored))
            .await?;
        ctx.app.refresh_client(&status);
        let snapshot = crate::status::snapshot(&ctx.app).await;
        ctx.app.events.publish(EventKind::Status, &snapshot);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::state;
    use crate::config::ClientChoice;
    use crate::jobs::Scheduler;
    use std::sync::Arc;

    fn closed_port() -> String {
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = l.local_addr().expect("addr");
        drop(l);
        format!("http://{addr}/transmission/rpc")
    }

    #[tokio::test]
    async fn configured_but_silent_client_is_unreachable() {
        let client = ClientConfig {
            kind: ClientChoice::Transmission,
            url: closed_port(),
            ..ClientConfig::default()
        };
        let st = probe(&client).await;
        assert_eq!(st.kind, Some(ClientKind::Transmission));
        assert_eq!(st.url.as_deref(), Some(client.url.as_str()));
        assert!(!st.reachable);
        assert!(st.version.is_none());
    }

    #[tokio::test]
    async fn job_stores_the_result() {
        let (_dir, app) = state();
        app.update_config(|c| {
            c.client.kind = ClientChoice::Rtorrent;
            c.client.url = "127.0.0.1:1".into();
        });
        Scheduler::run_inline(&app, Arc::new(DetectClient))
            .await
            .expect("run");
        let stored: Option<ClientStatus> = app
            .db
            .read(|c| settings::get_json(c, keys::CLIENT_DETECTED))
            .await
            .expect("read");
        let stored = stored.expect("stored");
        assert_eq!(stored.kind, Some(ClientKind::Rtorrent));
        assert!(!stored.reachable);
    }

    #[test]
    fn path_search_finds_executables_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let exe = dir.path().join("tool");
        std::fs::write(&exe, b"").expect("write");
        let path = dir.path().as_os_str();
        assert!(!on_path("tool", Some(path)));
        let mode = <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755);
        std::fs::set_permissions(&exe, mode).expect("chmod");
        assert!(on_path("tool", Some(path)));
        assert!(!on_path("tool", None));
    }
}
