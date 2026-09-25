//! The download client handle built from detection; see `docs/DOWNLOAD-CLIENTS.md`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use mistarr_clients::{
    ClientKind, DownloadClient, PathMapping, RemotePathMap, Rtorrent, Transmission,
};

use crate::config::ClientConfig;
use crate::jobs::detect_client::ClientStatus;

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

    /// Builds the client; `None` when the URL does not suit the kind.
    ///
    /// ```
    /// use mistarr_clients::ClientKind;
    /// use mistarr_server::client::ClientKey;
    /// let key = ClientKey { kind: ClientKind::Rtorrent, url: "127.0.0.1:5000".into(), path_map: vec![] };
    /// assert!(key.build().is_some());
    /// ```
    #[must_use]
    pub fn build(&self) -> Option<Arc<dyn DownloadClient>> {
        let built: Result<Arc<dyn DownloadClient>, _> = match self.kind {
            ClientKind::Transmission => {
                Transmission::new(&self.url).map(|t| Arc::new(t) as Arc<dyn DownloadClient>)
            }
            ClientKind::Rtorrent => Rtorrent::new(&self.url).map(|r| {
                let map = RemotePathMap::new(self.path_map.clone());
                Arc::new(r.with_path_map(map)) as Arc<dyn DownloadClient>
            }),
            _ => return None,
        };
        built
            .map_err(|e| tracing::warn!(error = %e, "cannot use the detected client"))
            .ok()
    }
}

/// The path the client should be given for a local directory: the inverse of
/// `client.remote_path_map`, longest local prefix first, whole components only.
///
/// ```
/// use std::path::Path;
/// use mistarr_clients::PathMapping;
/// let map = [PathMapping::new("/downloads", "/media/fat/mistarr/staging")];
/// let dir = mistarr_server::client::to_remote(&map, Path::new("/media/fat/mistarr/staging/ab"));
/// assert_eq!(dir, Path::new("/downloads/ab"));
/// ```
#[must_use]
pub fn to_remote(map: &[PathMapping], local: &Path) -> PathBuf {
    map.iter()
        .filter_map(|m| local.strip_prefix(&m.local).ok().map(|rest| (m, rest)))
        .max_by_key(|(m, _)| m.local.components().count())
        .map_or_else(
            || local.to_path_buf(),
            |(m, rest)| {
                if rest.as_os_str().is_empty() {
                    m.remote.clone()
                } else {
                    m.remote.join(rest)
                }
            },
        )
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
        assert!(a.build().is_some());
        config.remote_path_map = vec![PathMapping::new("/r", "/l")];
        let b = ClientKey::from_detection(&found, &config).expect("key");
        assert_ne!(a, b);
        let no_url = status(Some(ClientKind::Rtorrent), None);
        assert!(ClientKey::from_detection(&no_url, &config).is_none());
        let bad = ClientKey {
            kind: ClientKind::Transmission,
            url: "not a url".into(),
            path_map: Vec::new(),
        };
        assert!(bad.build().is_none());
    }

    #[test]
    fn remote_paths_invert_the_map() {
        let map = [
            PathMapping::new("/dl", "/data"),
            PathMapping::new("/dl2", "/data/staging"),
        ];
        assert_eq!(
            to_remote(&map, Path::new("/data/staging/x")),
            Path::new("/dl2/x")
        );
        assert_eq!(to_remote(&map, Path::new("/data")), Path::new("/dl"));
        assert_eq!(
            to_remote(&map, Path::new("/database")),
            Path::new("/database")
        );
        assert_eq!(to_remote(&[], Path::new("/a")), Path::new("/a"));
    }
}
