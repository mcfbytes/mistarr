//! The `detect_client` job and the re-detection loop; the order is in
//! `docs/DOWNLOAD-CLIENTS.md` "Detection".

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mistarr_clients::detect::{self, DetectConfig};
use mistarr_clients::launch::Launcher;
use mistarr_clients::{ClientKind, DownloadClient, Transmission};
use serde::{Deserialize, Serialize};

use super::{wizard, Job, JobContext};
use crate::app::AppState;
use crate::config::ClientConfig;
use crate::db::settings::{self, keys};
use crate::error::Result;
use crate::events::EventKind;

/// Time allowed for each probe.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// What detection found, stored under `client.detected` and shown in `/system/status`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)] // One flag per offer the UI makes.
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
    /// Whether `transmission-daemon` is on `PATH`, for the "Start Transmission" offer.
    #[serde(default)]
    pub transmission_on_path: bool,
    /// Whether the `Buildroot_MiSTer` init script for Transmission exists.
    #[serde(default)]
    pub transmission_service: bool,
    /// Whether its opt-in directory exists, so the image starts it at boot.
    #[serde(default)]
    pub transmission_opt_in: bool,
    /// Unix seconds of the probe.
    pub checked_at: i64,
}

impl ClientStatus {
    /// True when a client was found and answered.
    ///
    /// ```
    /// let st = mistarr_server::jobs::detect_client::ClientStatus::default();
    /// assert!(!st.usable());
    /// ```
    #[must_use]
    pub fn usable(&self) -> bool {
        self.kind.is_some() && self.reachable
    }
}

/// Runs detection for `client`, probes what it finds and notes which
/// clients `launcher` sees installed.
pub async fn probe(client: &ClientConfig, launcher: &Launcher) -> ClientStatus {
    let url = Some(client.url.trim().to_owned()).filter(|u| !u.is_empty());
    let cfg = DetectConfig {
        kind: client.kind.kind(),
        url,
        timeout: PROBE_TIMEOUT,
        rtorrent_socket: launcher.data_dir.join("rtorrent.sock"),
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
    let launcher = launcher.clone();
    let installed =
        crate::threads::blocking(crate::threads::label::DETECT, move || launcher.installed())
            .await
            .unwrap_or_default();
    ClientStatus {
        kind: found.as_ref().map(|d| d.kind),
        url: found.map(|d| d.url),
        reachable,
        version,
        rtorrent_on_path: installed.rtorrent_on_path,
        transmission_on_path: installed.transmission_on_path,
        transmission_service: installed.transmission_service,
        transmission_opt_in: installed.transmission_opt_in,
        checked_at: crate::unix_now(),
    }
}

/// Detects the client, stores the result, points the app at it, publishes
/// `status` when anything but the probe time changed (always with `announce`),
/// and checks the wizard. Runs one at a time; a result whose probe began
/// before the stored one was taken is dropped and the stored one returned.
///
/// # Errors
///
/// [`crate::Error::Db`] when the result cannot be stored.
pub async fn detect_and_store(app: &Arc<AppState>, announce: bool) -> Result<ClientStatus> {
    let _one = app.detect_lock.lock().await;
    let began = crate::unix_now();
    let client = app.config().client;
    let status = probe(&client, &app.launcher()).await;
    let stored = status.clone();
    let before = app
        .db
        .write(move |c| {
            let before = settings::get_json::<ClientStatus>(c, keys::CLIENT_DETECTED)
                .ok()
                .flatten();
            if before.as_ref().is_some_and(|b| b.checked_at > began) {
                return Ok(Err(before));
            }
            settings::set_json(c, keys::CLIENT_DETECTED, &stored)?;
            Ok(Ok(before))
        })
        .await?;
    let before = match before {
        Ok(before) => before,
        Err(newer) => return Ok(newer.unwrap_or(status)),
    };
    let changed = before.is_none_or(|b| {
        ClientStatus { checked_at: 0, ..b }
            != ClientStatus {
                checked_at: 0,
                ..status.clone()
            }
    });
    if changed {
        tracing::info!(
            kind = status.kind.map_or("none", ClientKind::as_str),
            reachable = status.reachable,
            "download client detection"
        );
    }
    app.refresh_client(&status);
    if changed || announce {
        let snapshot = crate::status::snapshot(app).await;
        app.events.publish(EventKind::Status, &snapshot);
    }
    if let Err(e) = wizard::on_change(app).await {
        tracing::warn!(error = %e, "cannot check wizard completion");
    }
    Ok(status)
}

/// Re-runs detection every [`crate::app::Options::redetect_poll`] while no
/// client answers, and whenever [`AppState::redetect`] is notified, so a
/// client started after mistarr is picked up without the wizard.
pub async fn watch(app: Arc<AppState>) {
    let mut stop = app.shutdown_signal();
    loop {
        let woken = tokio::select! {
            () = tokio::time::sleep(app.options.redetect_poll) => false,
            () = app.redetect.notified() => true,
            _ = stop.wait_for(|s| *s) => return,
        };
        let usable = app
            .db
            .read(|c| settings::get_json::<ClientStatus>(c, keys::CLIENT_DETECTED))
            .await
            .ok()
            .flatten()
            .is_some_and(|s| s.usable());
        if woken || !usable {
            if let Err(e) = detect_and_store(&app, false).await {
                tracing::warn!(error = %e, "cannot store client detection");
            }
        }
    }
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
        detect_and_store(&ctx.app, true).await.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::state;
    use crate::config::ClientChoice;
    use crate::jobs::Scheduler;

    fn closed_port() -> String {
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = l.local_addr().expect("addr");
        drop(l);
        format!("http://{addr}/transmission/rpc")
    }

    fn nowhere() -> Launcher {
        let mut l = Launcher::board("/nonexistent".as_ref());
        l.search_path = Some("/nonexistent".into());
        l.transmission_init = "/nonexistent/init".into();
        l.transmission_opt_in = "/nonexistent/opt".into();
        l
    }

    #[tokio::test]
    async fn configured_but_silent_client_is_unreachable() {
        let client = ClientConfig {
            kind: ClientChoice::Transmission,
            url: closed_port(),
            ..ClientConfig::default()
        };
        let st = probe(&client, &nowhere()).await;
        assert_eq!(st.kind, Some(ClientKind::Transmission));
        assert_eq!(st.url.as_deref(), Some(client.url.as_str()));
        assert!(!st.reachable && !st.usable());
        assert!(st.version.is_none());
        assert!(!st.transmission_on_path && !st.transmission_service);
    }

    #[tokio::test]
    async fn installed_clients_are_reported() {
        let dir = tempfile::tempdir().expect("tempdir");
        let exe = dir.path().join("transmission-daemon");
        std::fs::write(&exe, "#!/bin/sh\n").expect("write");
        let mode = <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755);
        std::fs::set_permissions(&exe, mode).expect("chmod");
        std::fs::write(dir.path().join("init"), "").expect("write");
        let launcher = Launcher {
            search_path: Some(dir.path().as_os_str().to_owned()),
            transmission_init: dir.path().join("init"),
            ..nowhere()
        };
        let client = ClientConfig {
            kind: ClientChoice::Rtorrent,
            url: "127.0.0.1:1".into(),
            ..ClientConfig::default()
        };
        let st = probe(&client, &launcher).await;
        assert!(st.transmission_on_path && st.transmission_service);
        assert!(!st.transmission_opt_in && !st.rtorrent_on_path);
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

    #[tokio::test]
    async fn the_loop_redetects_until_a_client_answers() {
        let (_dir, app) = state();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr").to_string();
        drop(listener);
        app.update_config(|c| {
            c.client.kind = ClientChoice::Rtorrent;
            c.client.url = addr.clone();
        });
        detect_and_store(&app, false).await.expect("detect");
        let task = tokio::spawn(watch(Arc::clone(&app)));
        let _listener = tokio::net::TcpListener::bind(&addr).await.expect("rebind");
        app.redetect.notify_one();
        for _ in 0..200 {
            let st: Option<ClientStatus> = app
                .db
                .read(|c| settings::get_json(c, keys::CLIENT_DETECTED))
                .await
                .expect("read");
            if st.is_some_and(|s| s.reachable) {
                task.abort();
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the started client was never picked up");
    }

    #[tokio::test]
    async fn a_probe_older_than_the_stored_result_is_dropped() {
        let (_dir, app) = state();
        app.update_config(|c| {
            c.client.kind = ClientChoice::Rtorrent;
            c.client.url = "127.0.0.1:1".into();
        });
        let newer = ClientStatus {
            kind: Some(ClientKind::Transmission),
            checked_at: crate::unix_now() + 100,
            ..ClientStatus::default()
        };
        let stored = newer.clone();
        app.db
            .write(move |c| settings::set_json(c, keys::CLIENT_DETECTED, &stored))
            .await
            .expect("store");
        let got = detect_and_store(&app, false).await.expect("detect");
        assert_eq!(got, newer);
        let kept: Option<ClientStatus> = app
            .db
            .read(|c| settings::get_json(c, keys::CLIENT_DETECTED))
            .await
            .expect("read");
        assert_eq!(kept, Some(newer));
    }

    #[test]
    fn stored_status_without_the_install_flags_still_reads() {
        let old = r#"{"kind":null,"url":null,"reachable":false,"version":null,"rtorrent_on_path":true,"checked_at":1}"#;
        let st: ClientStatus = serde_json::from_str(old).expect("json");
        assert!(st.rtorrent_on_path && !st.transmission_on_path);
    }
}
