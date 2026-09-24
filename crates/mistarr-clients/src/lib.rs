//! The `DownloadClient` trait and its implementations; see `docs/DOWNLOAD-CLIENTS.md`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]

use std::fmt;
use std::path::Path;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub mod detect;
mod error;
#[cfg(any(test, feature = "test-support"))]
pub mod fake;
mod http;
mod metainfo;
mod path_map;
pub mod rtorrent;
mod scgi;
pub mod transmission;
pub mod xmlrpc;

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

/// The torrent to hand to the client.
///
/// ```
/// use mistarr_clients::TorrentSource;
/// let src = TorrentSource::Metainfo(b"d4:infod6:lengthi1e4:name1:aee".to_vec());
/// assert!(matches!(src, TorrentSource::Metainfo(_)));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TorrentSource {
    /// The bytes of a `.torrent` file.
    Metainfo(Vec<u8>),
    /// A magnet URI; the client fetches the metadata from peers.
    Magnet(String),
}

/// The client's own identifier for a torrent, stored in `sources.client_id`.
/// For Transmission and rtorrent it is the lowercase hex infohash.
///
/// ```
/// use mistarr_clients::ClientTorrentId;
/// let id = ClientTorrentId::new("00ff");
/// assert_eq!(id.as_str(), "00ff");
/// assert_eq!(id.to_string(), "00ff");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ClientTorrentId(String);

impl ClientTorrentId {
    /// Wraps an identifier as the client reported it.
    ///
    /// ```
    /// let id = mistarr_clients::ClientTorrentId::new(String::from("ab"));
    /// assert_eq!(id.as_str(), "ab");
    /// ```
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The identifier as sent to the client.
    ///
    /// ```
    /// assert_eq!(mistarr_clients::ClientTorrentId::new("x").as_str(), "x");
    /// ```
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ClientTorrentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A 20-byte v1 infohash. Displays as 40 lowercase hex digits.
///
/// ```
/// use mistarr_clients::InfoHash;
/// let h = InfoHash::from_bytes([0xab; 20]);
/// assert_eq!(InfoHash::from_hex(&h.to_string()), Some(h));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InfoHash([u8; 20]);

impl InfoHash {
    /// Wraps raw hash bytes.
    ///
    /// ```
    /// let h = mistarr_clients::InfoHash::from_bytes([1; 20]);
    /// assert_eq!(h.as_bytes(), &[1; 20]);
    /// ```
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 20]) -> Self {
        Self(bytes)
    }

    /// Parses 40 hex digits in either case; `None` for anything else.
    ///
    /// ```
    /// use mistarr_clients::InfoHash;
    /// assert_eq!(InfoHash::from_hex(&"0A".repeat(20)), Some(InfoHash::from_bytes([10; 20])));
    /// assert_eq!(InfoHash::from_hex("0a"), None);
    /// ```
    #[must_use]
    pub fn from_hex(hex: &str) -> Option<Self> {
        let digits = hex.as_bytes();
        if digits.len() != 40 {
            return None;
        }
        let mut out = [0u8; 20];
        for (byte, pair) in out.iter_mut().zip(digits.chunks_exact(2)) {
            let hi = char::from(pair[0]).to_digit(16)?;
            let lo = char::from(pair[1]).to_digit(16)?;
            *byte = u8::try_from(hi << 4 | lo).ok()?;
        }
        Some(Self(out))
    }

    /// The raw hash bytes.
    ///
    /// ```
    /// assert_eq!(mistarr_clients::InfoHash::from_bytes([2; 20]).as_bytes()[0], 2);
    /// ```
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 20] {
        &self.0
    }
}

impl fmt::Display for InfoHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in &self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
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
/// let f = FileProgress { index: 0, bytes_done: 10, size: 10, wanted: true };
/// assert!(f.is_complete());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileProgress {
    /// Index in the torrent's file list.
    pub index: u32,
    /// Bytes of this file the client has verified.
    pub bytes_done: u64,
    /// Size of the file in bytes.
    pub size: u64,
    /// Whether the client is set to download this file.
    pub wanted: bool,
}

impl FileProgress {
    /// True when every byte of the file is present.
    ///
    /// ```
    /// use mistarr_clients::FileProgress;
    /// assert!(!FileProgress { index: 0, bytes_done: 1, size: 2, wanted: true }.is_complete());
    /// ```
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.bytes_done == self.size
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
/// use mistarr_clients::{FileProgress, InfoHash, TorrentState, TorrentStatus};
/// let st = TorrentStatus {
///     infohash: InfoHash::from_bytes([7; 20]),
///     state: TorrentState::Downloading,
///     files: vec![FileProgress { index: 0, bytes_done: 4, size: 4, wanted: true }],
///     ratio: 0.0,
///     down_rate: 0,
///     up_rate: 0,
///     is_finished: false,
/// };
/// assert!(st.file_done(0));
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
    /// True when file `index` is complete and the client is not checking,
    /// which is when the importer may take it (docs/DOWNLOAD-CLIENTS.md).
    ///
    /// ```
    /// use mistarr_clients::{FileProgress, InfoHash, TorrentState, TorrentStatus};
    /// let st = TorrentStatus {
    ///     infohash: InfoHash::from_bytes([7; 20]),
    ///     state: TorrentState::Checking,
    ///     files: vec![FileProgress { index: 0, bytes_done: 4, size: 4, wanted: true }],
    ///     ratio: 0.0, down_rate: 0, up_rate: 0, is_finished: false,
    /// };
    /// assert!(!st.file_done(0));
    /// assert!(!st.file_done(9));
    /// ```
    #[must_use]
    pub fn file_done(&self, index: u32) -> bool {
        self.state != TorrentState::Checking
            && self
                .files
                .iter()
                .any(|f| f.index == index && f.is_complete())
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
///
/// async fn fetch(metainfo: Vec<u8>) -> mistarr_clients::Result<()> {
///     let client = Transmission::new(Transmission::DEFAULT_URL)?;
///     let src = TorrentSource::Metainfo(metainfo);
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

    /// Adds a torrent paused into `download_dir` with only the `wanted` file
    /// indices selected and `seed` applied, and returns its id.
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

    /// Sets global rate limits in the client's kilobytes per second (1000
    /// bytes for Transmission, 1024 for rtorrent). `None` or `Some(0)` lifts the limit.
    async fn set_rate_limits(&self, down_kbps: Option<u32>, up_kbps: Option<u32>) -> Result<()>;
}
