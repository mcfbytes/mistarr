//! An in-process HTTP or HTTPS file server with scripted routes, for fetch tests.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use super::lock;

/// What one path answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRoute {
    /// HTTP status code.
    pub status: u16,
    /// Extra headers.
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: Vec<u8>,
    /// `Content-Length` sent; `None` ends the body by closing the connection.
    pub length: Option<u64>,
    /// Bytes per write and the pause after each, for a slow body.
    pub pace: Option<(usize, Duration)>,
}

impl FileRoute {
    /// A 200 with `body` and its length.
    ///
    /// ```
    /// let r = mistarr_clients::fake::FileRoute::ok(b"abc".to_vec());
    /// assert_eq!((r.status, r.length), (200, Some(3)));
    /// ```
    #[must_use]
    pub fn ok(body: Vec<u8>) -> Self {
        Self {
            status: 200,
            headers: Vec::new(),
            length: Some(body.len() as u64),
            body,
            pace: None,
        }
    }

    /// A redirect with `status` to `location`.
    ///
    /// ```
    /// let r = mistarr_clients::fake::FileRoute::redirect(302, "/b");
    /// assert_eq!(r.headers[0].1, "/b");
    /// ```
    #[must_use]
    pub fn redirect(status: u16, location: &str) -> Self {
        let mut r = Self::ok(Vec::new()).with_header("Location", location);
        r.status = status;
        r
    }

    /// Adds a header.
    ///
    /// ```
    /// let r = mistarr_clients::fake::FileRoute::ok(Vec::new()).with_header("A", "b");
    /// assert_eq!(r.headers.len(), 1);
    /// ```
    #[must_use]
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    /// Sends no `Content-Length`, so the body ends when the connection closes.
    ///
    /// ```
    /// assert_eq!(mistarr_clients::fake::FileRoute::ok(vec![1]).without_length().length, None);
    /// ```
    #[must_use]
    pub fn without_length(mut self) -> Self {
        self.length = None;
        self
    }

    /// Writes the body `bytes` at a time, pausing `pause` after each.
    ///
    /// ```
    /// use std::time::Duration;
    /// let r = mistarr_clients::fake::FileRoute::ok(vec![0; 4]).paced(2, Duration::from_millis(1));
    /// assert_eq!(r.pace, Some((2, Duration::from_millis(1))));
    /// ```
    #[must_use]
    pub fn paced(mut self, bytes: usize, pause: Duration) -> Self {
        self.pace = Some((bytes.max(1), pause));
        self
    }
}

#[derive(Default)]
struct Files {
    routes: HashMap<String, FileRoute>,
    hits: Vec<String>,
}

/// Serves [`FileRoute`]s by request path and records every path asked for.
pub struct FileServer {
    addr: SocketAddr,
    secure: bool,
    state: Arc<Mutex<Files>>,
    task: JoinHandle<()>,
}

impl FileServer {
    /// Starts a plain HTTP server on an ephemeral localhost port.
    ///
    /// # Errors
    ///
    /// When the listener cannot bind.
    pub async fn start() -> io::Result<Self> {
        Self::launch(None, IpAddr::from([127, 0, 0, 1])).await
    }

    /// Starts a plain HTTP server on `ip`, such as another loopback address.
    ///
    /// # Errors
    ///
    /// When the listener cannot bind.
    pub async fn start_at(ip: IpAddr) -> io::Result<Self> {
        Self::launch(None, ip).await
    }

    /// Starts an HTTPS server presenting `config`'s certificate.
    ///
    /// # Errors
    ///
    /// When the listener cannot bind.
    pub async fn start_tls(config: rustls::ServerConfig) -> io::Result<Self> {
        let tls = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        Self::launch(Some(tls), IpAddr::from([127, 0, 0, 1])).await
    }

    async fn launch(tls: Option<tokio_rustls::TlsAcceptor>, ip: IpAddr) -> io::Result<Self> {
        let listener = TcpListener::bind((ip, 0)).await?;
        let addr = listener.local_addr()?;
        let state = Arc::new(Mutex::new(Files::default()));
        let shared = Arc::clone(&state);
        let secure = tls.is_some();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let state = Arc::clone(&shared);
                let tls = tls.clone();
                tokio::spawn(async move {
                    match tls {
                        Some(acceptor) => {
                            if let Ok(s) = acceptor.accept(stream).await {
                                answer(s, state).await;
                            }
                        }
                        None => answer(stream, state).await,
                    }
                });
            }
        });
        Ok(Self {
            addr,
            secure,
            state,
            task,
        })
    }

    /// Answers `path` with `route` from now on.
    pub fn route(&self, path: &str, route: FileRoute) {
        lock(&self.state).routes.insert(path.to_owned(), route);
    }

    /// The URL of `path` on this server, by IP address.
    #[must_use]
    pub fn url(&self, path: &str) -> String {
        let scheme = if self.secure { "https" } else { "http" };
        format!("{scheme}://{}{path}", self.addr)
    }

    /// The listening address.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Every request path so far, in order.
    #[must_use]
    pub fn hits(&self) -> Vec<String> {
        lock(&self.state).hits.clone()
    }
}

impl Drop for FileServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn answer<S: AsyncRead + AsyncWrite + Unpin>(mut stream: S, state: Arc<Mutex<Files>>) {
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        match stream.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => head.extend_from_slice(&buf[..n]),
        }
        if head.len() > 64 * 1024 {
            return;
        }
    }
    let text = String::from_utf8_lossy(&head);
    let path = text.split_whitespace().nth(1).unwrap_or("/").to_owned();
    let route = {
        let mut files = lock(&state);
        files.hits.push(path.clone());
        files.routes.get(&path).cloned()
    };
    let route = route.unwrap_or_else(|| {
        let mut r = FileRoute::ok(b"not found".to_vec());
        r.status = 404;
        r
    });
    let mut out = format!("HTTP/1.1 {} X\r\nConnection: close\r\n", route.status);
    for (k, v) in &route.headers {
        let _ = write!(out, "{k}: {v}\r\n");
    }
    if let Some(n) = route.length {
        let _ = write!(out, "Content-Length: {n}\r\n");
    }
    out.push_str("\r\n");
    if stream.write_all(out.as_bytes()).await.is_err() {
        return;
    }
    match route.pace {
        None => {
            let _ = stream.write_all(&route.body).await;
        }
        Some((size, pause)) => {
            for part in route.body.chunks(size) {
                if stream.write_all(part).await.is_err() || stream.flush().await.is_err() {
                    return;
                }
                tokio::time::sleep(pause).await;
            }
        }
    }
    let _ = stream.shutdown().await;
}
