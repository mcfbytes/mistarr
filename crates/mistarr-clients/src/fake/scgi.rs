//! In-process SCGI server speaking rtorrent's XML-RPC framing.

use std::collections::VecDeque;
use std::io;
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use super::lock;
use crate::xmlrpc::{self, Fault, Value};

/// Largest SCGI request the fake accepts.
const MAX_REQUEST: usize = 64 * 1024 * 1024;

/// One scripted SCGI reply.
///
/// ```
/// use mistarr_clients::fake::ScgiReply;
/// use mistarr_clients::xmlrpc::Value;
/// let r = ScgiReply::Value(Value::Int(0));
/// assert!(matches!(r, ScgiReply::Value(_)));
/// ```
#[derive(Debug, Clone, PartialEq)]
pub enum ScgiReply {
    /// A successful XML-RPC response carrying this value.
    Value(Value),
    /// An XML-RPC fault response.
    Fault(Fault),
    /// These exact bytes, headers included.
    Raw(Vec<u8>),
    /// Read the request and never answer.
    Silent,
}

impl ScgiReply {
    /// A fault with `code` and `message`.
    ///
    /// ```
    /// use mistarr_clients::fake::ScgiReply;
    /// assert!(matches!(ScgiReply::fault(-501, "gone"), ScgiReply::Fault(f) if f.code == -501));
    /// ```
    #[must_use]
    pub fn fault(code: i64, message: &str) -> Self {
        ScgiReply::Fault(Fault {
            code,
            message: message.to_owned(),
        })
    }

    /// A `system.multicall` reply: each value wrapped in a one-element array.
    ///
    /// ```
    /// use mistarr_clients::fake::ScgiReply;
    /// use mistarr_clients::xmlrpc::Value;
    /// let r = ScgiReply::multicall(vec![Value::Int(1)]);
    /// assert_eq!(r, ScgiReply::Value(Value::Array(vec![Value::Array(vec![Value::Int(1)])])));
    /// ```
    #[must_use]
    pub fn multicall(results: Vec<Value>) -> Self {
        ScgiReply::Value(Value::Array(
            results.into_iter().map(|v| Value::Array(vec![v])).collect(),
        ))
    }
}

/// An SCGI request as the fake received it.
///
/// ```
/// use mistarr_clients::fake::ScgiRequest;
/// use mistarr_clients::xmlrpc::{encode_call, Value};
/// let r = ScgiRequest { headers: vec![], body: encode_call("d.stop", &[Value::from("AB")]) };
/// assert_eq!(r.call(), Some(("d.stop".to_owned(), vec![Value::from("AB")])));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScgiRequest {
    /// Header name and value pairs in the order sent.
    pub headers: Vec<(String, String)>,
    /// Raw body.
    pub body: Vec<u8>,
}

impl ScgiRequest {
    /// The body decoded as an XML-RPC call, or `None` if it is not one.
    ///
    /// ```
    /// use mistarr_clients::fake::ScgiRequest;
    /// let r = ScgiRequest { headers: vec![], body: b"nope".to_vec() };
    /// assert_eq!(r.call(), None);
    /// ```
    #[must_use]
    pub fn call(&self) -> Option<(String, Vec<Value>)> {
        xmlrpc::decode_call(&self.body).ok()
    }
}

#[derive(Default)]
struct State {
    script: VecDeque<ScgiReply>,
    requests: Vec<ScgiRequest>,
}

/// A local SCGI server on TCP or a unix socket. Each connection carries one
/// request, answered with the next scripted reply, or a fault when none is left.
///
/// ```
/// use mistarr_clients::fake::{FakeScgiServer, ScgiReply};
/// use mistarr_clients::xmlrpc::Value;
/// use mistarr_clients::{DownloadClient, Rtorrent};
/// # tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async {
/// let fake = FakeScgiServer::start().await?;
/// fake.push(ScgiReply::Value(Value::from("0.9.8")));
/// let info = Rtorrent::new(&fake.addr())?.probe().await?;
/// assert_eq!(info.version, "0.9.8");
/// assert_eq!(fake.methods(), vec!["system.client_version"]);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # })?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct FakeScgiServer {
    addr: String,
    socket: Option<PathBuf>,
    state: Arc<Mutex<State>>,
    task: JoinHandle<()>,
}

impl FakeScgiServer {
    /// Binds `127.0.0.1:0` and starts serving on the current runtime.
    ///
    /// # Errors
    /// Fails if the socket cannot be bound.
    ///
    /// ```
    /// # tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async {
    /// let fake = mistarr_clients::fake::FakeScgiServer::start().await?;
    /// assert!(fake.addr().starts_with("127.0.0.1:"));
    /// # Ok::<(), std::io::Error>(())
    /// # })?;
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub async fn start() -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?.to_string();
        let state = Arc::new(Mutex::new(State::default()));
        let shared = Arc::clone(&state);
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve(stream, Arc::clone(&shared)));
            }
        });
        Ok(Self {
            addr,
            socket: None,
            state,
            task,
        })
    }

    /// Binds a unix socket at `path` and serves on the current runtime,
    /// replacing a stale file; the socket is removed on drop.
    ///
    /// # Errors
    /// Fails if the socket cannot be bound.
    ///
    /// ```
    /// # tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async {
    /// let path = std::env::temp_dir().join(format!("mistarr-doc-{}.sock", std::process::id()));
    /// let fake = mistarr_clients::fake::FakeScgiServer::start_unix(&path)?;
    /// assert_eq!(fake.addr(), path.display().to_string());
    /// # Ok::<(), std::io::Error>(())
    /// # })?;
    /// # Ok::<(), std::io::Error>(())
    /// ```
    #[cfg(unix)]
    pub fn start_unix(path: &Path) -> io::Result<Self> {
        match std::fs::remove_file(path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        let listener = tokio::net::UnixListener::bind(path)?;
        let state = Arc::new(Mutex::new(State::default()));
        let shared = Arc::clone(&state);
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve(stream, Arc::clone(&shared)));
            }
        });
        Ok(Self {
            addr: path.display().to_string(),
            socket: Some(path.to_path_buf()),
            state,
            task,
        })
    }

    /// The address to hand to [`crate::Rtorrent::new`].
    ///
    /// ```
    /// # tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async {
    /// let fake = mistarr_clients::fake::FakeScgiServer::start().await?;
    /// assert!(mistarr_clients::detect::ScgiAddr::parse(&fake.addr()).is_some());
    /// # Ok::<(), std::io::Error>(())
    /// # })?;
    /// # Ok::<(), std::io::Error>(())
    /// ```
    #[must_use]
    pub fn addr(&self) -> String {
        self.addr.clone()
    }

    /// Queues the reply for the next unanswered request.
    ///
    /// ```
    /// # tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async {
    /// use mistarr_clients::fake::{FakeScgiServer, ScgiReply};
    /// let fake = FakeScgiServer::start().await?;
    /// fake.push(ScgiReply::Silent);
    /// # Ok::<(), std::io::Error>(())
    /// # })?;
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub fn push(&self, reply: ScgiReply) {
        lock(&self.state).script.push_back(reply);
    }

    /// Every request received so far, in arrival order.
    ///
    /// ```
    /// # tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async {
    /// let fake = mistarr_clients::fake::FakeScgiServer::start().await?;
    /// assert!(fake.requests().is_empty());
    /// # Ok::<(), std::io::Error>(())
    /// # })?;
    /// # Ok::<(), std::io::Error>(())
    /// ```
    #[must_use]
    pub fn requests(&self) -> Vec<ScgiRequest> {
        lock(&self.state).requests.clone()
    }

    /// Every request decoded as `(method, params)`; undecodable bodies are skipped.
    ///
    /// ```
    /// # tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async {
    /// let fake = mistarr_clients::fake::FakeScgiServer::start().await?;
    /// assert!(fake.calls().is_empty());
    /// # Ok::<(), std::io::Error>(())
    /// # })?;
    /// # Ok::<(), std::io::Error>(())
    /// ```
    #[must_use]
    pub fn calls(&self) -> Vec<(String, Vec<Value>)> {
        self.requests()
            .iter()
            .filter_map(ScgiRequest::call)
            .collect()
    }

    /// The method name of every decodable request, in order.
    ///
    /// ```
    /// # tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async {
    /// let fake = mistarr_clients::fake::FakeScgiServer::start().await?;
    /// assert!(fake.methods().is_empty());
    /// # Ok::<(), std::io::Error>(())
    /// # })?;
    /// # Ok::<(), std::io::Error>(())
    /// ```
    #[must_use]
    pub fn methods(&self) -> Vec<String> {
        self.calls().into_iter().map(|(m, _)| m).collect()
    }
}

impl Drop for FakeScgiServer {
    fn drop(&mut self) {
        self.task.abort();
        if let Some(path) = &self.socket {
            let _ = std::fs::remove_file(path);
        }
    }
}

async fn serve<S: AsyncRead + AsyncWrite + Unpin>(mut stream: S, state: Arc<Mutex<State>>) {
    let Some(request) = read_request(&mut stream).await else {
        return;
    };
    let reply = {
        let mut st = lock(&state);
        st.requests.push(request);
        st.script
            .pop_front()
            .unwrap_or_else(|| ScgiReply::fault(-1, "unscripted call"))
    };
    let out = match reply {
        ScgiReply::Silent => return std::future::pending().await,
        ScgiReply::Raw(bytes) => bytes,
        ScgiReply::Value(v) => response(&xmlrpc::encode_response(&v)),
        ScgiReply::Fault(f) => response(&xmlrpc::encode_fault(&f)),
    };
    // A client that already gave up is not an error for the fake.
    let _ = stream.write_all(&out).await;
    let _ = stream.shutdown().await;
}

fn response(body: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "Status: 200 OK\r\nContent-Type: text/xml\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    out
}

async fn read_request<S: AsyncRead + Unpin>(stream: &mut S) -> Option<ScgiRequest> {
    let mut buf = Vec::new();
    loop {
        if let Some(request) = parse(&buf) {
            return Some(request);
        }
        if buf.len() > MAX_REQUEST {
            return None;
        }
        let mut chunk = [0u8; 8192];
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// Parses a complete netstring header and `CONTENT_LENGTH` body, or `None` if
/// more bytes are needed.
fn parse(buf: &[u8]) -> Option<ScgiRequest> {
    let colon = buf.iter().position(|&b| b == b':')?;
    let len: usize = std::str::from_utf8(&buf[..colon]).ok()?.parse().ok()?;
    let head = buf.get(colon + 1..colon + 1 + len)?;
    if buf.get(colon + 1 + len) != Some(&b',') {
        return None;
    }
    let fields: Vec<String> = head
        .split(|&b| b == 0)
        .map(|f| String::from_utf8_lossy(f).into_owned())
        .collect();
    let headers: Vec<(String, String)> = fields
        .as_chunks::<2>()
        .0
        .iter()
        .map(|[k, v]| (k.clone(), v.clone()))
        .collect();
    let body_len: usize = headers
        .iter()
        .find(|(n, _)| n == "CONTENT_LENGTH")?
        .1
        .parse()
        .ok()?;
    let start = colon + 2 + len;
    let body = buf.get(start..start + body_len)?.to_vec();
    Some(ScgiRequest { headers, body })
}
