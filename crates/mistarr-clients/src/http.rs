//! One-shot HTTP/1.1 POST over plain TCP, enough for a client RPC on the LAN.

use std::time::Duration;

use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Bytes;
use hyper::header::{HeaderValue, AUTHORIZATION, CONTENT_TYPE, HOST, WWW_AUTHENTICATE};
use hyper::{Request, Uri};
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;
use tokio::task::JoinHandle;

use crate::ClientError;

/// Largest response body accepted; a `torrent-get` of a 10 000 file torrent
/// is a few MiB.
const MAX_BODY: usize = 16 * 1024 * 1024;

/// Name of Transmission's CSRF header.
pub(crate) const SESSION_HEADER: &str = "x-transmission-session-id";

/// Where to send requests: `host:port` to dial and the request path.
#[derive(Debug, Clone)]
pub(crate) struct Endpoint {
    authority: String,
    path: String,
}

impl Endpoint {
    /// Parses an `http://host[:port]/path` URL; other schemes are rejected.
    pub(crate) fn parse(url: &str) -> Result<Self, ClientError> {
        let bad = |why: &str| ClientError::Protocol(format!("invalid client url {url:?}: {why}"));
        let uri: Uri = url.parse().map_err(|_| bad("not a URL"))?;
        if uri.scheme_str() != Some("http") {
            return Err(bad("only http:// is supported"));
        }
        let auth = uri.authority().ok_or_else(|| bad("no host"))?;
        let authority = format!("{}:{}", auth.host(), auth.port_u16().unwrap_or(80));
        let path = uri
            .path_and_query()
            .map_or_else(|| "/".to_owned(), |p| p.as_str().to_owned());
        Ok(Self { authority, path })
    }
}

/// The parts of a response the clients look at.
#[derive(Debug)]
pub(crate) struct Response {
    pub(crate) status: u16,
    pub(crate) session_id: Option<String>,
    pub(crate) www_authenticate: Option<String>,
    pub(crate) body: Bytes,
}

/// Extra request headers for [`post`].
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Headers<'a> {
    pub(crate) session_id: Option<&'a str>,
    pub(crate) authorization: Option<&'a str>,
}

struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// POSTs a JSON `body` on a fresh connection and reads the whole response,
/// all within `timeout`.
pub(crate) async fn post(
    endpoint: &Endpoint,
    headers: Headers<'_>,
    body: Bytes,
    timeout: Duration,
) -> Result<Response, ClientError> {
    tokio::time::timeout(timeout, exchange(endpoint, headers, body))
        .await
        .map_err(|_| ClientError::Unreachable(format!("{} timed out", endpoint.authority)))?
}

async fn exchange(
    endpoint: &Endpoint,
    headers: Headers<'_>,
    body: Bytes,
) -> Result<Response, ClientError> {
    let unreachable = |e: &dyn std::fmt::Display| {
        ClientError::Unreachable(format!("{}: {e}", endpoint.authority))
    };
    let stream = TcpStream::connect(&endpoint.authority)
        .await
        .map_err(|e| unreachable(&e))?;
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .map_err(|e| unreachable(&e))?;
    let _driver = AbortOnDrop(tokio::spawn(async move {
        // Errors surface through the request future instead.
        let _ = conn.await;
    }));

    let mut req = Request::post(endpoint.path.as_str())
        .header(HOST, endpoint.authority.as_str())
        .header(CONTENT_TYPE, "application/json");
    if let Some(id) = headers.session_id {
        req = req.header(SESSION_HEADER, id);
    }
    if let Some(auth) = headers.authorization {
        req = req.header(AUTHORIZATION, auth);
    }
    let req = req
        .body(Full::new(body))
        .map_err(|e| ClientError::Protocol(format!("building request: {e}")))?;

    let resp = sender
        .send_request(req)
        .await
        .map_err(|e| unreachable(&e))?;
    let text = |v: Option<&HeaderValue>| v.and_then(|v| v.to_str().ok()).map(str::to_owned);
    let status = resp.status().as_u16();
    let session_id = text(resp.headers().get(SESSION_HEADER));
    let www_authenticate = text(resp.headers().get(WWW_AUTHENTICATE));
    let body = Limited::new(resp.into_body(), MAX_BODY)
        .collect()
        .await
        .map_err(|e| {
            if e.is::<http_body_util::LengthLimitError>() {
                ClientError::Protocol(format!("response larger than {MAX_BODY} bytes"))
            } else {
                unreachable(&e)
            }
        })?
        .to_bytes();
    Ok(Response {
        status,
        session_id,
        www_authenticate,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeResponse, FakeServer};

    #[test]
    fn parses_endpoints() {
        let e = Endpoint::parse("http://127.0.0.1:9091/transmission/rpc").expect("valid");
        assert_eq!(e.authority, "127.0.0.1:9091");
        assert_eq!(e.path, "/transmission/rpc");
        let e = Endpoint::parse("http://nas.local").expect("valid");
        assert_eq!(e.authority, "nas.local:80");
        assert_eq!(e.path, "/");
        assert!(Endpoint::parse("https://127.0.0.1/").is_err());
        assert!(Endpoint::parse("127.0.0.1:5000").is_err());
    }

    #[tokio::test]
    async fn posts_headers_and_body() {
        let fake = FakeServer::start().await.expect("bind");
        fake.push(FakeResponse::new(200).with_header("X-Transmission-Session-Id", "s1"));
        let ep = Endpoint::parse(&fake.url()).expect("valid");
        let headers = Headers {
            session_id: Some("s0"),
            authorization: Some("Basic eDp5"),
        };
        let resp = post(
            &ep,
            headers,
            Bytes::from_static(b"{}"),
            Duration::from_secs(5),
        )
        .await
        .expect("answered");
        assert_eq!(resp.status, 200);
        assert_eq!(resp.session_id.as_deref(), Some("s1"));
        let req = &fake.requests()[0];
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/transmission/rpc");
        assert_eq!(req.header("x-transmission-session-id"), Some("s0"));
        assert_eq!(req.header("authorization"), Some("Basic eDp5"));
        assert_eq!(req.header("content-type"), Some("application/json"));
        assert_eq!(req.body, b"{}");
    }

    #[tokio::test]
    async fn times_out_as_unreachable() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let url = format!("http://{}/", listener.local_addr().expect("addr"));
        let ep = Endpoint::parse(&url).expect("valid");
        let err = post(
            &ep,
            Headers::default(),
            Bytes::new(),
            Duration::from_millis(100),
        )
        .await
        .expect_err("no answer");
        assert!(matches!(err, ClientError::Unreachable(_)), "{err:?}");
    }
}
