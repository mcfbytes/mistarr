//! Transmission over JSON-RPC; see `docs/DOWNLOAD-CLIENTS.md` "Transmission".

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use hyper::body::Bytes;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use tokio::sync::Mutex;

use crate::http::{self, Endpoint, Headers};
use crate::{
    metainfo, ClientError, ClientInfo, ClientKind, ClientTorrentId, DownloadClient, FileProgress,
    InfoHash, Result, SeedPolicy, TorrentSource, TorrentState, TorrentStatus,
};

/// Fields requested by [`DownloadClient::status`].
const STATUS_FIELDS: [&str; 12] = [
    "id",
    "hashString",
    "status",
    "percentDone",
    "error",
    "errorString",
    "files",
    "fileStats",
    "rateDownload",
    "rateUpload",
    "uploadRatio",
    "isFinished",
];

/// Transmission's `error` value for a local error, which stops the torrent.
const LOCAL_ERROR: i64 = 3;

/// A transmission-daemon reached over its RPC URL.
///
/// Calls are serialised through one lock, which also guards the
/// `X-Transmission-Session-Id` the daemon hands out.
///
/// ```
/// use std::time::Duration;
/// use mistarr_clients::Transmission;
/// let t = Transmission::new(Transmission::DEFAULT_URL)?
///     .with_credentials("user", "pass")
///     .with_timeout(Duration::from_secs(10));
/// # Ok::<(), mistarr_clients::ClientError>(())
/// ```
pub struct Transmission {
    endpoint: Endpoint,
    authorization: Option<String>,
    timeout: Duration,
    session: Mutex<Option<String>>,
}

impl std::fmt::Debug for Transmission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Transmission")
            .field("endpoint", &self.endpoint)
            .field(
                "authorization",
                &self.authorization.as_ref().map(|_| "<redacted>"),
            )
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl Transmission {
    /// Where transmission-daemon listens by default.
    ///
    /// ```
    /// assert!(mistarr_clients::Transmission::DEFAULT_URL.starts_with("http://127.0.0.1:9091/"));
    /// ```
    pub const DEFAULT_URL: &'static str = "http://127.0.0.1:9091/transmission/rpc";

    /// Above this many files a selection is applied with the empty-list
    /// shorthand and verified, instead of listing every unwanted index.
    ///
    /// ```
    /// assert_eq!(mistarr_clients::Transmission::LARGE_TORRENT_FILES, 2000);
    /// ```
    pub const LARGE_TORRENT_FILES: usize = 2000;

    /// Default time allowed for one RPC, connect to last byte.
    ///
    /// ```
    /// assert_eq!(mistarr_clients::Transmission::DEFAULT_TIMEOUT.as_secs(), 30);
    /// ```
    pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

    /// A client for the RPC at `url`, e.g. [`Transmission::DEFAULT_URL`].
    /// No request is made until the first operation.
    ///
    /// # Errors
    /// [`ClientError::Protocol`] if `url` is not an `http://` URL.
    ///
    /// ```
    /// use mistarr_clients::Transmission;
    /// assert!(Transmission::new("http://127.0.0.1:9091/transmission/rpc").is_ok());
    /// assert!(Transmission::new("https://127.0.0.1:9091/transmission/rpc").is_err());
    /// ```
    pub fn new(url: &str) -> Result<Self> {
        Ok(Self {
            endpoint: Endpoint::parse(url)?,
            authorization: None,
            timeout: Self::DEFAULT_TIMEOUT,
            session: Mutex::new(None),
        })
    }

    /// Sends HTTP basic credentials with every request.
    ///
    /// ```
    /// let t = mistarr_clients::Transmission::new("http://127.0.0.1:9091/transmission/rpc")?
    ///     .with_credentials("user", "secret");
    /// # Ok::<(), mistarr_clients::ClientError>(())
    /// ```
    #[must_use]
    pub fn with_credentials(mut self, user: &str, password: &str) -> Self {
        let token = BASE64.encode(format!("{user}:{password}"));
        self.authorization = Some(format!("Basic {token}"));
        self
    }

    /// Sets the time allowed for one RPC.
    ///
    /// ```
    /// use std::time::Duration;
    /// let t = mistarr_clients::Transmission::new("http://127.0.0.1:9091/transmission/rpc")?
    ///     .with_timeout(Duration::from_secs(5));
    /// # Ok::<(), mistarr_clients::ClientError>(())
    /// ```
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Sends one RPC, renewing the session id once on a 409.
    async fn rpc(
        &self,
        session: &mut Option<String>,
        method: &str,
        arguments: Value,
    ) -> Result<Value> {
        let body = Bytes::from(json!({ "method": method, "arguments": arguments }).to_string());
        for _ in 0..2 {
            let headers = Headers {
                session_id: session.as_deref(),
                authorization: self.authorization.as_deref(),
            };
            let resp = http::post(&self.endpoint, headers, body.clone(), self.timeout).await?;
            match resp.status {
                409 => {
                    let id = resp.session_id.ok_or_else(|| {
                        ClientError::Protocol("409 without X-Transmission-Session-Id".into())
                    })?;
                    *session = Some(id);
                }
                401 | 403 => return Err(ClientError::Auth),
                200 => return parse_reply(&resp.body),
                status => return Err(ClientError::Protocol(format!("HTTP status {status}"))),
            }
        }
        Err(ClientError::Protocol(
            "session id rejected after renewal".into(),
        ))
    }

    /// `torrent-get` for one torrent; [`ClientError::NotFound`] if absent.
    async fn get_one(
        &self,
        session: &mut Option<String>,
        id: &ClientTorrentId,
        fields: &[&str],
    ) -> Result<Value> {
        let args = json!({ "ids": [id.as_str()], "fields": fields });
        let reply = self.rpc(session, "torrent-get", args).await?;
        let torrents = match reply {
            Value::Object(mut args) => args.remove("torrents"),
            _ => None,
        };
        match torrents {
            Some(Value::Array(list)) => list.into_iter().next().ok_or(ClientError::NotFound),
            _ => Err(ClientError::Protocol("torrent-get without torrents".into())),
        }
    }

    /// The per-file wanted flags; empty while a magnet has no metadata.
    async fn wanted_flags(
        &self,
        session: &mut Option<String>,
        id: &ClientTorrentId,
    ) -> Result<Vec<bool>> {
        let torrent = self.get_one(session, id, &["wanted"]).await?;
        let raw: WantedOnly = serde_json::from_value(torrent).map_err(protocol)?;
        Ok(raw.wanted.into_iter().map(Flag::into_bool).collect())
    }

    async fn torrent_set(
        &self,
        session: &mut Option<String>,
        id: &ClientTorrentId,
        fields: Value,
    ) -> Result<()> {
        let mut args = Map::new();
        args.insert("ids".into(), json!([id.as_str()]));
        if let Value::Object(extra) = fields {
            args.extend(extra);
        }
        self.rpc(session, "torrent-set", Value::Object(args))
            .await
            .map(drop)
    }

    /// Makes exactly `wanted` selected out of `file_count` files.
    async fn apply_selection(
        &self,
        session: &mut Option<String>,
        id: &ClientTorrentId,
        file_count: usize,
        wanted: &BTreeSet<u32>,
    ) -> Result<()> {
        if file_count <= Self::LARGE_TORRENT_FILES {
            // An empty index list means "all files" to Transmission, so omit empty lists.
            let mut fields = Map::new();
            if !wanted.is_empty() {
                fields.insert("files-wanted".into(), json!(wanted));
            }
            let unwanted = complement(wanted, file_count);
            if !unwanted.is_empty() {
                fields.insert("files-unwanted".into(), json!(unwanted));
            }
            if fields.is_empty() {
                return Ok(());
            }
            return self.torrent_set(session, id, Value::Object(fields)).await;
        }
        self.torrent_set(session, id, json!({ "files-unwanted": [] }))
            .await?;
        if !wanted.is_empty() {
            self.torrent_set(session, id, json!({ "files-wanted": wanted }))
                .await?;
        }
        let flags = self.wanted_flags(session, id).await?;
        let applied = flags.len() == file_count
            && flags
                .iter()
                .zip(0u32..)
                .all(|(&on, i)| on == wanted.contains(&i));
        if applied {
            Ok(())
        } else {
            Err(ClientError::Protocol(
                "file selection was not applied".into(),
            ))
        }
    }

    async fn apply_seed(
        &self,
        session: &mut Option<String>,
        id: &ClientTorrentId,
        seed: &SeedPolicy,
        fresh: bool,
    ) -> Result<()> {
        let fields = match seed {
            // A newly added torrent already follows the session default.
            SeedPolicy::Client if fresh => return Ok(()),
            SeedPolicy::Client => json!({ "seedRatioMode": 0 }),
            SeedPolicy::None => json!({ "seedRatioMode": 1, "seedRatioLimit": 0.0 }),
            SeedPolicy::Ratio { ratio } => {
                json!({ "seedRatioMode": 1, "seedRatioLimit": f64::from(*ratio) })
            }
        };
        self.torrent_set(session, id, fields).await
    }

    async fn simple(&self, method: &str, id: &ClientTorrentId, extra: Value) -> Result<()> {
        let mut session = self.session.lock().await;
        self.get_one(&mut session, id, &["id"]).await?;
        let mut args = Map::new();
        args.insert("ids".into(), json!([id.as_str()]));
        if let Value::Object(extra) = extra {
            args.extend(extra);
        }
        self.rpc(&mut session, method, Value::Object(args))
            .await
            .map(drop)
    }
}

#[async_trait]
impl DownloadClient for Transmission {
    async fn probe(&self) -> Result<ClientInfo> {
        let mut session = self.session.lock().await;
        let reply = self
            .rpc(
                &mut session,
                "session-get",
                json!({ "fields": ["version"] }),
            )
            .await?;
        let version = reply
            .get("version")
            .and_then(Value::as_str)
            .ok_or_else(|| ClientError::Protocol("session-get without version".into()))?;
        Ok(ClientInfo {
            kind: ClientKind::Transmission,
            version: version.to_owned(),
        })
    }

    async fn add(
        &self,
        src: TorrentSource,
        download_dir: &Path,
        wanted: &[u32],
        seed: SeedPolicy,
    ) -> Result<ClientTorrentId> {
        let wanted: BTreeSet<u32> = wanted.iter().copied().collect();
        let dir = download_dir
            .to_str()
            .ok_or_else(|| ClientError::Protocol("download dir is not UTF-8".into()))?;
        let mut args = Map::new();
        args.insert("download-dir".into(), json!(dir));
        args.insert("paused".into(), json!(true));
        let known_count = match &src {
            TorrentSource::Metainfo(bytes) => {
                args.insert("metainfo".into(), json!(BASE64.encode(bytes)));
                metainfo::file_count(bytes)
            }
            TorrentSource::Magnet(uri) => {
                args.insert("filename".into(), json!(uri));
                None
            }
        };
        if let Some(count) = known_count {
            check_indices(&wanted, count)?;
            let unwanted = complement(&wanted, count);
            if count <= Self::LARGE_TORRENT_FILES && !unwanted.is_empty() {
                args.insert("files-unwanted".into(), json!(unwanted));
            }
        }

        let mut session = self.session.lock().await;
        let reply = self
            .rpc(&mut session, "torrent-add", Value::Object(args))
            .await?;
        let (id, fresh) = match (reply.get("torrent-added"), reply.get("torrent-duplicate")) {
            (Some(added), _) => (torrent_id(added)?, true),
            (None, Some(dup)) => (torrent_id(dup)?, false),
            (None, None) => {
                return Err(ClientError::Protocol(
                    "torrent-add without torrent-added".into(),
                ))
            }
        };
        let sent_with_add = fresh && known_count.is_some_and(|c| c <= Self::LARGE_TORRENT_FILES);
        if !sent_with_add {
            let count = match known_count {
                Some(count) => count,
                None => self.wanted_flags(&mut session, &id).await?.len(),
            };
            if count > 0 {
                check_indices(&wanted, count)?;
                self.apply_selection(&mut session, &id, count, &wanted)
                    .await?;
            }
        }
        self.apply_seed(&mut session, &id, &seed, fresh).await?;
        Ok(id)
    }

    async fn set_wanted(&self, id: &ClientTorrentId, wanted: &[u32]) -> Result<()> {
        let wanted: BTreeSet<u32> = wanted.iter().copied().collect();
        let mut session = self.session.lock().await;
        let count = self.wanted_flags(&mut session, id).await?.len();
        if count == 0 {
            return Err(ClientError::MetadataPending);
        }
        check_indices(&wanted, count)?;
        self.apply_selection(&mut session, id, count, &wanted).await
    }

    async fn set_seed_policy(&self, id: &ClientTorrentId, seed: SeedPolicy) -> Result<()> {
        let mut session = self.session.lock().await;
        self.get_one(&mut session, id, &["id"]).await?;
        self.apply_seed(&mut session, id, &seed, false).await
    }

    async fn start(&self, id: &ClientTorrentId) -> Result<()> {
        self.simple("torrent-start", id, json!({})).await
    }

    async fn stop(&self, id: &ClientTorrentId) -> Result<()> {
        self.simple("torrent-stop", id, json!({})).await
    }

    async fn status(&self, id: &ClientTorrentId) -> Result<TorrentStatus> {
        let mut session = self.session.lock().await;
        let torrent = self.get_one(&mut session, id, &STATUS_FIELDS).await?;
        drop(session);
        let raw: RawTorrent = serde_json::from_value(torrent).map_err(protocol)?;
        raw.into_status()
    }

    async fn remove(&self, id: &ClientTorrentId, delete_data: bool) -> Result<()> {
        self.simple(
            "torrent-remove",
            id,
            json!({ "delete-local-data": delete_data }),
        )
        .await
    }

    async fn set_rate_limits(&self, down_kbps: Option<u32>, up_kbps: Option<u32>) -> Result<()> {
        let mut args = Map::new();
        for (dir, limit) in [("down", down_kbps), ("up", up_kbps)] {
            match limit {
                Some(kbps) if kbps > 0 => {
                    args.insert(format!("speed-limit-{dir}"), json!(kbps));
                    args.insert(format!("speed-limit-{dir}-enabled"), json!(true));
                }
                _ => {
                    args.insert(format!("speed-limit-{dir}-enabled"), json!(false));
                }
            }
        }
        let mut session = self.session.lock().await;
        self.rpc(&mut session, "session-set", Value::Object(args))
            .await
            .map(drop)
    }
}

fn protocol(e: impl std::fmt::Display) -> ClientError {
    ClientError::Protocol(e.to_string())
}

fn parse_reply(body: &[u8]) -> Result<Value> {
    #[derive(Deserialize)]
    struct Reply {
        result: String,
        #[serde(default)]
        arguments: Value,
    }
    let reply: Reply = serde_json::from_slice(body).map_err(protocol)?;
    if reply.result == "success" {
        Ok(reply.arguments)
    } else {
        Err(ClientError::Protocol(reply.result))
    }
}

fn torrent_id(entry: &Value) -> Result<ClientTorrentId> {
    let hash = entry
        .get("hashString")
        .and_then(Value::as_str)
        .ok_or_else(|| ClientError::Protocol("added torrent without hashString".into()))?;
    Ok(ClientTorrentId::new(hash.to_ascii_lowercase()))
}

fn complement(wanted: &BTreeSet<u32>, file_count: usize) -> Vec<u32> {
    (0u32..)
        .take(file_count)
        .filter(|i| !wanted.contains(i))
        .collect()
}

fn check_indices(wanted: &BTreeSet<u32>, file_count: usize) -> Result<()> {
    match wanted.last() {
        Some(&index) if usize::try_from(index).unwrap_or(usize::MAX) >= file_count => {
            Err(ClientError::FileIndex { index, file_count })
        }
        _ => Ok(()),
    }
}

/// A flag Transmission sends as a boolean or, in older versions, as 0 or 1.
#[derive(Deserialize)]
#[serde(untagged)]
enum Flag {
    Bool(bool),
    Int(i64),
}

impl Flag {
    fn into_bool(self) -> bool {
        match self {
            Flag::Bool(b) => b,
            Flag::Int(i) => i != 0,
        }
    }
}

#[derive(Deserialize)]
struct WantedOnly {
    #[serde(default)]
    wanted: Vec<Flag>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawTorrent {
    hash_string: String,
    status: i64,
    #[serde(default)]
    error: i64,
    #[serde(default)]
    error_string: String,
    #[serde(default)]
    files: Vec<RawFile>,
    #[serde(default)]
    file_stats: Vec<RawFileStat>,
    #[serde(default)]
    rate_download: u64,
    #[serde(default)]
    rate_upload: u64,
    #[serde(default)]
    upload_ratio: f64,
    #[serde(default)]
    is_finished: bool,
}

#[derive(Deserialize)]
struct RawFile {
    length: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawFileStat {
    bytes_completed: u64,
    wanted: Flag,
}

impl RawTorrent {
    fn into_status(self) -> Result<TorrentStatus> {
        let infohash = InfoHash::from_hex(&self.hash_string)
            .ok_or_else(|| protocol(format!("bad hashString {:?}", self.hash_string)))?;
        if self.files.len() != self.file_stats.len() {
            return Err(protocol("files and fileStats differ in length"));
        }
        let files = self
            .files
            .into_iter()
            .zip(self.file_stats)
            .zip(0u32..)
            .map(|((file, stat), index)| FileProgress {
                index,
                bytes_done: stat.bytes_completed,
                size: file.length,
                wanted: stat.wanted.into_bool(),
            })
            .collect();
        let state = if self.error == LOCAL_ERROR {
            TorrentState::Error(self.error_string)
        } else {
            match self.status {
                0 => TorrentState::Stopped,
                1 | 2 => TorrentState::Checking,
                3 | 5 => TorrentState::Queued,
                4 => TorrentState::Downloading,
                6 => TorrentState::Seeding,
                other => return Err(protocol(format!("unknown torrent status {other}"))),
            }
        };
        // A ratio needs no f64 precision.
        #[allow(clippy::cast_possible_truncation)]
        let ratio = match self.upload_ratio {
            r if r >= 0.0 => r as f32,
            // Transmission sends -2 for an infinite ratio and -1 when not applicable.
            r if (r + 2.0).abs() < f64::EPSILON => f32::INFINITY,
            _ => 0.0,
        };
        Ok(TorrentStatus {
            infohash,
            state,
            files,
            ratio,
            down_rate: self.rate_download,
            up_rate: self.rate_upload,
            is_finished: self.is_finished,
        })
    }
}

#[cfg(test)]
mod tests;
