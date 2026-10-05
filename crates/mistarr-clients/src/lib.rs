//! The `DownloadClient` trait and its implementations; see `docs/DOWNLOAD-CLIENTS.md`.

use std::fmt;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use mistarr_core::InfoHash;
use serde::{Deserialize, Serialize};

pub mod detect;
mod error;
#[cfg(any(test, feature = "test-support"))]
pub mod fake;
pub mod fetch;
mod http;
pub mod launch;
mod path_map;
pub mod rtorrent;
mod scgi;
#[cfg(test)]
mod testutil;
pub mod transmission;
mod wanted;
#[cfg(any(test, feature = "test-support"))]
pub mod xmlrpc;
#[cfg(not(any(test, feature = "test-support")))]
mod xmlrpc;

pub use error::ClientError;
pub use path_map::{PathMapping, RemotePathMap};
pub use rtorrent::Rtorrent;
pub use transmission::Transmission;

/// Result type of every client operation.
///
/// ```
/// fn check(r: mistarr_clients::Result<()>) -> bool { r.is_ok() }
/// assert!(check(Ok(())));
/// ```
pub type Result<T, E = ClientError> = std::result::Result<T, E>;

/// Seeding policy for one source, chosen by the user (PRINCIPLES.md §4).
///
/// ```
/// use mistarr_clients::SeedPolicy;
/// let p: SeedPolicy = serde_json::from_str(r#"{"kind":"ratio","ratio":1.0}"#)?;
/// assert_eq!(p, SeedPolicy::Ratio { ratio: 1.0 });
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SeedPolicy {
    /// Stop as soon as every wanted file has been imported.
    None,
    /// Seed until the client reports this upload ratio, then stop.
    Ratio {
        /// Upload divided by download, e.g. `1.0`.
        ratio: f32,
    },
    /// Leave seeding to the client's own configuration.
    Client,
}

/// Which torrent client is behind a [`DownloadClient`]. Serialises as the
/// `client.kind` config value.
///
/// ```
/// use mistarr_clients::ClientKind;
/// assert_eq!(ClientKind::Rtorrent.as_str(), "rtorrent");
/// assert_eq!(serde_json::to_string(&ClientKind::Transmission)?, r#""transmission""#);
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum ClientKind {
    /// transmission-daemon, driven over JSON-RPC.
    Transmission,
    /// rtorrent, driven over XML-RPC on SCGI.
    Rtorrent,
}

impl ClientKind {
    /// The lowercase name used in config and the API.
    ///
    /// ```
    /// assert_eq!(mistarr_clients::ClientKind::Transmission.as_str(), "transmission");
    /// ```
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ClientKind::Transmission => "transmission",
            ClientKind::Rtorrent => "rtorrent",
        }
    }
}

impl fmt::Display for ClientKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What [`DownloadClient::probe`] learned about the client.
///
/// ```
/// use mistarr_clients::{ClientInfo, ClientKind};
/// let info = ClientInfo { kind: ClientKind::Transmission, version: "4.0.0".into() };
/// assert_eq!(info.kind, ClientKind::Transmission);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientInfo {
    /// Which client answered.
    pub kind: ClientKind,
    /// The version string the client reports, unparsed.
    pub version: String,
}

/// Which way a global rate limit applies.
///
/// ```
/// assert_ne!(mistarr_clients::Direction::Down, mistarr_clients::Direction::Up);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    /// Downloads.
    Down,
    /// Uploads.
    Up,
}

/// A client's global rate limit in one direction, as [`DownloadClient::rate_limit`]
/// read it, so [`DownloadClient::set_rate_limit`] can put it back exactly. The rate
/// is kept while the limit is off, as Transmission keeps it.
///
/// ```
/// use mistarr_clients::RateLimit;
/// let l: RateLimit = serde_json::from_str(r#"{"enabled":true,"kbps":64}"#)?;
/// assert_eq!(l, RateLimit::kbps(64));
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateLimit {
    /// Whether the limit applies.
    pub enabled: bool,
    /// The limit in the client's kilobytes per second: 1000 bytes for
    /// Transmission, 1024 for rtorrent.
    pub kbps: u32,
}

impl RateLimit {
    /// The lowest rate the client holds: zero in Transmission, 1 KiB/s in
    /// rtorrent, which reads zero as no limit.
    pub const HELD: Self = Self {
        enabled: true,
        kbps: 0,
    };

    /// An enabled limit of `kbps`.
    ///
    /// ```
    /// assert!(mistarr_clients::RateLimit::kbps(5).enabled);
    /// ```
    #[must_use]
    pub const fn kbps(kbps: u32) -> Self {
        Self {
            enabled: true,
            kbps,
        }
    }

    /// Whether this reading holds the direction at [`RateLimit::HELD`] or
    /// what a client keeps for it.
    ///
    /// ```
    /// use mistarr_clients::RateLimit;
    /// assert!(RateLimit::HELD.is_held() && RateLimit::kbps(1).is_held());
    /// assert!(!RateLimit::kbps(2).is_held() && !RateLimit::default().is_held());
    /// ```
    #[must_use]
    pub const fn is_held(self) -> bool {
        self.enabled && self.kbps <= 1
    }
}

/// The torrent to hand to the client, with what the caller's own parse of it
/// found, so no client reads the metainfo or the magnet again.
///
/// ```
/// use mistarr_clients::TorrentSource;
/// use mistarr_core::InfoHash;
/// let infohash = InfoHash::from_bytes([1; 20]);
/// let src = TorrentSource::Magnet { uri: format!("magnet:?xt=urn:btih:{infohash}"), infohash };
/// assert_eq!(src.infohash(), infohash);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TorrentSource {
    /// The bytes of a `.torrent` file.
    Metainfo {
        /// The whole file.
        bytes: Vec<u8>,
        /// Its v1 infohash.
        infohash: InfoHash,
        /// Entries in its v1 file list: the length of `info.files`, or 1 for a single file.
        file_count: usize,
    },
    /// A magnet URI; the client fetches the metadata from peers.
    Magnet {
        /// The URI as the user gave it.
        uri: String,
        /// The infohash of its `xt=urn:btih:` topic.
        infohash: InfoHash,
    },
}

impl TorrentSource {
    /// The torrent's v1 infohash.
    ///
    /// ```
    /// use mistarr_clients::TorrentSource;
    /// use mistarr_core::InfoHash;
    /// let infohash = InfoHash::from_bytes([2; 20]);
    /// let src = TorrentSource::Metainfo { bytes: Vec::new(), infohash, file_count: 1 };
    /// assert_eq!(src.infohash(), infohash);
    /// ```
    #[must_use]
    pub fn infohash(&self) -> InfoHash {
        match self {
            Self::Metainfo { infohash, .. } | Self::Magnet { infohash, .. } => *infohash,
        }
    }
}

/// The client's own identifier for a torrent, stored in `sources.client_id`.
/// Transmission and rtorrent both name a torrent by its v1 infohash; it
/// displays and serialises as 40 lowercase hex digits.
///
/// ```
/// use mistarr_clients::ClientTorrentId;
/// use mistarr_core::InfoHash;
/// let id = ClientTorrentId::new(InfoHash::from_bytes([0xab; 20]));
/// assert_eq!(id.to_string(), "ab".repeat(20));
/// assert_eq!(id.to_string().parse::<ClientTorrentId>().ok(), Some(id));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ClientTorrentId(InfoHash);

impl ClientTorrentId {
    /// The id of the torrent with `infohash`.
    ///
    /// ```
    /// use mistarr_core::InfoHash;
    /// let h = InfoHash::from_bytes([1; 20]);
    /// assert_eq!(mistarr_clients::ClientTorrentId::new(h).infohash(), h);
    /// ```
    #[must_use]
    pub const fn new(infohash: InfoHash) -> Self {
        Self(infohash)
    }

    /// The infohash the id names.
    ///
    /// ```
    /// use mistarr_core::InfoHash;
    /// let id = mistarr_clients::ClientTorrentId::new(InfoHash::from_bytes([3; 20]));
    /// assert_eq!(id.infohash().as_bytes()[0], 3);
    /// ```
    #[must_use]
    pub const fn infohash(&self) -> InfoHash {
        self.0
    }
}

impl fmt::Display for ClientTorrentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Reads a stored id in either case. Text that is not an infohash names no
/// torrent in any client, so it fails as [`ClientError::NotFound`].
impl FromStr for ClientTorrentId {
    type Err = ClientError;

    fn from_str(s: &str) -> Result<Self> {
        s.parse().map(Self).map_err(|_| ClientError::NotFound)
    }
}

/// The client for `kind` at `url`, an RPC URL or SCGI address, with `map`
/// translating paths between mistarr and the client both ways.
/// No request is made until the first operation.
///
/// # Errors
/// [`ClientError::Protocol`] when `url` does not suit `kind`.
///
/// ```
/// use mistarr_clients::{connect, ClientKind, RemotePathMap};
/// assert!(connect(ClientKind::Rtorrent, "127.0.0.1:5000", RemotePathMap::default()).is_ok());
/// assert!(connect(ClientKind::Transmission, "127.0.0.1:5000", RemotePathMap::default()).is_err());
/// ```
pub fn connect(kind: ClientKind, url: &str, map: RemotePathMap) -> Result<Arc<dyn DownloadClient>> {
    Ok(match kind {
        ClientKind::Transmission => Arc::new(Transmission::new(url)?.with_path_map(map)),
        ClientKind::Rtorrent => Arc::new(Rtorrent::new(url)?.with_path_map(map)),
    })
}

/// Coarse state of a torrent as the client reports it.
///
/// ```
/// use mistarr_clients::TorrentState;
/// let s = TorrentState::Error("disk full".into());
/// assert_ne!(s, TorrentState::Seeding);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum TorrentState {
    /// Paused by the user, by mistarr or by the seed policy.
    Stopped,
    /// Waiting for a download or seed slot.
    Queued,
    /// Verifying local data, or waiting to; progress is not yet trustworthy.
    Checking,
    /// Transferring wanted pieces.
    Downloading,
    /// Every wanted piece is present and the torrent is uploading.
    Seeding,
    /// The client stopped the torrent because of a local error.
    Error(String),
}

/// Progress of one file of a torrent, by index in the metainfo file list.
///
/// ```
/// use mistarr_clients::FileProgress;
/// let f = FileProgress { index: 0, bytes_done: 10, size: Some(10), wanted: true };
/// assert!(f.is_complete());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileProgress {
    /// Index in the torrent's file list.
    pub index: u32,
    /// Bytes of this file the client has verified.
    pub bytes_done: u64,
    /// Size of the file in bytes; `None` when the client's status leaves sizes to
    /// [`DownloadClient::files`] and the metainfo, as Transmission's does for an unfinished torrent.
    pub size: Option<u64>,
    /// Whether the client is set to download this file.
    pub wanted: bool,
}

impl FileProgress {
    /// True when every byte of the file is present; false while its size is unknown.
    ///
    /// ```
    /// use mistarr_clients::FileProgress;
    /// assert!(!FileProgress { index: 0, bytes_done: 1, size: Some(2), wanted: true }.is_complete());
    /// assert!(!FileProgress { index: 0, bytes_done: 2, size: None, wanted: true }.is_complete());
    /// ```
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.size == Some(self.bytes_done)
    }

    /// The file's size as the client reports it, else `known`, its size from the metainfo.
    ///
    /// ```
    /// use mistarr_clients::FileProgress;
    /// let f = FileProgress { index: 0, bytes_done: 2, size: None, wanted: true };
    /// assert_eq!(f.size_or(2), 2);
    /// assert_eq!(FileProgress { size: Some(4), ..f }.size_or(2), 4);
    /// ```
    #[must_use]
    pub fn size_or(&self, known: u64) -> u64 {
        self.size.unwrap_or(known)
    }
}

/// One file of a torrent as the client lists it, from [`DownloadClient::files`].
///
/// ```
/// use mistarr_clients::ClientFile;
/// let f = ClientFile { index: 0, path: "Sub/a.bin".into(), size: 4 };
/// assert_eq!(f.path, "Sub/a.bin");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientFile {
    /// Index in the torrent's file list.
    pub index: u32,
    /// Path inside the torrent, `/`-separated, without the torrent's own name.
    pub path: String,
    /// Size of the file in bytes.
    pub size: u64,
}

/// A snapshot of one torrent, as returned by [`DownloadClient::status`].
///
/// ```
/// use mistarr_clients::{FileProgress, TorrentState, TorrentStatus};
/// use mistarr_core::InfoHash;
/// let st = TorrentStatus {
///     infohash: InfoHash::from_bytes([7; 20]),
///     state: TorrentState::Downloading,
///     files: vec![FileProgress { index: 0, bytes_done: 4, size: Some(4), wanted: true }],
///     ratio: 0.0,
///     down_rate: 0,
///     up_rate: 0,
///     is_finished: false,
/// };
/// assert!(st.file_done(0, 4));
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct TorrentStatus {
    /// The torrent's v1 infohash.
    pub infohash: InfoHash,
    /// Coarse state.
    pub state: TorrentState,
    /// Per-file progress in index order; empty while a magnet has no metadata.
    pub files: Vec<FileProgress>,
    /// Upload ratio; `0.0` when nothing was downloaded, infinite when seeding
    /// data that was never downloaded.
    pub ratio: f32,
    /// Download rate in bytes per second.
    pub down_rate: u64,
    /// Upload rate in bytes per second.
    pub up_rate: u64,
    /// True when the client stopped the torrent after reaching its seed limit.
    pub is_finished: bool,
}

impl TorrentStatus {
    /// True when file `index`, of `size` bytes by the metainfo, is complete and the client
    /// is not checking, which is when the importer may take it (docs/DOWNLOAD-CLIENTS.md).
    ///
    /// ```
    /// use mistarr_clients::{FileProgress, TorrentState, TorrentStatus};
    /// use mistarr_core::InfoHash;
    /// let mut st = TorrentStatus {
    ///     infohash: InfoHash::from_bytes([7; 20]),
    ///     state: TorrentState::Checking,
    ///     files: vec![FileProgress { index: 0, bytes_done: 4, size: None, wanted: true }],
    ///     ratio: 0.0, down_rate: 0, up_rate: 0, is_finished: false,
    /// };
    /// assert!(!st.file_done(0, 4));
    /// st.state = TorrentState::Downloading;
    /// assert!(st.file_done(0, 4));
    /// assert!(!st.file_done(0, 5));
    /// assert!(!st.file_done(9, 4));
    /// ```
    #[must_use]
    pub fn file_done(&self, index: u32, size: u64) -> bool {
        self.state != TorrentState::Checking
            && self
                .file(index)
                .is_some_and(|f| f.bytes_done == f.size_or(size))
    }

    /// Progress of file `index`. Clients list files in index order, so this is a direct
    /// lookup, falling back to a search for a list that is not.
    ///
    /// ```
    /// use mistarr_clients::{FileProgress, TorrentState, TorrentStatus};
    /// use mistarr_core::InfoHash;
    /// let file = |index| FileProgress { index, bytes_done: 0, size: Some(4), wanted: true };
    /// let st = TorrentStatus {
    ///     infohash: InfoHash::from_bytes([7; 20]),
    ///     state: TorrentState::Downloading,
    ///     files: vec![file(0), file(1), file(5)],
    ///     ratio: 0.0, down_rate: 0, up_rate: 0, is_finished: false,
    /// };
    /// assert_eq!(st.file(1).map(|f| f.index), Some(1));
    /// assert_eq!(st.file(5).map(|f| f.index), Some(5));
    /// assert!(st.file(2).is_none());
    /// ```
    #[must_use]
    pub fn file(&self, index: u32) -> Option<&FileProgress> {
        usize::try_from(index)
            .ok()
            .and_then(|i| self.files.get(i))
            .filter(|f| f.index == index)
            .or_else(|| self.files.iter().find(|f| f.index == index))
    }
}

/// One torrent client, driven over its RPC. Implementations serialise their
/// calls, so at most one RPC is in flight per client (ARCHITECTURE.md budgets).
///
/// Operations that name a torrent return [`ClientError::NotFound`] when the
/// client does not have it.
///
/// ```no_run
/// use std::path::Path;
/// use mistarr_clients::{DownloadClient, SeedPolicy, TorrentSource, Transmission};
/// use mistarr_core::InfoHash;
///
/// async fn fetch(bytes: Vec<u8>, infohash: InfoHash) -> mistarr_clients::Result<()> {
///     let client = Transmission::new(Transmission::DEFAULT_URL)?;
///     let src = TorrentSource::Metainfo { bytes, infohash, file_count: 1 };
///     let id = client.add(src, Path::new("/media/fat/mistarr/staging/x"), &[0], SeedPolicy::None).await?;
///     client.start(&id).await?;
///     let status = client.status(&id).await?;
///     println!("{:?}", status.files);
///     Ok(())
/// }
/// ```
#[async_trait]
pub trait DownloadClient: Send + Sync {
    /// Checks the client answers and reports its version.
    async fn probe(&self) -> Result<ClientInfo>;

    /// Adds a torrent paused into `download_dir`, a local path the client
    /// maps to its own, with only the `wanted` file indices selected and
    /// `seed` applied, and returns its id.
    ///
    /// If the client already has the torrent, `wanted` replaces its selection
    /// and `seed` is applied, so retrying a failed `add` repairs it.
    /// For a magnet whose metadata the client does not have yet, the
    /// selection is not applied: call `set_wanted` once `status` lists files.
    async fn add(
        &self,
        src: TorrentSource,
        download_dir: &Path,
        wanted: &[u32],
        seed: SeedPolicy,
    ) -> Result<ClientTorrentId>;

    /// Replaces the set of wanted files; every other file is deselected.
    /// Returns [`ClientError::MetadataPending`] for a magnet without metadata.
    async fn set_wanted(&self, id: &ClientTorrentId, wanted: &[u32]) -> Result<()>;

    /// Applies a seeding policy to a torrent already in the client.
    async fn set_seed_policy(&self, id: &ClientTorrentId, seed: SeedPolicy) -> Result<()>;

    /// Starts or resumes transfer.
    async fn start(&self, id: &ClientTorrentId) -> Result<()>;

    /// Pauses transfer; data stays on disk.
    async fn stop(&self, id: &ClientTorrentId) -> Result<()>;

    /// Reads state, rates and per-file progress.
    async fn status(&self, id: &ClientTorrentId) -> Result<TorrentStatus>;

    /// Lists the torrent's files with their paths, for binding a magnet once
    /// the client has its metadata. [`ClientError::MetadataPending`] until then.
    async fn files(&self, id: &ClientTorrentId) -> Result<Vec<ClientFile>>;

    /// Removes the torrent from the client, deleting its data if asked.
    async fn remove(&self, id: &ClientTorrentId, delete_data: bool) -> Result<()>;

    /// Reads the global rate limit in `dir`.
    async fn rate_limit(&self, dir: Direction) -> Result<RateLimit>;

    /// Sets the global rate limit in `dir`, exactly as [`DownloadClient::rate_limit`]
    /// read it, or [`RateLimit::HELD`] to hold that direction.
    async fn set_rate_limit(&self, dir: Direction, limit: RateLimit) -> Result<()>;

    /// The client's process id as it reports it, `None` when its RPC has no way to.
    async fn process_id(&self) -> Result<Option<u32>>;

    /// The alternate upload limit the client switches to by hand or on a
    /// schedule, with `enabled` saying whether it is in use; `None` when it has none.
    async fn alt_up_limit(&self) -> Result<Option<RateLimit>> {
        Ok(None)
    }

    /// Sets the alternate upload rate, leaving whether it is in use alone; a
    /// client without one ignores it.
    async fn set_alt_up_rate(&self, kbps: u32) -> Result<()> {
        let _ = kbps;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_read_either_case_and_refuse_other_text_as_not_found() {
        let id = ClientTorrentId::new(InfoHash::from_bytes([0xcd; 20]));
        assert_eq!("CD".repeat(20).parse::<ClientTorrentId>().ok(), Some(id));
        for bad in ["", "not-a-hash", &"cd".repeat(19)] {
            let err = bad.parse::<ClientTorrentId>().expect_err(bad);
            assert!(matches!(err, ClientError::NotFound), "{err:?}");
        }
        let json = serde_json::to_string(&id).expect("json");
        assert_eq!(json, format!("\"{}\"", "cd".repeat(20)));
        assert_eq!(
            serde_json::from_str::<ClientTorrentId>(&json).expect("id"),
            id
        );
    }

    #[test]
    fn connect_builds_each_kind_from_its_own_address() {
        let none = RemotePathMap::default;
        assert!(connect(ClientKind::Transmission, Transmission::DEFAULT_URL, none()).is_ok());
        assert!(connect(ClientKind::Rtorrent, "scgi://127.0.0.1:5000", none()).is_ok());
        for (kind, url) in [
            (ClientKind::Transmission, "not a url"),
            (ClientKind::Rtorrent, "http://127.0.0.1:9091/"),
        ] {
            let err = connect(kind, url, none()).err();
            assert!(matches!(err, Some(ClientError::Protocol(_))), "{kind}");
        }
    }
}
