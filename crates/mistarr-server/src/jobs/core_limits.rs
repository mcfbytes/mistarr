//! Rate limits and held uploads that follow the core gate; see `docs/DOWNLOAD-CLIENTS.md`
//! "Core gate".

use std::fmt::Display;
use std::sync::Arc;

use mistarr_clients::{ClientKind, DownloadClient, UploadLimit};
use serde::{Deserialize, Serialize};

use crate::app::AppState;
use crate::config::LimitsConfig;
use crate::db::settings::{self, keys};
use crate::events::EventKind;

/// The upload limit a client had before mistarr held its uploads, stored under
/// [`keys::UPLOADS_PAUSED`] until it is put back, so a restart can restore it.
///
/// ```
/// use mistarr_clients::{ClientKind, UploadLimit};
/// use mistarr_server::jobs::core_limits::Marker;
/// let m = Marker { kind: ClientKind::Rtorrent, limit: UploadLimit::default() };
/// assert_eq!(serde_json::to_string(&m)?, r#"{"kind":"rtorrent","limit":{"enabled":false,"kbps":0}}"#);
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    /// The client the limit belongs to.
    pub kind: ClientKind,
    /// Its upload limit before the hold.
    pub limit: UploadLimit,
}

/// The `(down, up)` limits in KiB/s for the menu or a running core; 0 is unlimited.
///
/// ```
/// use mistarr_server::config::LimitsConfig;
/// use mistarr_server::jobs::core_limits::limits_for;
/// assert_eq!(limits_for(&LimitsConfig::default(), true), (512, 64));
/// assert_eq!(limits_for(&LimitsConfig::default(), false), (0, 0));
/// ```
#[must_use]
pub fn limits_for(limits: &LimitsConfig, core: bool) -> (u32, u32) {
    if core {
        (limits.down_kbps_core, limits.up_kbps_core)
    } else {
        (limits.down_kbps_menu, limits.up_kbps_menu)
    }
}

/// What this process has had the client take.
struct Applied {
    /// Whether the core limits, rather than the menu ones, were sent last.
    core: bool,
    /// Whether uploads are held.
    held: bool,
    /// The limit to put back, while one is recorded.
    marker: Option<Marker>,
}

impl Applied {
    fn settled(&self, core: bool, hold: bool) -> bool {
        self.core == core && self.held == hold && (hold || self.marker.is_none())
    }
}

/// Applies the core limits each time CORENAME leaves `MENU` and the menu
/// limits each time it returns, once per transition. With
/// `transfer.pause_uploads_while_playing`, uploads are also held while the
/// core runs and the recorded upload limit is put back at the menu, or at
/// start when a previous run left them held. Whatever the client does not
/// take is retried every [`crate::app::Options::corename_poll`].
pub async fn follow_gate(app: Arc<AppState>) {
    let mut rx = app.gate.subscribe();
    let marker = app
        .db
        .read(|c| settings::get_json::<Marker>(c, keys::UPLOADS_PAUSED))
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "cannot read the held upload limit");
            None
        });
    let mut applied = Applied {
        core: false,
        held: false,
        marker,
    };
    loop {
        let core = rx.borrow_and_update().core_running();
        let hold = core && app.config().transfer.pause_uploads_while_playing;
        let settled = applied.settled(core, hold) || step(&app, &mut applied, core, hold).await;
        let changed = tokio::select! {
            r = rx.changed() => r.is_ok(),
            () = app.limits_wake.notified() => true,
            () = tokio::time::sleep(app.options.corename_poll), if !settled => true,
        };
        if !changed {
            return;
        }
    }
}

/// Moves the client towards `core` and `hold`; true once it took every call.
async fn step(app: &AppState, applied: &mut Applied, core: bool, hold: bool) -> bool {
    let Some((kind, client)) = app.client_with_kind() else {
        tracing::debug!("no download client for rate limits");
        return false;
    };
    let done = if hold {
        hold_uploads(app, applied, kind, client.as_ref(), core).await
    } else {
        release_uploads(app, applied, kind, client.as_ref(), core).await
    };
    done.is_some()
}

async fn hold_uploads(
    app: &AppState,
    applied: &mut Applied,
    kind: ClientKind,
    client: &dyn DownloadClient,
    core: bool,
) -> Option<()> {
    if applied.marker.is_none_or(|m| m.kind != kind) {
        let limit = logged(client.upload_limit().await, "cannot read the upload limit")?;
        let marker = Marker { kind, limit };
        let stored = app
            .db
            .write(move |c| settings::set_json(c, keys::UPLOADS_PAUSED, &marker))
            .await;
        logged(stored, "cannot record the upload limit")?;
        applied.marker = Some(marker);
    }
    if applied.core != core {
        send_limits(app, client, core).await?;
        applied.core = core;
    }
    if !applied.held {
        logged(client.pause_uploads().await, "cannot pause uploads")?;
        applied.held = true;
        tracing::info!("uploads paused while a core runs");
        publish(app, true).await;
    }
    Some(())
}

async fn release_uploads(
    app: &AppState,
    applied: &mut Applied,
    kind: ClientKind,
    client: &dyn DownloadClient,
    core: bool,
) -> Option<()> {
    if let Some(marker) = applied.marker {
        if marker.kind == kind {
            let restored = client.set_upload_limit(marker.limit).await;
            logged(restored, "cannot restore the upload limit")?;
            tracing::info!("upload limit restored");
        } else {
            tracing::warn!(held = %marker.kind, now = %kind, "the held client is gone");
        }
        let removed = app
            .db
            .write(|c| settings::remove(c, keys::UPLOADS_PAUSED))
            .await;
        logged(removed, "cannot clear the held upload limit")?;
        applied.marker = None;
        applied.held = false;
        publish(app, false).await;
        // While a core runs the restored limit replaced its upload limit, so the core limits go again.
        applied.core = applied.core && !core;
    }
    if applied.core != core {
        send_limits(app, client, core).await?;
        applied.core = core;
    }
    Some(())
}

/// Sends the core or menu limits.
async fn send_limits(app: &AppState, client: &dyn DownloadClient, core: bool) -> Option<()> {
    let (down, up) = limits_for(&app.config().limits, core);
    let sent = client.set_rate_limits(Some(down), Some(up)).await;
    logged(sent, "cannot apply rate limits")?;
    tracing::info!(core, down, up, "rate limits applied");
    Some(())
}

/// Records whether uploads are held and publishes the status when that changed.
async fn publish(app: &AppState, held: bool) {
    if app.set_uploads_paused(held) {
        let status = crate::status::snapshot(app).await;
        app.events.publish(EventKind::Status, &status);
    }
}

fn logged<T, E: Display>(r: Result<T, E>, what: &str) -> Option<T> {
    r.map_err(|e| tracing::warn!(error = %e, "{what}")).ok()
}

#[cfg(test)]
mod tests;
