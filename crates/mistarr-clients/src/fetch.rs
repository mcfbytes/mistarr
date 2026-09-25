//! One GET of a URL the user supplied, streamed; the contract is `docs/ARCHITECTURE.md` "Fetching a URL".

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use http_body_util::{BodyExt, Empty};
use hyper::body::{Bytes, Incoming};
use hyper::header::{self, HeaderMap};
use hyper::{Request, StatusCode};
use hyper_util::rt::TokioIo;
use rustls::pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::task::JoinHandle;

mod disposition;
mod roots;
mod url;

pub use disposition::disposition_file_name;
pub use roots::{Roots, RootsOrigin, CERT_FILE_ENV, SYSTEM_BUNDLES};
use url::percent_decode_bytes;
pub use url::{percent_decode, FetchUrl};

#[cfg(test)]
mod tests;

/// Redirects of one request that are followed; one more fails the fetch.
pub const MAX_REDIRECTS: u8 = 5;

/// Longest a connection, with its TLS handshake, may take.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Longest the server may send nothing, for the answer's head or any part of its body.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Slowest average a body may arrive at, in bytes per second, once [`RATE_WINDOW`] has passed.
pub const MIN_RATE: u64 = 1024;

/// How long a body may take before [`MIN_RATE`] applies.
pub const RATE_WINDOW: Duration = Duration::from_secs(300);

/// Whether `ip` is on this machine or a local network: loopback, private, link-local,
/// shared (100.64/10), unique local or unspecified, also inside an IPv4-mapped address.
///
/// ```
/// use mistarr_clients::fetch::is_local;
/// assert!(is_local("192.168.1.5".parse().unwrap()));
/// assert!(is_local("::ffff:127.0.0.1".parse().unwrap()));
/// assert!(!is_local("192.0.2.1".parse().unwrap()));
/// ```
#[must_use]
pub fn is_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || (a == 100 && (64..128).contains(&b))
        }
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_local(IpAddr::V4(v4)),
            None => {
                v6.is_loopback()
                    || v6.is_unspecified()
                    || v6.is_unique_local()
                    || v6.is_unicast_link_local()
            }
        },
    }
}

/// A failed fetch. No message carries the URL or its host.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FetchError {
    /// The URL, or a redirect's target, is not one that may be fetched.
    #[error("{0}")]
    Url(&'static str),
    /// No connection could be made.
    #[error("Cannot connect to the server: {0}.")]
    Connect(String),
    /// Connecting took longer than the limit, in seconds.
    #[error("The server did not answer within {0} seconds.")]
    ConnectTimeout(u64),
    /// The TLS handshake failed; the text says why in general terms.
    #[error("{0}")]
    Tls(&'static str),
    /// The server sent nothing for this many seconds.
    #[error("The server sent nothing for {0} seconds.")]
    Stalled(u64),
    /// The final answer was not a success.
    #[error("The server answered {0}.")]
    Status(u16),
    /// More than [`MAX_REDIRECTS`] redirects.
    #[error("The server redirected more than {MAX_REDIRECTS} times.")]
    Redirects,
    /// A redirect from https to http.
    #[error("A redirect from https to http was refused.")]
    Downgrade,
    /// The exchange broke off or was malformed.
    #[error("The transfer failed: {0}.")]
    Transfer(String),
    /// A redirect from a public address to one on this machine or the local network.
    #[error("A redirect to an address on the local network was refused.")]
    LocalRedirect,
    /// The body arrived slower than [`Limits::min_rate`] on average.
    #[error("The server sent the file too slowly, under {0} bytes a second.")]
    TooSlow(u64),
    /// The body is compressed with a `Content-Encoding` that was not asked for.
    #[error("The server sent a compressed file mistarr can't read.")]
    Compressed,
}

/// Timeouts and redirect limit of a [`Fetcher`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// For the TCP connection and the TLS handshake together.
    pub connect: Duration,
    /// For the answer's head and between parts of its body.
    pub idle: Duration,
    /// Slowest average rate of a body, in bytes per second, once `window` has passed.
    pub min_rate: u64,
    /// How long a body may take before `min_rate` applies.
    pub window: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            connect: CONNECT_TIMEOUT,
            idle: IDLE_TIMEOUT,
            min_rate: MIN_RATE,
            window: RATE_WINDOW,
        }
    }
}

/// Makes one GET, following redirects of that request only; see [`Fetcher::get`].
pub struct Fetcher {
    tls: tokio_rustls::TlsConnector,
    limits: Limits,
    local: fn(IpAddr) -> bool,
}

struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl Fetcher {
    /// A fetcher trusting `roots` for https, over rustls with the ring provider.
    ///
    /// ```
    /// use mistarr_clients::fetch::{Fetcher, Limits, Roots};
    /// assert!(Fetcher::new(Roots::bundled(), Limits::default()).is_ok());
    /// ```
    ///
    /// # Errors
    ///
    /// [`FetchError::Tls`] when the TLS configuration cannot be built.
    pub fn new(roots: Roots, limits: Limits) -> Result<Self, FetchError> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|_| FetchError::Tls("TLS cannot be set up."))?
            .with_root_certificates(roots.store)
            .with_no_client_auth();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Self {
            tls: tokio_rustls::TlsConnector::from(Arc::new(config)),
            limits,
            local: is_local,
        })
    }

    /// This fetcher with `local` in place of [`is_local`], for tests on one loopback host.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn with_local(mut self, local: fn(IpAddr) -> bool) -> Self {
        self.local = local;
        self
    }

    /// GETs `url`, following up to [`MAX_REDIRECTS`] redirects, never from https to
    /// http, nor to a local address when `url` itself is not local, and returns the
    /// successful answer with its body still to read. Each host is resolved once and only
    /// the addresses checked are dialled.
    ///
    /// # Errors
    ///
    /// Any [`FetchError`]; the body is never read on failure.
    pub async fn get(&self, url: &FetchUrl) -> Result<Response, FetchError> {
        let mut url = url.clone();
        let mut hops = 0;
        let mut typed_local = false;
        loop {
            let addrs = self.resolve(&url).await?;
            let local = addrs.iter().any(|a| (self.local)(a.ip()));
            if hops == 0 {
                typed_local = local;
            } else if local && !typed_local {
                return Err(FetchError::LocalRedirect);
            }
            let (head, driver) = self.request(&url, &addrs).await?;
            let status = head.status();
            if is_redirect(status) {
                if hops == MAX_REDIRECTS {
                    return Err(FetchError::Redirects);
                }
                let next = head
                    .headers()
                    .get(header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or(FetchError::Status(status.as_u16()))
                    .and_then(|l| url.join(l))?;
                if url.is_secure() && !next.is_secure() {
                    return Err(FetchError::Downgrade);
                }
                hops += 1;
                url = next;
                continue;
            }
            if !status.is_success() || status == StatusCode::PARTIAL_CONTENT {
                return Err(FetchError::Status(status.as_u16()));
            }
            let (parts, body) = head.into_parts();
            let encoded = parts
                .headers
                .get(header::CONTENT_ENCODING)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| !v.trim().eq_ignore_ascii_case("identity"));
            if encoded {
                return Err(FetchError::Compressed);
            }
            return Ok(Response {
                content_length: content_length(&parts.headers),
                name: disposition_name(&parts.headers).or_else(|| url.last_segment()),
                body,
                limits: self.limits,
                started: Instant::now(),
                received: 0,
                _driver: driver,
            });
        }
    }

    /// The addresses `url`'s host resolves to, within the connect limit.
    async fn resolve(&self, url: &FetchUrl) -> Result<Vec<SocketAddr>, FetchError> {
        let limit = self.limits.connect;
        let found = tokio::time::timeout(limit, tokio::net::lookup_host((url.host(), url.port())))
            .await
            .map_err(|_| FetchError::ConnectTimeout(limit.as_secs()))?
            .map_err(|e| FetchError::Connect(e.to_string()))?;
        let addrs: Vec<SocketAddr> = found.collect();
        if addrs.is_empty() {
            return Err(FetchError::Connect(
                "the host name has no address".to_owned(),
            ));
        }
        Ok(addrs)
    }

    async fn request(
        &self,
        url: &FetchUrl,
        addrs: &[SocketAddr],
    ) -> Result<(hyper::Response<Incoming>, AbortOnDrop), FetchError> {
        let limit = self.limits.connect;
        let timed_out = || FetchError::ConnectTimeout(limit.as_secs());
        let deadline = tokio::time::Instant::now() + limit;
        let tcp = tokio::time::timeout_at(deadline, TcpStream::connect(addrs))
            .await
            .map_err(|_| timed_out())?
            .map_err(|e| FetchError::Connect(e.to_string()))?;
        let _ = tcp.set_nodelay(true);
        if !url.is_secure() {
            return self.exchange(url, tcp).await;
        }
        let name = ServerName::try_from(url.host().to_owned())
            .map_err(|_| FetchError::Url("The link's host is not valid."))?;
        let tls = tokio::time::timeout_at(deadline, self.tls.connect(name, tcp))
            .await
            .map_err(|_| timed_out())?
            .map_err(|e| tls_error(&e))?;
        self.exchange(url, tls).await
    }

    async fn exchange<IO>(
        &self,
        url: &FetchUrl,
        io: IO,
    ) -> Result<(hyper::Response<Incoming>, AbortOnDrop), FetchError>
    where
        IO: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let broke = |e: hyper::Error| FetchError::Transfer(e.to_string());
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(io))
            .await
            .map_err(broke)?;
        // Errors surface through the request and body futures instead.
        let driver = AbortOnDrop(tokio::spawn(async move {
            let _ = conn.await;
        }));
        let req = Request::get(url.target())
            .header(header::HOST, url.authority())
            .header(
                header::USER_AGENT,
                concat!("mistarr/", env!("CARGO_PKG_VERSION")),
            )
            .header(header::ACCEPT, "*/*")
            .header(header::ACCEPT_ENCODING, "identity")
            .header(header::CONNECTION, "close")
            .body(Empty::<Bytes>::new())
            .map_err(|_| FetchError::Url("This is not a valid link."))?;
        let idle = self.limits.idle;
        let head = tokio::time::timeout(idle, sender.send_request(req))
            .await
            .map_err(|_| FetchError::Stalled(idle.as_secs()))?
            .map_err(broke)?;
        Ok((head, driver))
    }
}

fn is_redirect(status: StatusCode) -> bool {
    matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308)
}

/// Says in general terms why a handshake failed, without naming the host.
fn tls_error(e: &std::io::Error) -> FetchError {
    use rustls::CertificateError as C;
    let inner = e.get_ref().and_then(|i| i.downcast_ref::<rustls::Error>());
    let Some(rustls::Error::InvalidCertificate(cert)) = inner else {
        return match inner {
            Some(_) => FetchError::Tls("The secure connection could not be set up."),
            None => FetchError::Connect(e.kind().to_string()),
        };
    };
    FetchError::Tls(match cert {
        C::NotValidForName | C::NotValidForNameContext { .. } => {
            "The server's certificate is for another host."
        }
        C::Expired | C::ExpiredContext { .. } | C::NotValidYet | C::NotValidYetContext { .. } => {
            "The server's certificate is not valid at this time; check the board's clock."
        }
        _ => "The server's certificate is not trusted.",
    })
}

fn content_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse().ok())
}

fn disposition_name(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .and_then(disposition_file_name)
}

/// A successful answer whose body is read with [`Response::chunk`].
pub struct Response {
    content_length: Option<u64>,
    name: Option<String>,
    body: Incoming,
    limits: Limits,
    started: Instant,
    received: u64,
    _driver: AbortOnDrop,
}

impl Response {
    /// The body's length when the server gave one.
    #[must_use]
    pub fn content_length(&self) -> Option<u64> {
        self.content_length
    }

    /// The file name from `Content-Disposition`, else the final URL's last path segment,
    /// unsanitised.
    #[must_use]
    pub fn file_name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// The next part of the body, `None` at its end.
    ///
    /// # Errors
    ///
    /// [`FetchError::Stalled`] after the idle limit without data, [`FetchError::TooSlow`]
    /// once the window has passed with the average under the minimum rate,
    /// [`FetchError::Transfer`] when the connection breaks, including before a promised
    /// length arrived.
    pub async fn chunk(&mut self) -> Result<Option<Bytes>, FetchError> {
        let idle = self.limits.idle;
        loop {
            self.check_rate()?;
            let frame = tokio::time::timeout(idle, self.body.frame())
                .await
                .map_err(|_| FetchError::Stalled(idle.as_secs()))?;
            match frame {
                None => return Ok(None),
                Some(Err(e)) => return Err(FetchError::Transfer(e.to_string())),
                Some(Ok(f)) => {
                    if let Ok(data) = f.into_data() {
                        if !data.is_empty() {
                            self.received += data.len() as u64;
                            self.check_rate()?;
                            return Ok(Some(data));
                        }
                    }
                }
            }
        }
    }

    fn check_rate(&self) -> Result<(), FetchError> {
        let elapsed = self.started.elapsed();
        let Limits {
            min_rate, window, ..
        } = self.limits;
        let needed = u128::from(min_rate) * elapsed.as_millis() / 1000;
        if elapsed >= window && u128::from(self.received) < needed {
            return Err(FetchError::TooSlow(min_rate));
        }
        Ok(())
    }
}
