//! One GET of a URL the user supplied, streamed; the contract is `docs/ARCHITECTURE.md` "Fetching a URL".

use std::sync::Arc;
use std::time::Duration;

use http_body_util::{BodyExt, Empty};
use hyper::body::{Bytes, Incoming};
use hyper::header::{self, HeaderMap};
use hyper::{Request, StatusCode};
use hyper_util::rt::TokioIo;
use rustls::pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::task::JoinHandle;

mod roots;
mod url;

pub use roots::{Roots, RootsOrigin, CERT_FILE_ENV, SYSTEM_BUNDLES};
pub use url::{percent_decode, FetchUrl};

#[cfg(test)]
mod tests;

/// Redirects of one request that are followed; one more fails the fetch.
pub const MAX_REDIRECTS: u8 = 5;

/// Longest a connection, with its TLS handshake, may take.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Longest the server may send nothing, for the answer's head or any part of its body.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

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
}

/// Timeouts and redirect limit of a [`Fetcher`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// For the TCP connection and the TLS handshake together.
    pub connect: Duration,
    /// For the answer's head and between parts of its body.
    pub idle: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            connect: CONNECT_TIMEOUT,
            idle: IDLE_TIMEOUT,
        }
    }
}

/// Makes one GET, following redirects of that request only; see [`Fetcher::get`].
pub struct Fetcher {
    tls: tokio_rustls::TlsConnector,
    limits: Limits,
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
        })
    }

    /// GETs `url`, following up to [`MAX_REDIRECTS`] redirects, never from https to
    /// http, and returns the successful answer with its body still to read.
    ///
    /// # Errors
    ///
    /// Any [`FetchError`]; the body is never read on failure.
    pub async fn get(&self, url: &FetchUrl) -> Result<Response, FetchError> {
        let mut url = url.clone();
        let mut hops = 0;
        loop {
            let (head, driver) = self.request(&url).await?;
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
            return Ok(Response {
                content_length: content_length(&parts.headers),
                name: disposition_name(&parts.headers).or_else(|| url.last_segment()),
                body,
                idle: self.limits.idle,
                _driver: driver,
            });
        }
    }

    async fn request(
        &self,
        url: &FetchUrl,
    ) -> Result<(hyper::Response<Incoming>, AbortOnDrop), FetchError> {
        let limit = self.limits.connect;
        let timed_out = || FetchError::ConnectTimeout(limit.as_secs());
        let deadline = tokio::time::Instant::now() + limit;
        let tcp = tokio::time::timeout_at(deadline, TcpStream::connect((url.host(), url.port())))
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

/// The file name a `Content-Disposition` header gives, `filename*` before `filename`.
///
/// ```
/// use mistarr_clients::fetch::disposition_file_name;
/// assert_eq!(disposition_file_name(r#"attachment; filename="a b.dat""#).as_deref(), Some("a b.dat"));
/// assert_eq!(disposition_file_name("attachment; filename*=UTF-8''p%C3%A9.zip; filename=p.zip").as_deref(), Some("pé.zip"));
/// assert_eq!(disposition_file_name("inline"), None);
/// ```
#[must_use]
pub fn disposition_file_name(value: &str) -> Option<String> {
    let params: Vec<(String, &str)> = value
        .split(';')
        .skip(1)
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim()))
        .collect();
    let extended = params
        .iter()
        .find(|(k, _)| k == "filename*")
        .and_then(|(_, v)| {
            let (_charset, rest) = v.split_once('\'')?;
            let (_lang, encoded) = rest.split_once('\'')?;
            Some(percent_decode(encoded.trim_matches('"')))
        });
    let plain = || {
        params
            .iter()
            .find(|(k, _)| k == "filename")
            .map(|(_, v)| v.trim_matches('"').to_owned())
    };
    extended.or_else(plain).filter(|n| !n.trim().is_empty())
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
    idle: Duration,
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
    /// [`FetchError::Stalled`] after the idle limit without data, [`FetchError::Transfer`]
    /// when the connection breaks, including before a promised length arrived.
    pub async fn chunk(&mut self) -> Result<Option<Bytes>, FetchError> {
        loop {
            let frame = tokio::time::timeout(self.idle, self.body.frame())
                .await
                .map_err(|_| FetchError::Stalled(self.idle.as_secs()))?;
            match frame {
                None => return Ok(None),
                Some(Err(e)) => return Err(FetchError::Transfer(e.to_string())),
                Some(Ok(f)) => {
                    if let Ok(data) = f.into_data() {
                        if !data.is_empty() {
                            return Ok(Some(data));
                        }
                    }
                }
            }
        }
    }
}
