//! rtorrent over XML-RPC on SCGI; see `docs/DOWNLOAD-CLIENTS.md` "rtorrent".

use std::collections::{BTreeSet, HashMap};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::detect::ScgiAddr;
use crate::xmlrpc::{self, Fault, MethodResponse, Value};
use crate::{
    metainfo, scgi, ClientError, ClientInfo, ClientKind, ClientTorrentId, DownloadClient,
    FileProgress, InfoHash, RemotePathMap, Result, SeedPolicy, TorrentSource, TorrentState,
    TorrentStatus,
};

/// The rc mistarr writes when it starts rtorrent itself; the same text as in
/// `docs/DOWNLOAD-CLIENTS.md` "Detection".
const RECOMMENDED_RC: &str = "\
directory.default.set = /media/fat/mistarr/staging
session.path.set = /media/fat/mistarr/rtorrent-session
network.scgi.open_local = /media/fat/mistarr/rtorrent.sock
network.xmlrpc.size_limit.set = 8M
dht.mode.set = auto
protocol.pex.set = yes
throttle.global_down.max_rate.set_kb = 0
throttle.global_up.max_rate.set_kb = 0
system.daemon.set = true
";

/// Status commands sent per torrent, in the order [`RawStatus`] reads them.
const STATUS_COMMANDS: [&str; 10] = [
    "d.state",
    "d.is_active",
    "d.complete",
    "d.is_hash_checking",
    "d.hashing",
    "d.ratio",
    "d.down.rate",
    "d.up.rate",
    "d.message",
    "d.is_meta",
];

/// The rc text for an rtorrent that mistarr launches itself, including the
/// raised `network.xmlrpc.size_limit.set` that a user's own rtorrent may lack.
///
/// ```
/// let rc = mistarr_clients::rtorrent::recommended_rc();
/// assert!(rc.contains("network.scgi.open_local = /media/fat/mistarr/rtorrent.sock"));
/// ```
#[must_use]
pub fn recommended_rc() -> &'static str {
    RECOMMENDED_RC
}

/// An rtorrent reached over SCGI at a TCP address or unix socket.
///
/// Calls are serialised through one lock, which also guards the seed policy
/// per torrent. rtorrent has no per-torrent ratio limit, so [`DownloadClient::status`]
/// stops a seeding torrent once `d.ratio` reaches its policy, and reports a
/// stopped one that has reached it as finished. Policies live in
/// memory only; call [`DownloadClient::set_seed_policy`] again after a restart.
///
/// ```
/// use std::time::Duration;
/// use mistarr_clients::{PathMapping, RemotePathMap, Rtorrent};
/// let rt = Rtorrent::new("/media/fat/mistarr/rtorrent.sock")?
///     .with_timeout(Duration::from_secs(10))
///     .with_path_map(RemotePathMap::new(vec![PathMapping::new("/srv", "/media/fat")]));
/// # Ok::<(), mistarr_clients::ClientError>(())
/// ```
#[derive(Debug)]
pub struct Rtorrent {
    addr: ScgiAddr,
    timeout: Duration,
    path_map: RemotePathMap,
    torrents: Mutex<HashMap<InfoHash, SeedPolicy>>,
}

impl Rtorrent {
    /// Default time allowed for one RPC, connect to last byte.
    ///
    /// ```
    /// assert_eq!(mistarr_clients::Rtorrent::DEFAULT_TIMEOUT.as_secs(), 30);
    /// ```
    pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

    /// Most commands sent in one `system.multicall`, which keeps each request
    /// under rtorrent's default XML-RPC size limit.
    ///
    /// ```
    /// assert_eq!(mistarr_clients::Rtorrent::MULTICALL_CHUNK, 500);
    /// ```
    pub const MULTICALL_CHUNK: usize = 500;

    /// A client for the SCGI address `addr`, in any form
    /// [`ScgiAddr::parse`] accepts. No request is made until the first operation.
    ///
    /// # Errors
    /// [`ClientError::Protocol`] if `addr` is not an SCGI address.
    ///
    /// ```
    /// use mistarr_clients::Rtorrent;
    /// assert!(Rtorrent::new("scgi://127.0.0.1:5000").is_ok());
    /// assert!(Rtorrent::new("http://127.0.0.1:9091/").is_err());
    /// ```
    pub fn new(addr: &str) -> Result<Self> {
        let addr = ScgiAddr::parse(addr)
            .ok_or_else(|| ClientError::Protocol(format!("invalid SCGI address {addr:?}")))?;
        Ok(Self {
            addr,
            timeout: Self::DEFAULT_TIMEOUT,
            path_map: RemotePathMap::default(),
            torrents: Mutex::new(HashMap::new()),
        })
    }

    /// Sets the time allowed for one RPC.
    ///
    /// ```
    /// use std::time::Duration;
    /// let rt = mistarr_clients::Rtorrent::new("127.0.0.1:5000")?.with_timeout(Duration::from_secs(5));
    /// # Ok::<(), mistarr_clients::ClientError>(())
    /// ```
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Sets the map applied to paths rtorrent reports before mistarr deletes
    /// data in [`DownloadClient::remove`].
    ///
    /// ```
    /// let rt = mistarr_clients::Rtorrent::new("127.0.0.1:5000")?
    ///     .with_path_map(mistarr_clients::RemotePathMap::default());
    /// # Ok::<(), mistarr_clients::ClientError>(())
    /// ```
    #[must_use]
    pub fn with_path_map(mut self, path_map: RemotePathMap) -> Self {
        self.path_map = path_map;
        self
    }

    /// Sends one XML-RPC call. Callers hold the `torrents` lock.
    async fn call(&self, method: &str, params: &[Value]) -> Result<Value> {
        let body = xmlrpc::encode_call(method, params);
        let reply = scgi::request(&self.addr, &body, self.timeout).await?;
        match xmlrpc::decode_response(&reply).map_err(protocol)? {
            MethodResponse::Success(v) => Ok(v),
            MethodResponse::Fault(f) => Err(fault_error(&f)),
        }
    }

    /// Sends `calls` as one `system.multicall`; the first fault fails the whole.
    async fn multicall(&self, calls: Vec<(&str, Vec<Value>)>) -> Result<Vec<Value>> {
        let count = calls.len();
        let list = calls
            .into_iter()
            .map(|(method, params)| {
                Value::Struct(vec![
                    ("methodName".into(), Value::from(method)),
                    ("params".into(), Value::Array(params)),
                ])
            })
            .collect();
        let Value::Array(results) = self.call("system.multicall", &[Value::Array(list)]).await?
        else {
            return Err(protocol("system.multicall reply is not an array"));
        };
        if results.len() != count {
            return Err(protocol("system.multicall reply has the wrong length"));
        }
        results
            .into_iter()
            .map(|entry| match entry {
                Value::Array(mut one) if one.len() == 1 => Ok(one.remove(0)),
                other => Err(Fault::from_value(&other).map_or_else(
                    || protocol("malformed system.multicall entry"),
                    |f| fault_error(&f),
                )),
            })
            .collect()
    }

    /// The file count, or `None` while a magnet has no metadata.
    async fn file_count(&self, target: &str) -> Result<Option<usize>> {
        let r = self
            .multicall(vec![
                ("d.is_meta", vec![target.into()]),
                ("d.size_files", vec![target.into()]),
            ])
            .await?;
        if int(&r[0])? != 0 {
            return Ok(None);
        }
        let count = usize::try_from(int(&r[1])?).map_err(protocol)?;
        Ok(Some(count))
    }

    /// Sets every file's priority to normal if wanted, else off, in
    /// multicall chunks, then `d.update_priorities`.
    async fn apply_selection(
        &self,
        target: &str,
        file_count: usize,
        wanted: &BTreeSet<u32>,
    ) -> Result<()> {
        let mut start = 0;
        while start < file_count {
            let end = file_count.min(start + Self::MULTICALL_CHUNK);
            let calls = (start..end)
                .map(|i| {
                    let on = u32::try_from(i).is_ok_and(|i| wanted.contains(&i));
                    let params = vec![Value::from(format!("{target}:f{i}")), i64::from(on).into()];
                    ("f.priority.set", params)
                })
                .collect();
            self.multicall(calls).await?;
            start = end;
        }
        self.call("d.update_priorities", &[target.into()])
            .await
            .map(drop)
    }
}

#[async_trait]
impl DownloadClient for Rtorrent {
    async fn probe(&self) -> Result<ClientInfo> {
        let _guard = self.torrents.lock().await;
        let version = self.call("system.client_version", &[]).await?;
        let version = version
            .as_str()
            .ok_or_else(|| protocol("system.client_version is not a string"))?;
        Ok(ClientInfo {
            kind: ClientKind::Rtorrent,
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
            .ok_or_else(|| protocol("download dir is not UTF-8"))?;
        let (hash, known_count, method, data) = match src {
            TorrentSource::Metainfo(bytes) => {
                let hash = metainfo::info_hash(&bytes)
                    .ok_or_else(|| protocol("metainfo has no info dictionary"))?;
                let count = metainfo::file_count(&bytes);
                (hash, count, "load.raw", Value::Base64(bytes))
            }
            TorrentSource::Magnet(uri) => {
                let hash =
                    magnet_hash(&uri).ok_or_else(|| protocol("magnet without a btih hash"))?;
                (hash, None, "load.normal", Value::String(uri))
            }
        };
        if let Some(count) = known_count {
            check_indices(&wanted, count)?;
        }
        let target = target(&hash);

        let mut torrents = self.torrents.lock().await;
        let fresh = match self.call("d.hash", &[target.as_str().into()]).await {
            Ok(_) => false,
            Err(ClientError::NotFound) => true,
            Err(e) => return Err(e),
        };
        if fresh {
            // Trailing load commands are replayed when a magnet's metadata
            // replaces the meta-download; load.raw and load.normal leave it stopped.
            let replay = directory_command(dir);
            self.call(method, &["".into(), data, replay.into()]).await?;
            self.call("d.directory.set", &[target.as_str().into(), dir.into()])
                .await
                .map_err(not_loaded)?;
        }
        let count = match known_count.filter(|_| fresh) {
            Some(count) => Some(count),
            None => self.file_count(&target).await.map_err(not_loaded)?,
        };
        if let Some(count) = count {
            check_indices(&wanted, count)?;
            self.apply_selection(&target, count, &wanted).await?;
        }
        torrents.insert(hash, seed);
        Ok(ClientTorrentId::new(hash.to_string()))
    }

    async fn set_wanted(&self, id: &ClientTorrentId, wanted: &[u32]) -> Result<()> {
        let wanted: BTreeSet<u32> = wanted.iter().copied().collect();
        let target = target(&parse_id(id)?);
        let _guard = self.torrents.lock().await;
        let count = self
            .file_count(&target)
            .await?
            .ok_or(ClientError::MetadataPending)?;
        check_indices(&wanted, count)?;
        self.apply_selection(&target, count, &wanted).await
    }

    async fn set_seed_policy(&self, id: &ClientTorrentId, seed: SeedPolicy) -> Result<()> {
        let hash = parse_id(id)?;
        let mut torrents = self.torrents.lock().await;
        self.call("d.hash", &[target(&hash).into()]).await?;
        torrents.insert(hash, seed);
        Ok(())
    }

    async fn start(&self, id: &ClientTorrentId) -> Result<()> {
        let hash = parse_id(id)?;
        let _guard = self.torrents.lock().await;
        self.call("d.start", &[target(&hash).into()])
            .await
            .map(drop)
    }

    async fn stop(&self, id: &ClientTorrentId) -> Result<()> {
        let hash = parse_id(id)?;
        let _guard = self.torrents.lock().await;
        self.call("d.stop", &[target(&hash).into()]).await.map(drop)
    }

    async fn status(&self, id: &ClientTorrentId) -> Result<TorrentStatus> {
        let hash = parse_id(id)?;
        let target = target(&hash);
        let torrents = self.torrents.lock().await;
        let mut calls: Vec<(&str, Vec<Value>)> = STATUS_COMMANDS
            .iter()
            .map(|&c| (c, vec![target.as_str().into()]))
            .collect();
        calls.push((
            "f.multicall",
            vec![
                target.as_str().into(),
                "".into(),
                "f.size_bytes=".into(),
                "f.completed_chunks=".into(),
                "f.size_chunks=".into(),
                "f.priority=".into(),
            ],
        ));
        let raw = RawStatus::read(&self.multicall(calls).await?)?;
        let mut state = raw.state();
        let passes = torrents.get(&hash).is_some_and(|p| raw.passes(p));
        if passes && state == TorrentState::Seeding {
            self.call("d.stop", &[target.as_str().into()]).await?;
            state = TorrentState::Stopped;
        }
        let finished = passes && state == TorrentState::Stopped && raw.wanted_done();
        Ok(TorrentStatus {
            infohash: hash,
            state,
            files: raw.files,
            ratio: raw.ratio,
            down_rate: raw.down_rate,
            up_rate: raw.up_rate,
            is_finished: finished,
        })
    }

    async fn remove(&self, id: &ClientTorrentId, delete_data: bool) -> Result<()> {
        let hash = parse_id(id)?;
        let target = target(&hash);
        let mut torrents = self.torrents.lock().await;
        let data = if delete_data {
            let r = self
                .multicall(vec![
                    ("d.directory", vec![target.as_str().into()]),
                    ("d.is_multi_file", vec![target.as_str().into()]),
                    (
                        "f.multicall",
                        vec![target.as_str().into(), "".into(), "f.path=".into()],
                    ),
                ])
                .await?;
            Some(DataLayout::read(&r, &self.path_map)?)
        } else {
            None
        };
        // Files go before the erase, so a failed erase leaves a torrent whose
        // file list a retry can still read.
        let deleted = match data {
            Some(layout) => tokio::task::spawn_blocking(move || layout.delete())
                .await
                .map_err(|e| ClientError::Io(io::Error::other(e)))?,
            None => Ok(()),
        };
        self.call("d.erase", &[target.as_str().into()]).await?;
        torrents.remove(&hash);
        deleted.map_err(ClientError::Io)
    }

    async fn set_rate_limits(&self, down_kbps: Option<u32>, up_kbps: Option<u32>) -> Result<()> {
        let _guard = self.torrents.lock().await;
        for (method, limit) in [
            ("throttle.global_down.max_rate.set_kb", down_kbps),
            ("throttle.global_up.max_rate.set_kb", up_kbps),
        ] {
            let kb = i64::from(limit.unwrap_or(0));
            self.call(method, &["".into(), kb.into()]).await?;
        }
        Ok(())
    }
}

fn protocol(e: impl std::fmt::Display) -> ClientError {
    ClientError::Protocol(e.to_string())
}

/// rtorrent reports an unknown hash as a fault naming the info-hash.
fn fault_error(fault: &Fault) -> ClientError {
    if fault.message.contains("Could not find info-hash") {
        ClientError::NotFound
    } else {
        ClientError::Protocol(format!("fault {}: {}", fault.code, fault.message))
    }
}

/// After a load, a missing torrent means rtorrent rejected the data.
fn not_loaded(e: ClientError) -> ClientError {
    match e {
        ClientError::NotFound => protocol("rtorrent did not load the torrent"),
        other => other,
    }
}

fn int(v: &Value) -> Result<i64> {
    v.as_i64()
        .ok_or_else(|| protocol(format!("expected an integer, got {v:?}")))
}

fn uint(v: &Value) -> Result<u64> {
    u64::try_from(int(v)?).map_err(protocol)
}

fn text(v: &Value) -> Result<String> {
    v.as_str()
        .map(str::to_owned)
        .ok_or_else(|| protocol(format!("expected a string, got {v:?}")))
}

/// rtorrent addresses a download by its uppercase hex infohash.
fn target(hash: &InfoHash) -> String {
    hash.to_string().to_ascii_uppercase()
}

fn parse_id(id: &ClientTorrentId) -> Result<InfoHash> {
    InfoHash::from_hex(id.as_str()).ok_or(ClientError::NotFound)
}

fn check_indices(wanted: &BTreeSet<u32>, file_count: usize) -> Result<()> {
    match wanted.last() {
        Some(&index) if usize::try_from(index).unwrap_or(usize::MAX) >= file_count => {
            Err(ClientError::FileIndex { index, file_count })
        }
        _ => Ok(()),
    }
}

/// The infohash in a magnet's `xt=urn:btih:` parameter, hex or base32.
fn magnet_hash(uri: &str) -> Option<InfoHash> {
    let query = uri.strip_prefix("magnet:?")?;
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        if key != "xt" {
            return None;
        }
        let value = percent_decode(value)?;
        let prefix = value.get(..9)?;
        if !prefix.eq_ignore_ascii_case("urn:btih:") {
            return None;
        }
        let hash = &value[9..];
        InfoHash::from_hex(hash).or_else(|| base32_hash(hash))
    })
}

/// Decodes `%XX` escapes; `None` for a malformed escape or non-UTF-8 result.
fn percent_decode(s: &str) -> Option<String> {
    let mut out = Vec::with_capacity(s.len());
    let mut bytes = s.bytes();
    while let Some(b) = bytes.next() {
        if b == b'%' {
            let hi = char::from(bytes.next()?).to_digit(16)?;
            let lo = char::from(bytes.next()?).to_digit(16)?;
            out.push(u8::try_from(hi << 4 | lo).ok()?);
        } else {
            out.push(b);
        }
    }
    String::from_utf8(out).ok()
}

/// A `d.directory.set` command for the trailing arguments of `load.*`, with
/// the path quoted for rtorrent's command parser.
fn directory_command(dir: &str) -> String {
    let mut quoted = String::with_capacity(dir.len() + 2);
    for c in dir.chars() {
        if matches!(c, '"' | '\\') {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    format!("d.directory.set=\"{quoted}\"")
}

fn base32_hash(s: &str) -> Option<InfoHash> {
    if s.len() != 32 {
        return None;
    }
    let mut out = [0u8; 20];
    let mut acc: u32 = 0;
    let mut bits = 0;
    let mut pos = 0;
    for c in s.bytes() {
        let v = match c.to_ascii_uppercase() {
            c @ b'A'..=b'Z' => c - b'A',
            c @ b'2'..=b'7' => c - b'2' + 26,
            _ => return None,
        };
        acc = (acc << 5 | u32::from(v)) & 0xfff;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out[pos] = u8::try_from((acc >> bits) & 0xff).ok()?;
            pos += 1;
        }
    }
    Some(InfoHash::from_bytes(out))
}

/// The reply to the status multicall: [`STATUS_COMMANDS`] then `f.multicall`.
struct RawStatus {
    open_state: i64,
    active: bool,
    complete: bool,
    hashing: bool,
    ratio: f32,
    down_rate: u64,
    up_rate: u64,
    message: String,
    files: Vec<FileProgress>,
}

impl RawStatus {
    fn read(r: &[Value]) -> Result<Self> {
        let [state, active, complete, checking, hashing, ratio, down, up, message, meta, files] = r
        else {
            return Err(protocol("status reply has the wrong length"));
        };
        let files = if int(meta)? == 0 {
            read_files(files)?
        } else {
            Vec::new()
        };
        // d.ratio is in thousandths; f32 is ample for a ratio.
        #[allow(clippy::cast_precision_loss)]
        let ratio = int(ratio)? as f32 / 1000.0;
        Ok(Self {
            open_state: int(state)?,
            active: int(active)? != 0,
            complete: int(complete)? != 0,
            // d.hashing is non-zero while queued for a check, before it starts.
            hashing: int(checking)? != 0 || int(hashing)? != 0,
            ratio,
            down_rate: uint(down)?,
            up_rate: uint(up)?,
            message: text(message)?,
            files,
        })
    }

    /// Every file the user wants is present, which is when rtorrent seeds.
    fn wanted_done(&self) -> bool {
        self.complete
            || (!self.files.is_empty()
                && self
                    .files
                    .iter()
                    .filter(|f| f.wanted)
                    .all(FileProgress::is_complete))
    }

    fn state(&self) -> TorrentState {
        if self.hashing {
            TorrentState::Checking
        } else if !self.active && !self.message.is_empty() && !self.message.starts_with("Tracker:")
        {
            TorrentState::Error(self.message.clone())
        } else if self.open_state == 0 || !self.active {
            TorrentState::Stopped
        } else if self.wanted_done() {
            TorrentState::Seeding
        } else {
            TorrentState::Downloading
        }
    }

    fn passes(&self, policy: &SeedPolicy) -> bool {
        match policy {
            SeedPolicy::None => true,
            SeedPolicy::Ratio { ratio } => self.ratio >= *ratio,
            SeedPolicy::Client => false,
        }
    }
}

/// Per-file progress from `f.multicall` rows of size, completed chunks,
/// chunk count and priority. Bytes are prorated by chunks, so a file reads
/// complete only when all its chunks are.
fn read_files(v: &Value) -> Result<Vec<FileProgress>> {
    let rows = v
        .as_array()
        .ok_or_else(|| protocol("f.multicall reply is not an array"))?;
    rows.iter()
        .zip(0u32..)
        .map(|(row, index)| {
            let [size, done, chunks, priority] = row.as_array().unwrap_or_default() else {
                return Err(protocol("malformed f.multicall row"));
            };
            let size = uint(size)?;
            let (done, chunks) = (uint(done)?, uint(chunks)?);
            let bytes_done = if done >= chunks {
                size
            } else {
                u64::try_from(u128::from(size) * u128::from(done) / u128::from(chunks))
                    .map_err(protocol)?
            };
            Ok(FileProgress {
                index,
                bytes_done,
                size,
                wanted: int(priority)? > 0,
            })
        })
        .collect()
}

/// Where a torrent's files sit on the local filesystem, for deletion.
struct DataLayout {
    directory: PathBuf,
    multi_file: bool,
    files: Vec<PathBuf>,
}

impl DataLayout {
    fn read(r: &[Value], map: &RemotePathMap) -> Result<Self> {
        let [directory, multi, files] = r else {
            return Err(protocol("remove reply has the wrong length"));
        };
        let directory = map.to_local(Path::new(&text(directory)?));
        let files = files
            .as_array()
            .ok_or_else(|| protocol("f.multicall reply is not an array"))?
            .iter()
            .map(|row| {
                let [path] = row.as_array().unwrap_or_default() else {
                    return Err(protocol("malformed f.multicall row"));
                };
                let rel = PathBuf::from(text(path)?);
                if rel.components().all(|c| matches!(c, Component::Normal(_))) {
                    Ok(rel)
                } else {
                    Err(protocol(format!("unsafe file path {}", rel.display())))
                }
            })
            .collect::<Result<_>>()?;
        if !directory.is_absolute() {
            return Err(protocol("download directory is not absolute"));
        }
        Ok(Self {
            directory,
            multi_file: int(multi)? != 0,
            files,
        })
    }

    /// Removes the torrent's files, then any directories they leave empty
    /// inside a multi-file torrent's own directory. Carries on past failures
    /// and returns the first.
    fn delete(self) -> io::Result<()> {
        let mut first_error = None;
        let mut dirs = BTreeSet::new();
        for rel in &self.files {
            match std::fs::remove_file(self.directory.join(rel)) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => {
                    first_error.get_or_insert(e);
                }
                _ => {}
            }
            let mut parent = rel.parent();
            while let Some(p) = parent.filter(|p| !p.as_os_str().is_empty()) {
                dirs.insert(p.to_path_buf());
                parent = p.parent();
            }
        }
        if self.multi_file {
            let mut dirs: Vec<PathBuf> = dirs.into_iter().collect();
            dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
            for d in dirs {
                // A directory still holding other files is left in place.
                let _ = std::fs::remove_dir(self.directory.join(d));
            }
            let _ = std::fs::remove_dir(&self.directory);
        }
        first_error.map_or(Ok(()), Err)
    }
}

#[cfg(test)]
mod tests;
