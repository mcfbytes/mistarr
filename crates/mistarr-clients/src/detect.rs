//! Client detection; see `docs/DOWNLOAD-CLIENTS.md` "Detection".

use std::path::PathBuf;
use std::time::Duration;

use hyper::body::Bytes;
use tokio::net::TcpStream;

use crate::http::{self, Endpoint, Headers};
use crate::{ClientKind, Transmission};

/// rtorrent's conventional SCGI TCP address.
pub const DEFAULT_RTORRENT_TCP: &str = "127.0.0.1:5000";

/// The SCGI socket named in the rc mistarr generates for rtorrent.
pub const DEFAULT_RTORRENT_SOCKET: &str = "/media/fat/mistarr/rtorrent.sock";

/// Inputs to [`detect`]: the `[client]` config plus the default addresses,
/// which tests override.
///
/// ```
/// use mistarr_clients::detect::DetectConfig;
/// let cfg = DetectConfig::default();
/// assert!(cfg.kind.is_none() && cfg.url.is_none());
/// ```
#[derive(Debug, Clone)]
pub struct DetectConfig {
    /// `client.kind`; `None` means `auto`.
    pub kind: Option<crate::ClientKind>,
    /// `client.url`; `None` when empty.
    pub url: Option<String>,
    /// Transmission RPC URL probed in step 2.
    pub transmission_url: String,
    /// rtorrent SCGI TCP address probed in step 3.
    pub rtorrent_tcp: String,
    /// rtorrent SCGI unix socket probed last in step 3.
    pub rtorrent_socket: PathBuf,
    /// Time allowed for each probe.
    pub timeout: Duration,
}

impl Default for DetectConfig {
    fn default() -> Self {
        Self {
            kind: None,
            url: None,
            transmission_url: Transmission::DEFAULT_URL.to_owned(),
            rtorrent_tcp: DEFAULT_RTORRENT_TCP.to_owned(),
            rtorrent_socket: PathBuf::from(DEFAULT_RTORRENT_SOCKET),
            timeout: Duration::from_secs(2),
        }
    }
}

/// The client detection settled on and where to reach it.
///
/// ```
/// use mistarr_clients::{ClientKind, detect::Detected};
/// let d = Detected { kind: ClientKind::Rtorrent, url: "127.0.0.1:5000".into() };
/// assert_eq!(d.kind.as_str(), "rtorrent");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detected {
    /// Which client.
    pub kind: ClientKind,
    /// A Transmission RPC URL, or an rtorrent SCGI address in a form
    /// [`ScgiAddr::parse`] accepts.
    pub url: String,
}

/// An rtorrent SCGI address: `host:port`, `scgi://host:port`, an absolute
/// socket path, or `scgi:///path`.
///
/// ```
/// use std::path::PathBuf;
/// use mistarr_clients::detect::ScgiAddr;
/// assert_eq!(ScgiAddr::parse("scgi://127.0.0.1:5000"), Some(ScgiAddr::Tcp("127.0.0.1:5000".into())));
/// assert_eq!(ScgiAddr::parse("/run/rt.sock"), Some(ScgiAddr::Unix(PathBuf::from("/run/rt.sock"))));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScgiAddr {
    /// A TCP `host:port`.
    Tcp(String),
    /// A unix socket path.
    Unix(PathBuf),
}

impl ScgiAddr {
    /// Parses an address; `None` for an empty string or an `http://` URL.
    ///
    /// ```
    /// use mistarr_clients::detect::ScgiAddr;
    /// assert_eq!(ScgiAddr::parse("http://127.0.0.1:9091/"), None);
    /// ```
    #[must_use]
    pub fn parse(addr: &str) -> Option<Self> {
        let addr = addr.trim();
        let bare = addr.strip_prefix("scgi://").unwrap_or(addr);
        if bare.is_empty() || bare.contains("://") {
            None
        } else if bare.starts_with('/') {
            Some(Self::Unix(PathBuf::from(bare)))
        } else if bare.contains(':') {
            Some(Self::Tcp(bare.to_owned()))
        } else {
            None
        }
    }
}

/// Runs steps 1 to 3 of the detection order and returns the first client
/// that answers, or `None`. A configured kind with a url is taken without
/// probing; a configured kind without one probes only that kind's defaults.
///
/// ```no_run
/// use mistarr_clients::detect::{detect, DetectConfig};
/// # async fn f() {
/// if let Some(found) = detect(&DetectConfig::default()).await {
///     println!("{} at {}", found.kind, found.url);
/// }
/// # }
/// ```
pub async fn detect(cfg: &DetectConfig) -> Option<Detected> {
    let url = cfg.url.as_deref().map(str::trim).filter(|u| !u.is_empty());
    if let (Some(kind), Some(url)) = (cfg.kind, url) {
        return Some(Detected {
            kind,
            url: url.to_owned(),
        });
    }
    let want = |k: ClientKind| cfg.kind.is_none() || cfg.kind == Some(k);

    if want(ClientKind::Transmission) {
        let rpc = url
            .filter(|u| u.starts_with("http://"))
            .unwrap_or(&cfg.transmission_url);
        if probe_transmission(rpc, cfg.timeout).await {
            return Some(Detected {
                kind: ClientKind::Transmission,
                url: rpc.to_owned(),
            });
        }
    }
    if want(ClientKind::Rtorrent) {
        let socket = cfg.rtorrent_socket.to_string_lossy();
        let candidates = url
            .filter(|u| ScgiAddr::parse(u).is_some())
            .into_iter()
            .chain([cfg.rtorrent_tcp.as_str(), socket.as_ref()]);
        for candidate in candidates {
            if probe_scgi(candidate, cfg.timeout).await {
                return Some(Detected {
                    kind: ClientKind::Rtorrent,
                    url: candidate.to_owned(),
                });
            }
        }
    }
    None
}

/// True if `url` answers like Transmission: a 409 carrying
/// `X-Transmission-Session-Id`, or a 401 whose challenge names Transmission.
///
/// ```no_run
/// # async fn f() {
/// use std::time::Duration;
/// use mistarr_clients::{detect::probe_transmission, Transmission};
/// let alive = probe_transmission(Transmission::DEFAULT_URL, Duration::from_secs(2)).await;
/// # }
/// ```
pub async fn probe_transmission(url: &str, timeout: Duration) -> bool {
    let Ok(endpoint) = Endpoint::parse(url) else {
        return false;
    };
    let body = Bytes::from_static(br#"{"method":"session-get"}"#);
    match http::post(&endpoint, Headers::default(), body, timeout).await {
        Ok(resp) => match resp.status {
            409 => resp.session_id.is_some(),
            401 => resp
                .www_authenticate
                .is_some_and(|c| c.contains("Transmission")),
            _ => false,
        },
        Err(_) => false,
    }
}

/// True if a connection to the SCGI address `addr` opens within `timeout`.
/// Nothing is sent; the full rtorrent client does the talking.
///
/// ```no_run
/// # async fn f() {
/// use std::time::Duration;
/// let alive = mistarr_clients::detect::probe_scgi("127.0.0.1:5000", Duration::from_secs(2)).await;
/// # }
/// ```
pub async fn probe_scgi(addr: &str, timeout: Duration) -> bool {
    let connect = async {
        match ScgiAddr::parse(addr) {
            Some(ScgiAddr::Tcp(a)) => TcpStream::connect(a).await.is_ok(),
            #[cfg(unix)]
            Some(ScgiAddr::Unix(p)) => tokio::net::UnixStream::connect(p).await.is_ok(),
            _ => false,
        }
    };
    tokio::time::timeout(timeout, connect)
        .await
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeResponse, FakeServer};
    use tokio::net::TcpListener;

    async fn closed_addr() -> String {
        let l = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = l.local_addr().expect("addr").to_string();
        drop(l);
        addr
    }

    fn missing_socket() -> PathBuf {
        std::env::temp_dir().join(format!("mistarr-missing-{}.sock", std::process::id()))
    }

    async fn quiet_config() -> DetectConfig {
        DetectConfig {
            transmission_url: format!("http://{}/transmission/rpc", closed_addr().await),
            rtorrent_tcp: closed_addr().await,
            rtorrent_socket: missing_socket(),
            timeout: Duration::from_secs(2),
            ..DetectConfig::default()
        }
    }

    #[tokio::test]
    async fn configured_kind_and_url_skip_probing() {
        let cfg = DetectConfig {
            kind: Some(ClientKind::Rtorrent),
            url: Some("/nowhere.sock".into()),
            ..quiet_config().await
        };
        let got = detect(&cfg).await;
        assert_eq!(
            got,
            Some(Detected {
                kind: ClientKind::Rtorrent,
                url: "/nowhere.sock".into()
            })
        );
    }

    #[tokio::test]
    async fn transmission_is_probed_first() {
        let fake = FakeServer::start().await.expect("bind");
        fake.push(FakeResponse::session_conflict("sid"));
        let rt = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let cfg = DetectConfig {
            transmission_url: fake.url(),
            rtorrent_tcp: rt.local_addr().expect("addr").to_string(),
            ..quiet_config().await
        };
        let got = detect(&cfg).await.expect("found");
        assert_eq!(got.kind, ClientKind::Transmission);
        assert_eq!(got.url, fake.url());
        assert_eq!(fake.requests().len(), 1);
    }

    #[tokio::test]
    async fn non_transmission_http_is_not_detected() {
        let fake = FakeServer::start().await.expect("bind");
        fake.push(FakeResponse::new(409));
        assert!(!probe_transmission(&fake.url(), Duration::from_secs(2)).await);
        fake.push(
            FakeResponse::new(401).with_header("WWW-Authenticate", "Basic realm=\"Transmission\""),
        );
        assert!(probe_transmission(&fake.url(), Duration::from_secs(2)).await);
        assert!(!probe_transmission("not a url", Duration::from_secs(2)).await);
    }

    #[tokio::test]
    async fn rtorrent_tcp_when_transmission_is_silent() {
        let rt = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = rt.local_addr().expect("addr").to_string();
        let cfg = DetectConfig {
            rtorrent_tcp: addr.clone(),
            ..quiet_config().await
        };
        let got = detect(&cfg).await.expect("found");
        assert_eq!(
            got,
            Detected {
                kind: ClientKind::Rtorrent,
                url: addr
            }
        );
    }

    #[tokio::test]
    async fn configured_scgi_url_is_tried_before_defaults() {
        let first = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let second = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let url = format!("scgi://{}", first.local_addr().expect("addr"));
        let cfg = DetectConfig {
            url: Some(url.clone()),
            rtorrent_tcp: second.local_addr().expect("addr").to_string(),
            ..quiet_config().await
        };
        assert_eq!(detect(&cfg).await.map(|d| d.url), Some(url));
    }

    #[tokio::test]
    async fn configured_http_url_replaces_default_transmission_url() {
        let fake = FakeServer::start().await.expect("bind");
        fake.push(FakeResponse::session_conflict("sid"));
        let cfg = DetectConfig {
            url: Some(fake.url()),
            ..quiet_config().await
        };
        let got = detect(&cfg).await.expect("found");
        assert_eq!(
            got,
            Detected {
                kind: ClientKind::Transmission,
                url: fake.url()
            }
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rtorrent_unix_socket_is_tried_last() {
        let path = std::env::temp_dir().join(format!("mistarr-detect-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let _listener = tokio::net::UnixListener::bind(&path).expect("bind socket");
        let cfg = DetectConfig {
            rtorrent_socket: path.clone(),
            ..quiet_config().await
        };
        let got = detect(&cfg).await;
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            got,
            Some(Detected {
                kind: ClientKind::Rtorrent,
                url: path.to_string_lossy().into_owned()
            })
        );
    }

    #[tokio::test]
    async fn configured_kind_limits_probing_to_that_kind() {
        let fake = FakeServer::start().await.expect("bind");
        fake.push(FakeResponse::session_conflict("sid"));
        let cfg = DetectConfig {
            kind: Some(ClientKind::Rtorrent),
            transmission_url: fake.url(),
            ..quiet_config().await
        };
        assert_eq!(detect(&cfg).await, None);
        assert!(fake.requests().is_empty());
    }

    #[tokio::test]
    async fn nothing_answering_gives_none() {
        assert_eq!(detect(&quiet_config().await).await, None);
    }

    #[test]
    fn scgi_addresses_parse() {
        assert_eq!(
            ScgiAddr::parse("127.0.0.1:5000"),
            Some(ScgiAddr::Tcp("127.0.0.1:5000".into()))
        );
        assert_eq!(
            ScgiAddr::parse("scgi:///a/b.sock"),
            Some(ScgiAddr::Unix(PathBuf::from("/a/b.sock")))
        );
        assert_eq!(ScgiAddr::parse(""), None);
        assert_eq!(ScgiAddr::parse("localhost"), None);
    }

    #[tokio::test]
    async fn probe_scgi_reports_connectability() {
        let l = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let open = l.local_addr().expect("addr").to_string();
        assert!(probe_scgi(&open, Duration::from_secs(2)).await);
        assert!(!probe_scgi(&closed_addr().await, Duration::from_secs(2)).await);
        assert!(!probe_scgi("garbage", Duration::from_secs(2)).await);
    }
}
