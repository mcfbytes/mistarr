//! In-process HTTP and SCGI servers that record requests and replay scripted responses.

use std::collections::VecDeque;
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

mod scgi;

pub use scgi::{FakeScgiServer, ScgiReply, ScgiRequest};

const MAX_HEAD: usize = 64 * 1024;

/// One scripted HTTP response.
///
/// ```
/// use mistarr_clients::fake::FakeResponse;
/// let r = FakeResponse::new(409).with_header("X-Transmission-Session-Id", "abc");
/// assert_eq!(r.status, 409);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeResponse {
    /// HTTP status code.
    pub status: u16,
    /// Extra headers; `Content-Length` and `Connection` are added.
    pub headers: Vec<(String, String)>,
    /// Response body.
    pub body: Vec<u8>,
}

impl FakeResponse {
    /// An empty response with `status`.
    ///
    /// ```
    /// assert!(mistarr_clients::fake::FakeResponse::new(500).body.is_empty());
    /// ```
    #[must_use]
    pub fn new(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    /// Adds a header.
    ///
    /// ```
    /// let r = mistarr_clients::fake::FakeResponse::new(200).with_header("A", "b");
    /// assert_eq!(r.headers, vec![("A".to_owned(), "b".to_owned())]);
    /// ```
    #[must_use]
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    /// A 200 with a JSON body.
    ///
    /// ```
    /// let r = mistarr_clients::fake::FakeResponse::json(&serde_json::json!({"a": 1}));
    /// assert_eq!(r.body, br#"{"a":1}"#);
    /// ```
    #[must_use]
    pub fn json(body: &Value) -> Self {
        let mut r = Self::new(200).with_header("Content-Type", "application/json");
        r.body = body.to_string().into_bytes();
        r
    }

    /// A Transmission reply with `result: "success"` and `arguments`.
    ///
    /// ```
    /// use mistarr_clients::fake::FakeResponse;
    /// let r = FakeResponse::success(serde_json::json!({}));
    /// assert_eq!(r.body, br#"{"arguments":{},"result":"success"}"#);
    /// ```
    #[must_use]
    pub fn success(arguments: Value) -> Self {
        let mut body = serde_json::Map::new();
        body.insert("arguments".into(), arguments);
        body.insert("result".into(), json!("success"));
        Self::json(&Value::Object(body))
    }

    /// A Transmission reply whose `result` is the error string `result`.
    ///
    /// ```
    /// let r = mistarr_clients::fake::FakeResponse::failure("no such method");
    /// assert_eq!(r.status, 200);
    /// ```
    #[must_use]
    pub fn failure(result: &str) -> Self {
        Self::json(&json!({ "result": result, "arguments": {} }))
    }

    /// Transmission's 409 carrying a new session id.
    ///
    /// ```
    /// let r = mistarr_clients::fake::FakeResponse::session_conflict("s1");
    /// assert_eq!(r.status, 409);
    /// ```
    #[must_use]
    pub fn session_conflict(session_id: &str) -> Self {
        Self::new(409).with_header("X-Transmission-Session-Id", session_id)
    }
}

/// A request as the fake received it.
///
/// ```
/// use mistarr_clients::fake::RecordedRequest;
/// let r = RecordedRequest {
///     method: "POST".into(),
///     path: "/".into(),
///     headers: vec![("host".into(), "x".into())],
///     body: b"{}".to_vec(),
/// };
/// assert_eq!(r.header("Host"), Some("x"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedRequest {
    /// Request method, e.g. `POST`.
    pub method: String,
    /// Request target as sent.
    pub path: String,
    /// Headers in order received, names lowercased.
    pub headers: Vec<(String, String)>,
    /// Raw body.
    pub body: Vec<u8>,
}

impl RecordedRequest {
    /// The first header named `name`, case-insensitively.
    ///
    /// ```
    /// use mistarr_clients::fake::RecordedRequest;
    /// let r = RecordedRequest { method: "GET".into(), path: "/".into(), headers: vec![], body: vec![] };
    /// assert_eq!(r.header("host"), None);
    /// ```
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The body parsed as JSON, or `None` if it is not JSON.
    ///
    /// ```
    /// use mistarr_clients::fake::RecordedRequest;
    /// let r = RecordedRequest { method: "POST".into(), path: "/".into(), headers: vec![], body: b"[1]".to_vec() };
    /// assert_eq!(r.json(), Some(serde_json::json!([1])));
    /// ```
    #[must_use]
    pub fn json(&self) -> Option<Value> {
        serde_json::from_slice(&self.body).ok()
    }
}

#[derive(Default)]
struct State {
    script: VecDeque<FakeResponse>,
    requests: Vec<RecordedRequest>,
}

/// A local HTTP server on an ephemeral port. Each connection carries one
/// request, answered with the next scripted response, or 500 when none is left.
///
/// ```
/// use mistarr_clients::fake::{FakeResponse, FakeServer};
/// use mistarr_clients::{DownloadClient, Transmission};
/// # tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async {
/// let fake = FakeServer::start().await?;
/// fake.push(FakeResponse::success(serde_json::json!({ "version": "4.0.0" })));
/// let info = Transmission::new(&fake.url())?.probe().await?;
/// assert_eq!(info.version, "4.0.0");
/// assert_eq!(fake.requests().len(), 1);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # })?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct FakeServer {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
    task: JoinHandle<()>,
}

impl FakeServer {
    /// Binds `127.0.0.1:0` and starts serving on the current runtime.
    ///
    /// # Errors
    /// Fails if the socket cannot be bound.
    ///
    /// ```
    /// # tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async {
    /// let fake = mistarr_clients::fake::FakeServer::start().await?;
    /// assert!(fake.addr().ip().is_loopback());
    /// # Ok::<(), std::io::Error>(())
    /// # })?;
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub async fn start() -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let state = Arc::new(Mutex::new(State::default()));
        let shared = Arc::clone(&state);
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve(stream, Arc::clone(&shared)));
            }
        });
        Ok(Self { addr, state, task })
    }

    /// The bound address.
    ///
    /// ```
    /// # tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async {
    /// let fake = mistarr_clients::fake::FakeServer::start().await?;
    /// assert_ne!(fake.addr().port(), 0);
    /// # Ok::<(), std::io::Error>(())
    /// # })?;
    /// # Ok::<(), std::io::Error>(())
    /// ```
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// A Transmission-style RPC URL on this server.
    ///
    /// ```
    /// # tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async {
    /// let fake = mistarr_clients::fake::FakeServer::start().await?;
    /// assert!(fake.url().ends_with("/transmission/rpc"));
    /// # Ok::<(), std::io::Error>(())
    /// # })?;
    /// # Ok::<(), std::io::Error>(())
    /// ```
    #[must_use]
    pub fn url(&self) -> String {
        format!("http://{}/transmission/rpc", self.addr)
    }

    /// Queues the response for the next unanswered request.
    ///
    /// ```
    /// # tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async {
    /// let fake = mistarr_clients::fake::FakeServer::start().await?;
    /// fake.push(mistarr_clients::fake::FakeResponse::new(204));
    /// # Ok::<(), std::io::Error>(())
    /// # })?;
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub fn push(&self, response: FakeResponse) {
        lock(&self.state).script.push_back(response);
    }

    /// Every request received so far, in arrival order.
    ///
    /// ```
    /// # tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async {
    /// let fake = mistarr_clients::fake::FakeServer::start().await?;
    /// assert!(fake.requests().is_empty());
    /// # Ok::<(), std::io::Error>(())
    /// # })?;
    /// # Ok::<(), std::io::Error>(())
    /// ```
    #[must_use]
    pub fn requests(&self) -> Vec<RecordedRequest> {
        lock(&self.state).requests.clone()
    }

    /// The JSON bodies of every request received so far.
    ///
    /// ```
    /// # tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async {
    /// let fake = mistarr_clients::fake::FakeServer::start().await?;
    /// assert!(fake.bodies().is_empty());
    /// # Ok::<(), std::io::Error>(())
    /// # })?;
    /// # Ok::<(), std::io::Error>(())
    /// ```
    #[must_use]
    pub fn bodies(&self) -> Vec<Value> {
        self.requests()
            .iter()
            .map(|r| r.json().unwrap_or(Value::Null))
            .collect()
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn lock<T>(state: &Mutex<T>) -> MutexGuard<'_, T> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

async fn serve(mut stream: TcpStream, state: Arc<Mutex<State>>) {
    let Some(request) = read_request(&mut stream).await else {
        return;
    };
    let response = {
        let mut st = lock(&state);
        st.requests.push(request);
        st.script
            .pop_front()
            .unwrap_or_else(|| FakeResponse::new(500))
    };
    let mut head = format!(
        "HTTP/1.1 {} Scripted\r\nContent-Length: {}\r\nConnection: close\r\n",
        response.status,
        response.body.len()
    );
    for (name, value) in &response.headers {
        for part in [name.as_str(), ": ", value.as_str(), "\r\n"] {
            head.push_str(part);
        }
    }
    head.push_str("\r\n");
    let mut out = head.into_bytes();
    out.extend_from_slice(&response.body);
    // A client that already gave up is not an error for the fake.
    let _ = stream.write_all(&out).await;
    let _ = stream.shutdown().await;
}

async fn read_request(stream: &mut TcpStream) -> Option<RecordedRequest> {
    let mut buf = Vec::new();
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        if buf.len() > MAX_HEAD {
            return None;
        }
        let mut chunk = [0u8; 4096];
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = std::str::from_utf8(&buf[..head_end]).ok()?;
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_owned();
    let path = first.next()?.to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let length: usize = headers
        .iter()
        .find(|(n, _)| n == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = buf.split_off(head_end + 4);
    while body.len() < length {
        let mut chunk = vec![0u8; length - body.len()];
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(length);
    Some(RecordedRequest {
        method,
        path,
        headers,
        body,
    })
}
