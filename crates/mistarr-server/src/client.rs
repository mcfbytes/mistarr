//! The download client handle built from detection; see `docs/DOWNLOAD-CLIENTS.md`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use mistarr_clients::{
    ClientKind, DownloadClient, PathMapping, RemotePathMap, Rtorrent, Transmission,
};

use crate::config::ClientConfig;
use crate::jobs::detect_client::ClientStatus;

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
    ///     rtorrent_on_path: false, checked_at: 0 };
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

#[cfg(test)]
mod tests {
    use super::*;

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
