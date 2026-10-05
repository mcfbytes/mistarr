//! The download client handle built from detection; see `docs/DOWNLOAD-CLIENTS.md`.

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use mistarr_clients::{ClientKind, DownloadClient, PathMapping};

use crate::config::ClientConfig;
use crate::jobs::detect_client::ClientStatus;
use crate::jobs::watch::core_limits::ClientHold;

/// The detected download client, how it is held for a running core, and the locks that
/// order detection, starting and freezing it. The handle and the hold change only
/// through methods; the locks are taken directly.
pub struct ClientSlot {
    handle: RwLock<Option<(ClientKey, Arc<dyn DownloadClient>)>>,
    hold: RwLock<Option<ClientHold>>,
    /// Held while the client is stopped or resumed for shutdown, so no stop
    /// lands after shutdown resumed it.
    pub freeze_lock: Arc<Mutex<()>>,
    /// Serialises client detection so an older probe never overwrites a newer one.
    pub detect_lock: tokio::sync::Mutex<()>,
    /// Held while `POST /system/client/start` runs, so a second one is `busy`.
    pub start_lock: tokio::sync::Mutex<()>,
}

impl Default for ClientSlot {
    fn default() -> Self {
        Self {
            handle: RwLock::new(None),
            hold: RwLock::new(None),
            freeze_lock: Arc::new(Mutex::new(())),
            detect_lock: tokio::sync::Mutex::new(()),
            start_lock: tokio::sync::Mutex::new(()),
        }
    }
}

impl ClientSlot {
    /// The client from the last detection, `None` when detection found none or while
    /// the client is frozen for a running core, since a stopped process never answers.
    /// It is a Transmission or rtorrent handle for the detected URL carrying
    /// `client.remote_path_map`; take a fresh one per operation, and expect
    /// `Unreachable` when the client is down.
    ///
    /// ```
    /// assert!(mistarr_server::client::ClientSlot::default().get().is_none());
    /// ```
    #[must_use]
    pub fn get(&self) -> Option<Arc<dyn DownloadClient>> {
        if self.frozen() {
            return None;
        }
        self.entry().map(|(_, c)| c)
    }

    /// The detected client and which one it is, frozen or not; only the core gate
    /// talks to it through this.
    ///
    /// ```
    /// assert!(mistarr_server::client::ClientSlot::default().entry().is_none());
    /// ```
    #[must_use]
    pub fn entry(&self) -> Option<(ClientEndpoint, Arc<dyn DownloadClient>)> {
        self.handle
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|(k, c)| (k.endpoint(), Arc::clone(c)))
    }

    /// How the client is held for a running core, if it is.
    ///
    /// ```
    /// assert_eq!(mistarr_server::client::ClientSlot::default().hold(), None);
    /// ```
    #[must_use]
    pub fn hold(&self) -> Option<ClientHold> {
        *self.hold.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether the client's process is stopped for a running core.
    ///
    /// ```
    /// use mistarr_server::{client::ClientSlot, jobs::watch::core_limits::ClientHold};
    /// let slot = ClientSlot::default();
    /// assert!(slot.set_hold(Some(ClientHold::Frozen)) && slot.frozen());
    /// ```
    #[must_use]
    pub fn frozen(&self) -> bool {
        self.hold() == Some(ClientHold::Frozen)
    }

    /// Records how the client is held; true when that changed.
    pub fn set_hold(&self, hold: Option<ClientHold>) -> bool {
        let mut slot = self.hold.write().unwrap_or_else(PoisonError::into_inner);
        std::mem::replace(&mut *slot, hold) != hold
    }

    /// Points [`ClientSlot::get`] at what `status` found under `config`, and says
    /// whether the handle was replaced. The current handle stays unless the probe
    /// answered from a different client, or only the path map changed; a probe that
    /// found nothing never drops it.
    pub fn refresh(&self, status: &ClientStatus, config: &ClientConfig) -> bool {
        let Some(key) = ClientKey::from_detection(status, config) else {
            return false;
        };
        let mut slot = self.handle.write().unwrap_or_else(PoisonError::into_inner);
        let replace = match slot.as_ref() {
            None => true,
            Some((k, _)) if *k == key => false,
            Some((k, _)) => status.reachable || (k.kind == key.kind && k.url == key.url),
        };
        if !replace {
            return false;
        }
        let map = mistarr_clients::RemotePathMap::new(key.path_map.clone());
        match mistarr_clients::connect(key.kind, &key.url, map) {
            Ok(client) => {
                *slot = Some((key, client));
                true
            }
            Err(e) => {
                tracing::warn!(error = %e, "cannot use the detected client");
                false
            }
        }
    }

    /// Installs `client` as the client at `at`, for tests that script one in process.
    #[cfg(test)]
    pub(crate) fn set(&self, at: ClientEndpoint, client: Arc<dyn DownloadClient>) {
        let key = ClientKey {
            kind: at.kind,
            url: at.url,
            path_map: Vec::new(),
        };
        *self.handle.write().unwrap_or_else(PoisonError::into_inner) = Some((key, client));
    }

    /// Drops the handle, leaving no client, as before the first detection.
    #[cfg(test)]
    pub(crate) fn clear(&self) {
        *self.handle.write().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

/// Which client a handle talks to: its kind and RPC URL or SCGI address.
///
/// ```
/// use mistarr_clients::ClientKind;
/// use mistarr_server::client::ClientEndpoint;
/// let e = ClientEndpoint { kind: ClientKind::Rtorrent, url: "127.0.0.1:5000".into() };
/// assert_eq!(serde_json::to_string(&e).unwrap(), r#"{"kind":"rtorrent","url":"127.0.0.1:5000"}"#);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ClientEndpoint {
    /// The client.
    pub kind: ClientKind,
    /// Its RPC URL or SCGI address.
    pub url: String,
}

/// What a handle was built from; an equal key keeps the existing handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientKey {
    /// The detected client.
    pub kind: ClientKind,
    /// Its RPC URL or SCGI address.
    pub url: String,
    /// `client.remote_path_map` at build time.
    pub path_map: Vec<PathMapping>,
}

impl ClientKey {
    /// The key for a detection result, `None` when no client was found.
    ///
    /// ```
    /// use mistarr_server::client::ClientKey;
    /// use mistarr_server::config::ClientConfig;
    /// use mistarr_server::jobs::detect_client::ClientStatus;
    /// let none = ClientStatus { kind: None, url: None, reachable: false, version: None,
    ///     rtorrent_on_path: false, checked_at: 0, ..ClientStatus::default() };
    /// assert!(ClientKey::from_detection(&none, &ClientConfig::default()).is_none());
    /// ```
    #[must_use]
    pub fn from_detection(status: &ClientStatus, config: &ClientConfig) -> Option<Self> {
        Some(Self {
            kind: status.kind?,
            url: status.url.clone()?,
            path_map: config.remote_path_map.clone(),
        })
    }

    /// The client this key talks to.
    ///
    /// ```
    /// use mistarr_clients::ClientKind;
    /// use mistarr_server::client::ClientKey;
    /// let key = ClientKey { kind: ClientKind::Rtorrent, url: "127.0.0.1:5000".into(), path_map: vec![] };
    /// assert_eq!(key.endpoint().url, "127.0.0.1:5000");
    /// ```
    #[must_use]
    pub fn endpoint(&self) -> ClientEndpoint {
        ClientEndpoint {
            kind: self.kind,
            url: self.url.clone(),
        }
    }
}

/// Creates `local`, the staging directory a torrent is added into, because
/// rtorrent creates only the last level of a download path. A failure is
/// logged and left to the client, which may reach the directory another way.
pub async fn prepare_download_dir(local: &Path) {
    if let Err(e) = tokio::fs::create_dir_all(local).await {
        tracing::warn!(dir = %local.display(), error = %e, "cannot create the download directory");
    }
}

/// Host and port of a client URL: an `http://` RPC URL or an SCGI address,
/// `None` for a unix socket path.
fn host_port(url: &str) -> Option<(&str, Option<u16>)> {
    let rest = ["http://", "https://", "scgi://"]
        .iter()
        .find_map(|p| url.strip_prefix(p))
        .unwrap_or(url);
    if rest.starts_with('/') {
        return None;
    }
    let authority = rest.split('/').next().unwrap_or(rest);
    if let Some(v6) = authority.strip_prefix('[') {
        let (host, tail) = v6.split_once(']')?;
        let port = tail.strip_prefix(':').and_then(|p| p.parse().ok());
        return Some((host, port));
    }
    match authority.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') => Some((host, port.parse().ok())),
        _ => Some((authority, None)),
    }
}

/// Whether a client at `url` runs on this machine: a unix socket, or a
/// loopback host.
///
/// ```
/// use mistarr_server::client::is_local;
/// assert!(is_local("http://127.0.0.1:9091/transmission/rpc"));
/// assert!(is_local("scgi:///media/fat/mistarr/rtorrent.sock"));
/// assert!(!is_local("http://192.168.1.5:9091/transmission/rpc"));
/// ```
#[must_use]
pub fn is_local(url: &str) -> bool {
    match host_port(url) {
        None => true,
        Some((host, _)) => {
            host.eq_ignore_ascii_case("localhost")
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        }
    }
}

/// The TCP port of a client URL, when it names one.
///
/// ```
/// assert_eq!(mistarr_server::client::port("127.0.0.1:5000"), Some(5000));
/// assert_eq!(mistarr_server::client::port("/run/rtorrent.sock"), None);
/// ```
#[must_use]
pub fn port(url: &str) -> Option<u16> {
    host_port(url).and_then(|(_, p)| p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_and_sockets_are_local() {
        for url in [
            "http://127.0.0.1:9091/transmission/rpc",
            "http://localhost:9091/transmission/rpc",
            "http://[::1]:9091/transmission/rpc",
            "127.0.0.1:5000",
            "scgi://127.0.0.2:5000",
            "/media/fat/mistarr/rtorrent.sock",
            "scgi:///run/rtorrent.sock",
        ] {
            assert!(is_local(url), "{url}");
        }
        for url in [
            "http://192.168.1.5:9091/transmission/rpc",
            "http://nas.home.arpa:9091/transmission/rpc",
            "10.0.0.2:5000",
            "http://[fe80::1]:9091/",
        ] {
            assert!(!is_local(url), "{url}");
        }
        assert_eq!(port("http://127.0.0.1:9091/transmission/rpc"), Some(9091));
        assert_eq!(port("http://[::1]:9092/x"), Some(9092));
        assert_eq!(port("scgi://127.0.0.1:5001"), Some(5001));
        assert_eq!(port("http://localhost/transmission/rpc"), None);
    }

    #[tokio::test]
    async fn download_dirs_are_created_with_their_parents() {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root.path().join("staging").join("ab");
        prepare_download_dir(&dir).await;
        assert!(dir.is_dir());
        prepare_download_dir(&dir).await;
        assert!(dir.is_dir());
    }

    fn status(kind: Option<ClientKind>, url: Option<&str>) -> ClientStatus {
        ClientStatus {
            kind,
            url: url.map(str::to_owned),
            reachable: false,
            version: None,
            rtorrent_on_path: false,
            checked_at: 0,
            ..ClientStatus::default()
        }
    }

    #[test]
    fn keys_follow_detection_and_path_map() {
        let mut config = ClientConfig::default();
        let found = status(
            Some(ClientKind::Transmission),
            Some("http://127.0.0.1:1/rpc"),
        );
        let a = ClientKey::from_detection(&found, &config).expect("key");
        assert_eq!(a.endpoint().url, "http://127.0.0.1:1/rpc");
        config.remote_path_map = vec![PathMapping::new("/r", "/l")];
        let b = ClientKey::from_detection(&found, &config).expect("key");
        assert_ne!(a, b);
        let no_url = status(Some(ClientKind::Rtorrent), None);
        assert!(ClientKey::from_detection(&no_url, &config).is_none());
    }
}
