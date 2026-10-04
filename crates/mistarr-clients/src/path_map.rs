//! Remote path mapping; see `docs/DOWNLOAD-CLIENTS.md` "Remote path mapping".

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One `{ remote, local }` prefix pair from `client.remote_path_map`.
///
/// ```
/// use mistarr_clients::PathMapping;
/// let m = PathMapping::new("/downloads", "/media/fat/mistarr/staging");
/// assert_eq!(m.remote.to_str(), Some("/downloads"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathMapping {
    /// Prefix as the client reports it.
    pub remote: PathBuf,
    /// The same directory as mistarr sees it.
    pub local: PathBuf,
}

impl PathMapping {
    /// Builds a pair from anything path-like.
    ///
    /// ```
    /// let m = mistarr_clients::PathMapping::new("/a", "/b");
    /// assert_eq!(m.local.to_str(), Some("/b"));
    /// ```
    pub fn new(remote: impl Into<PathBuf>, local: impl Into<PathBuf>) -> Self {
        Self {
            remote: remote.into(),
            local: local.into(),
        }
    }
}

/// Translates paths reported by a client on another machine into local
/// paths. Prefixes match whole components; the longest match wins.
///
/// ```
/// use std::path::Path;
/// use mistarr_clients::{PathMapping, RemotePathMap};
/// let map = RemotePathMap::new(vec![PathMapping::new("/downloads", "/media/fat/mistarr/staging")]);
/// assert_eq!(map.to_local(Path::new("/downloads/x/a.bin")), Path::new("/media/fat/mistarr/staging/x/a.bin"));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RemotePathMap {
    /// The configured pairs, in any order.
    pub entries: Vec<PathMapping>,
}

impl RemotePathMap {
    /// Builds a map from configured pairs.
    ///
    /// ```
    /// assert!(mistarr_clients::RemotePathMap::new(Vec::new()).entries.is_empty());
    /// ```
    #[must_use]
    pub fn new(entries: Vec<PathMapping>) -> Self {
        Self { entries }
    }

    /// Rewrites the longest matching remote prefix of `remote` to its local
    /// counterpart, or returns `remote` unchanged when nothing matches.
    ///
    /// ```
    /// use std::path::Path;
    /// let map = mistarr_clients::RemotePathMap::default();
    /// assert_eq!(map.to_local(Path::new("/srv/a")), Path::new("/srv/a"));
    /// ```
    #[must_use]
    pub fn to_local(&self, remote: &Path) -> PathBuf {
        self.rewrite(remote, |m| &m.remote, |m| &m.local)
    }

    /// The inverse of [`RemotePathMap::to_local`]: rewrites the longest matching
    /// local prefix of `local` to the path the client knows it by.
    ///
    /// ```
    /// use std::path::Path;
    /// use mistarr_clients::{PathMapping, RemotePathMap};
    /// let map = RemotePathMap::new(vec![PathMapping::new("/downloads", "/media/fat/mistarr/staging")]);
    /// assert_eq!(map.to_remote(Path::new("/media/fat/mistarr/staging/ab")), Path::new("/downloads/ab"));
    /// ```
    #[must_use]
    pub fn to_remote(&self, local: &Path) -> PathBuf {
        self.rewrite(local, |m| &m.local, |m| &m.remote)
    }

    /// Replaces the longest `from` prefix of `path`, by whole components, with its `to`.
    fn rewrite(
        &self,
        path: &Path,
        from: fn(&PathMapping) -> &PathBuf,
        to: fn(&PathMapping) -> &PathBuf,
    ) -> PathBuf {
        self.entries
            .iter()
            .filter_map(|m| path.strip_prefix(from(m)).ok().map(|rest| (m, rest)))
            .max_by_key(|(m, _)| from(m).components().count())
            .map_or_else(
                || path.to_path_buf(),
                |(m, rest)| {
                    if rest.as_os_str().is_empty() {
                        to(m).clone()
                    } else {
                        to(m).join(rest)
                    }
                },
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map() -> RemotePathMap {
        RemotePathMap::new(vec![
            PathMapping::new("/data", "/mnt/nas"),
            PathMapping::new("/data/torrents", "/media/fat/mistarr/staging"),
            PathMapping::new("/other/", "/local/other"),
        ])
    }

    #[test]
    fn unmatched_path_is_unchanged() {
        assert_eq!(map().to_local(Path::new("/srv/x")), Path::new("/srv/x"));
    }

    #[test]
    fn longest_prefix_wins_regardless_of_order() {
        let m = map();
        assert_eq!(
            m.to_local(Path::new("/data/torrents/ab/c.bin")),
            Path::new("/media/fat/mistarr/staging/ab/c.bin")
        );
        assert_eq!(
            m.to_local(Path::new("/data/music/c.bin")),
            Path::new("/mnt/nas/music/c.bin")
        );
    }

    #[test]
    fn prefix_matches_whole_components_only() {
        assert_eq!(
            map().to_local(Path::new("/database/c.bin")),
            Path::new("/database/c.bin")
        );
    }

    #[test]
    fn exact_match_and_trailing_slash() {
        let m = map();
        assert_eq!(m.to_local(Path::new("/data")), Path::new("/mnt/nas"));
        assert_eq!(
            m.to_local(Path::new("/other/f")),
            Path::new("/local/other/f")
        );
    }

    #[test]
    fn remote_paths_invert_the_map() {
        let m = RemotePathMap::new(vec![
            PathMapping::new("/dl", "/data"),
            PathMapping::new("/dl2", "/data/staging"),
        ]);
        assert_eq!(
            m.to_remote(Path::new("/data/staging/x")),
            Path::new("/dl2/x")
        );
        assert_eq!(m.to_remote(Path::new("/data")), Path::new("/dl"));
        assert_eq!(m.to_remote(Path::new("/database")), Path::new("/database"));
        assert_eq!(
            RemotePathMap::default().to_remote(Path::new("/a")),
            Path::new("/a")
        );
    }

    #[test]
    fn deserialises_from_config_shape() {
        let m: RemotePathMap =
            serde_json::from_str(r#"[{"remote":"/downloads","local":"/staging"}]"#)
                .expect("valid json");
        assert_eq!(m.entries, vec![PathMapping::new("/downloads", "/staging")]);
    }
}
