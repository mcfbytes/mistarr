//! Boots the server on an ephemeral port and speaks just enough HTTP/1.1 to test it.

#![allow(dead_code)] // Each test binary uses a different subset.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use mistarr_server::app::{self, Options, Running};
use mistarr_server::config::Config;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub struct Booted {
    pub dir: tempfile::TempDir,
    pub running: Running,
}

impl Booted {
    pub fn addr(&self) -> SocketAddr {
        self.running.addr
    }

    pub fn corename(&self) -> PathBuf {
        self.dir.path().join("CORENAME")
    }
}

/// A closed local port, so client detection never probes the host's defaults.
pub fn closed_url() -> String {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("addr");
    format!("http://{addr}/transmission/rpc")
}

/// Config rooted in `dir`, listening on an ephemeral port.
pub fn config_in(dir: &Path) -> Config {
    let mut c = Config::default();
    c.server.listen = "127.0.0.1:0".into();
    c.paths.root = dir.to_path_buf();
    c.paths.games = dir.join("games");
    c.paths.data = dir.join("data");
    c.client.kind = mistarr_server::config::ClientChoice::Transmission;
    c.client.url = closed_url();
    c
}

pub fn options_in(dir: &Path) -> Options {
    Options {
        corename_path: dir.join("CORENAME"),
        corename_poll: Duration::from_millis(20),
        status_interval: Duration::from_secs(3600),
        sources_poll: Duration::from_millis(50),
        sources_min_age_secs: 0,
        magnet_poll: Duration::from_millis(100),
        magnet_started_poll: Duration::from_millis(50),
        dats_poll: Duration::from_millis(50),
        dats_min_age: Duration::ZERO,
        poll_active: Duration::from_secs(3600),
        poll_idle: Duration::from_secs(3600),
        poll_backoff: Duration::from_secs(3600),
        command_path: dir.join("MiSTer_cmd"),
        launch_dir: dir.to_path_buf(),
        launch_gap: Duration::ZERO,
    }
}

pub async fn boot_with(dir: tempfile::TempDir, config: Config) -> Booted {
    let options = options_in(dir.path());
    boot_with_options(dir, config, options).await
}

pub async fn boot_with_options(dir: tempfile::TempDir, config: Config, options: Options) -> Booted {
    let running = app::start(config, options).await.expect("start");
    Booted { dir, running }
}

pub async fn boot() -> Booted {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = config_in(dir.path());
    boot_with(dir, config).await
}

pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("{e}: {}", self.body))
    }
}

/// Sends one request with `Connection: close` and the `X-Mistarr: 1` header the
/// SPA sends, and reads the whole response.
pub async fn request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
) -> Response {
    let mut all = vec![("X-Mistarr", "1")];
    all.extend_from_slice(headers);
    request_plain(addr, method, path, &all, body).await
}

/// [`request`] with exactly the given headers, as a page on another site could send.
pub async fn request_plain(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
) -> Response {
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    let body = body.unwrap_or("");
    if !body.is_empty() || method != "GET" {
        req.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        ));
    }
    req.push_str("\r\n");
    req.push_str(body);
    stream.write_all(req.as_bytes()).await.expect("write");
    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut raw))
        .await
        .expect("response in time")
        .expect("read");
    parse(&String::from_utf8_lossy(&raw))
}

pub async fn get(addr: SocketAddr, path: &str) -> Response {
    request(addr, "GET", path, &[], None).await
}

/// Sends one request with a raw body of any content type.
pub async fn request_bytes(
    addr: SocketAddr,
    method: &str,
    path: &str,
    content_type: &str,
    body: &[u8],
) -> Response {
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\
         X-Mistarr: 1\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await.expect("write");
    stream.write_all(body).await.expect("write");
    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut raw))
        .await
        .expect("response in time")
        .expect("read");
    parse(&String::from_utf8_lossy(&raw))
}

fn parse(raw: &str) -> Response {
    let (head, body) = raw.split_once("\r\n\r\n").expect("header end");
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .expect("status line");
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .collect();
    let chunked = headers
        .iter()
        .any(|(k, v)| k.eq_ignore_ascii_case("transfer-encoding") && v.contains("chunked"));
    let body = if chunked {
        dechunk(body)
    } else {
        body.to_owned()
    };
    Response {
        status,
        headers,
        body,
    }
}

fn dechunk(mut rest: &str) -> String {
    let mut out = String::new();
    while let Some((size, tail)) = rest.split_once("\r\n") {
        let Ok(n) = usize::from_str_radix(size.trim(), 16) else {
            break;
        };
        if n == 0 || tail.len() < n {
            break;
        }
        out.push_str(&tail[..n]);
        rest = tail[n..].trim_start_matches("\r\n");
    }
    out
}

/// An open SSE connection.
pub struct Sse {
    stream: TcpStream,
    pub text: String,
}

impl Sse {
    pub async fn open(addr: SocketAddr, path: &str, headers: &[(&str, &str)]) -> Self {
        let mut stream = TcpStream::connect(addr).await.expect("connect");
        let mut req =
            format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nAccept: text/event-stream\r\n");
        for (k, v) in headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str("\r\n");
        stream.write_all(req.as_bytes()).await.expect("write");
        Self {
            stream,
            text: String::new(),
        }
    }

    /// Reads until `text` contains `needle`, failing after five seconds.
    pub async fn until(&mut self, needle: &str) {
        self.until_count(needle, 1, Duration::from_secs(5)).await;
    }

    /// Reads until `text` contains `needle` at least `n` times, failing after `limit`.
    pub async fn until_count(&mut self, needle: &str, n: usize, limit: Duration) {
        let deadline = tokio::time::Instant::now() + limit;
        let mut buf = [0u8; 4096];
        while self.text.matches(needle).count() < n {
            let n = tokio::time::timeout_at(deadline, self.stream.read(&mut buf))
                .await
                .unwrap_or_else(|_| panic!("no {needle:?} in:\n{}", self.text))
                .expect("read");
            assert!(n > 0, "stream closed before {needle:?}:\n{}", self.text);
            self.text.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
    }
}

/// Polls `f` until it returns true, failing after five seconds.
pub async fn eventually<F, Fut>(what: &str, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..250 {
        if f().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}
