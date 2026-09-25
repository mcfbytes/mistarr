//! A minimal HTTP `BitTorrent` tracker for local tests: `announce` records the
//! caller and answers with the other peers of the swarm in compact form.

use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use mistarr_sources::bencode::{encode, Value};

use crate::{Error, Result};

/// Path the tracker answers announces on.
pub const ANNOUNCE_PATH: &str = "/announce";
/// Re-announce interval handed to clients, in seconds; short so a peer that
/// failed to connect is offered again quickly.
pub const INTERVAL_SECS: i64 = 2;

const MAX_REQUEST: usize = 8 * 1024;

#[derive(Debug, Clone, Copy)]
struct Peer {
    addr: SocketAddrV4,
    seeding: bool,
}

/// Peers per infohash, keyed by peer id.
type Swarms = HashMap<[u8; 20], HashMap<Vec<u8>, Peer>>;

/// This host's IPv4 address on the default route, which local clients accept
/// as a peer address; `None` without one. No packet is sent.
///
/// ```
/// if let Some(ip) = mistarr_fixture::tracker::local_ipv4() {
///     assert!(!ip.is_loopback());
/// }
/// ```
#[must_use]
pub fn local_ipv4() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if !ip.is_loopback() && !ip.is_unspecified() => Some(ip),
        _ => None,
    }
}

/// A running tracker; it stops when dropped.
#[derive(Debug)]
pub struct Tracker {
    addr: SocketAddr,
    swarms: Arc<Mutex<Swarms>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Tracker {
    /// Listens on `listen` (use port 0 for an ephemeral one) and serves on a
    /// background thread. Peers announcing from loopback are handed out as
    /// `loopback_as` when given, since Transmission refuses loopback peers.
    ///
    /// ```
    /// let t = mistarr_fixture::tracker::Tracker::start("127.0.0.1:0", None).unwrap();
    /// assert!(t.announce_url().starts_with("http://127.0.0.1:"));
    /// ```
    ///
    /// # Errors
    ///
    /// [`Error::Listen`] when the address cannot be bound.
    pub fn start(listen: &str, loopback_as: Option<Ipv4Addr>) -> Result<Self> {
        let listen_err = |source| Error::Listen {
            addr: listen.to_owned(),
            source,
        };
        let listener = TcpListener::bind(listen).map_err(listen_err)?;
        let addr = listener.local_addr().map_err(listen_err)?;
        let swarms = Arc::new(Mutex::new(Swarms::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (swarms, stop) = (Arc::clone(&swarms), Arc::clone(&stop));
            std::thread::Builder::new()
                .name("tracker-accept".into())
                .spawn(move || accept(&listener, &swarms, &stop, loopback_as))
                .map_err(listen_err)?
        };
        Ok(Self {
            addr,
            swarms,
            stop,
            thread: Some(thread),
        })
    }

    /// The bound address.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The URL to put in a torrent's `announce` field.
    #[must_use]
    pub fn announce_url(&self) -> String {
        format!("http://{}{ANNOUNCE_PATH}", self.addr)
    }

    /// Peers that announced `infohash` and have not stopped, with whether each is seeding.
    #[must_use]
    pub fn peers(&self, infohash: &[u8; 20]) -> Vec<(SocketAddrV4, bool)> {
        let swarms = self.swarms.lock().unwrap_or_else(PoisonError::into_inner);
        let mut out: Vec<_> = swarms
            .get(infohash)
            .map(|s| s.values().map(|p| (p.addr, p.seeding)).collect())
            .unwrap_or_default();
        out.sort();
        out
    }
}

impl Drop for Tracker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wakes the blocking accept so the thread sees the flag.
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_secs(1));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn accept(
    listener: &TcpListener,
    swarms: &Arc<Mutex<Swarms>>,
    stop: &AtomicBool,
    loopback_as: Option<Ipv4Addr>,
) {
    for conn in listener.incoming() {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let Ok(stream) = conn else { continue };
        let swarms = Arc::clone(swarms);
        // A connection the fixture cannot serve is dropped, as a busy tracker would.
        let _ = std::thread::Builder::new()
            .name("tracker-conn".into())
            .spawn(move || {
                let _ = serve(stream, &swarms, loopback_as);
            });
    }
}

fn serve(
    mut stream: TcpStream,
    swarms: &Mutex<Swarms>,
    loopback_as: Option<Ipv4Addr>,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < MAX_REQUEST {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let from = match (stream.peer_addr()?.ip(), loopback_as) {
        (ip, Some(v4)) if ip.is_loopback() => IpAddr::V4(v4),
        (ip, _) => ip,
    };
    let (status, body) = respond(&buf, from, swarms);
    let head = format!(
        "HTTP/1.0 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(&body)
}

/// The status line and body for one raw request from `from`.
fn respond(request: &[u8], from: IpAddr, swarms: &Mutex<Swarms>) -> (&'static str, Vec<u8>) {
    let line = request.split(|b| *b == b'\r').next().unwrap_or_default();
    let line = String::from_utf8_lossy(line);
    let mut parts = line.split(' ');
    let (Some("GET"), Some(target)) = (parts.next(), parts.next()) else {
        return ("400 Bad Request", Vec::new());
    };
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != ANNOUNCE_PATH {
        return ("404 Not Found", Vec::new());
    }
    let body = match Announce::parse(query, from) {
        Ok(a) => a.apply(swarms),
        Err(reason) => dict(vec![("failure reason", Value::Bytes(reason.into()))]),
    };
    ("200 OK", encode(&body))
}

fn dict(entries: Vec<(&str, Value)>) -> Value {
    Value::Dict(
        entries
            .into_iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v))
            .collect::<BTreeMap<_, _>>(),
    )
}

/// The fields of one announce that the tracker uses.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Announce {
    info_hash: [u8; 20],
    peer_id: Vec<u8>,
    addr: SocketAddrV4,
    left: Option<u64>,
    stopped: bool,
}

impl Announce {
    fn parse(query: &str, from: IpAddr) -> std::result::Result<Self, &'static str> {
        let mut info_hash = None;
        let mut peer_id = None;
        let mut port = None;
        let mut left = None;
        let mut stopped = false;
        for pair in query.split('&') {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            let v = percent_decode(v).ok_or("malformed query")?;
            match k {
                "info_hash" => {
                    info_hash = Some(<[u8; 20]>::try_from(v).map_err(|_| "bad info_hash")?);
                }
                "peer_id" => peer_id = Some(v),
                "port" => {
                    port = std::str::from_utf8(&v)
                        .ok()
                        .and_then(|p| p.parse::<u16>().ok());
                }
                "left" => left = std::str::from_utf8(&v).ok().and_then(|l| l.parse().ok()),
                "event" => stopped = v == b"stopped",
                _ => {}
            }
        }
        let IpAddr::V4(ip) = from else {
            return Err("IPv4 only");
        };
        Ok(Self {
            info_hash: info_hash.ok_or("missing info_hash")?,
            peer_id: peer_id.ok_or("missing peer_id")?,
            addr: SocketAddrV4::new(ip, port.ok_or("missing port")?),
            left,
            stopped,
        })
    }

    fn apply(self, swarms: &Mutex<Swarms>) -> Value {
        let mut swarms = swarms.lock().unwrap_or_else(PoisonError::into_inner);
        let swarm = swarms.entry(self.info_hash).or_default();
        if self.stopped {
            swarm.remove(&self.peer_id);
        } else {
            let peer = Peer {
                addr: self.addr,
                seeding: self.left == Some(0),
            };
            swarm.insert(self.peer_id.clone(), peer);
        }
        let mut compact = Vec::new();
        for (_, p) in swarm.iter().filter(|(id, _)| **id != self.peer_id) {
            compact.extend_from_slice(&p.addr.ip().octets());
            compact.extend_from_slice(&p.addr.port().to_be_bytes());
        }
        let seeders = swarm.values().filter(|p| p.seeding).count();
        let count = |n: usize| Value::Int(i64::try_from(n).unwrap_or(i64::MAX));
        dict(vec![
            ("interval", Value::Int(INTERVAL_SECS)),
            ("min interval", Value::Int(INTERVAL_SECS)),
            ("complete", count(seeders)),
            ("incomplete", count(swarm.len() - seeders)),
            ("peers", Value::Bytes(compact)),
        ])
    }
}

/// Decodes `%XX` escapes and `+`; `None` for a truncated or non-hex escape.
fn percent_decode(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use std::fmt::Write as _;

    use mistarr_sources::bencode::decode;

    use super::*;

    const LOCAL: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

    fn escaped(bytes: &[u8]) -> String {
        bytes.iter().fold(String::new(), |mut s, b| {
            let _ = write!(s, "%{b:02X}");
            s
        })
    }

    fn query(hash: &[u8; 20], peer: &str, port: u16, extra: &str) -> String {
        format!(
            "info_hash={}&peer_id={peer}&port={port}&left=0&compact=1{extra}",
            escaped(hash)
        )
    }

    fn get(t: &Tracker, target: &str) -> (String, Vec<u8>) {
        let mut s = TcpStream::connect(t.addr()).unwrap();
        write!(s, "GET {target} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut raw = Vec::new();
        s.read_to_end(&mut raw).unwrap();
        let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let head = String::from_utf8_lossy(&raw[..split]).into_owned();
        (head, raw[split + 4..].to_vec())
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("a%20b+c").unwrap(), b"a b c");
        assert_eq!(percent_decode("%ff%00").unwrap(), [0xff, 0]);
        assert!(percent_decode("%f").is_none());
        assert!(percent_decode("%zz").is_none());
    }

    #[test]
    fn second_peer_learns_the_first() {
        let t = Tracker::start("127.0.0.1:0", None).unwrap();
        let hash = [7u8; 20];
        let first = format!("{ANNOUNCE_PATH}?{}", query(&hash, "peer-one", 51413, ""));
        let (head, body) = get(&t, &first);
        assert!(head.starts_with("HTTP/1.0 200"), "{head}");
        let v = decode(&body).unwrap();
        assert_eq!(v.get("peers").unwrap().as_bytes(), Some(&[][..]));
        assert_eq!(v.get("interval").unwrap().as_int(), Some(INTERVAL_SECS));

        let second = format!("{ANNOUNCE_PATH}?{}", query(&hash, "peer-two", 6881, ""));
        let v = decode(&get(&t, &second).1).unwrap();
        let expected = [127, 0, 0, 1, 0xc8, 0xd5];
        assert_eq!(v.get("peers").unwrap().as_bytes(), Some(&expected[..]));
        assert_eq!(v.get("complete").unwrap().as_int(), Some(2));
        assert_eq!(t.peers(&hash).len(), 2);

        let stop = format!(
            "{ANNOUNCE_PATH}?{}",
            query(&hash, "peer-one", 51413, "&event=stopped")
        );
        let _ = get(&t, &stop);
        assert_eq!(
            t.peers(&hash),
            [(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 6881), true)]
        );
        assert!(t.peers(&[0u8; 20]).is_empty());
    }

    #[test]
    fn loopback_peers_are_advertised_as_the_given_address() {
        let lan = Ipv4Addr::new(192, 0, 2, 7);
        let t = Tracker::start("127.0.0.1:0", Some(lan)).unwrap();
        let hash = [9u8; 20];
        let _ = get(
            &t,
            &format!("{ANNOUNCE_PATH}?{}", query(&hash, "p", 7000, "")),
        );
        assert_eq!(t.peers(&hash), [(SocketAddrV4::new(lan, 7000), true)]);
    }

    #[test]
    fn local_ipv4_is_never_loopback() {
        assert!(local_ipv4().is_none_or(|ip| !ip.is_loopback()));
    }

    #[test]
    fn other_paths_are_not_found() {
        let t = Tracker::start("127.0.0.1:0", None).unwrap();
        assert!(get(&t, "/scrape").0.starts_with("HTTP/1.0 404"));
    }

    #[test]
    fn incomplete_announces_get_a_failure_reason() {
        let swarms = Mutex::new(Swarms::new());
        let (status, body) = respond(b"GET /announce?port=1 HTTP/1.1\r\n\r\n", LOCAL, &swarms);
        assert_eq!(status, "200 OK");
        let v = decode(&body).unwrap();
        assert!(v.get("failure reason").is_some());
        assert_eq!(
            respond(b"POST / HTTP/1.1", LOCAL, &swarms).0,
            "400 Bad Request"
        );
    }

    #[test]
    fn announce_url_uses_the_bound_port() {
        let t = Tracker::start("127.0.0.1:0", None).unwrap();
        let url = t.announce_url();
        assert_eq!(
            url,
            format!("http://127.0.0.1:{}{ANNOUNCE_PATH}", t.addr().port())
        );
    }
}
