//! SCGI request framing and transport for rtorrent; see `docs/DOWNLOAD-CLIENTS.md` "rtorrent".

use std::ops::Range;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::detect::ScgiAddr;
use crate::ClientError;

/// Largest response accepted; an `f.multicall` over 10 000 files is a few MiB.
const MAX_RESPONSE: usize = 16 * 1024 * 1024;

/// Frames `body` as an SCGI request: a netstring of `CONTENT_LENGTH` and
/// `SCGI=1` headers, each NUL-terminated, followed by the body.
pub(crate) fn frame(body: &[u8]) -> Vec<u8> {
    let headers = format!("CONTENT_LENGTH\0{}\0SCGI\x001\0", body.len());
    let mut out = format!("{}:{headers},", headers.len()).into_bytes();
    out.extend_from_slice(body);
    out
}

/// Where the body of an SCGI response starts and ends, after its CGI-style
/// headers. A `Status` other than 200 or a body shorter than `Content-Length` is an error.
pub(crate) fn body_range(response: &[u8]) -> Result<Range<usize>, ClientError> {
    let (head_end, start) = [&b"\r\n\r\n"[..], b"\n\n"]
        .iter()
        .filter_map(|sep| {
            response
                .windows(sep.len())
                .position(|w| w == *sep)
                .map(|i| (i, i + sep.len()))
        })
        .min_by_key(|(head_end, _)| *head_end)
        .ok_or_else(|| ClientError::Protocol("SCGI response without headers".into()))?;
    let head = String::from_utf8_lossy(&response[..head_end]);
    let mut body = start..response.len();
    for line in head.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("status") && !value.starts_with("200") {
            return Err(ClientError::Protocol(format!("SCGI status {value}")));
        }
        if name.eq_ignore_ascii_case("content-length") {
            let len: usize = value
                .parse()
                .map_err(|_| ClientError::Protocol(format!("bad Content-Length {value:?}")))?;
            if body.len() < len {
                return Err(ClientError::Protocol("truncated SCGI response".into()));
            }
            body.end = start + len;
        }
    }
    Ok(body)
}

/// Sends one request on a fresh connection and returns the response body,
/// all within `timeout`. Connection failures and timeouts are `Unreachable`.
pub(crate) async fn request(
    addr: &ScgiAddr,
    body: &[u8],
    timeout: Duration,
) -> Result<Vec<u8>, ClientError> {
    let name = match addr {
        ScgiAddr::Tcp(a) => a.clone(),
        ScgiAddr::Unix(p) => p.display().to_string(),
    };
    let unreachable = |e: &dyn std::fmt::Display| ClientError::Unreachable(format!("{name}: {e}"));
    let framed = frame(body);
    let work = async {
        let mut raw = match addr {
            ScgiAddr::Tcp(a) => {
                let stream = TcpStream::connect(a).await.map_err(|e| unreachable(&e))?;
                exchange(stream, &framed).await
            }
            #[cfg(unix)]
            ScgiAddr::Unix(p) => {
                let stream = tokio::net::UnixStream::connect(p)
                    .await
                    .map_err(|e| unreachable(&e))?;
                exchange(stream, &framed).await
            }
            #[cfg(not(unix))]
            ScgiAddr::Unix(_) => return Err(unreachable(&"unix sockets are not supported")),
        }
        .map_err(|e| unreachable(&e))?;
        if raw.len() > MAX_RESPONSE {
            return Err(ClientError::Protocol(format!(
                "response larger than {MAX_RESPONSE} bytes"
            )));
        }
        let body = body_range(&raw)?;
        raw.truncate(body.end);
        raw.drain(..body.start);
        Ok(raw)
    };
    tokio::time::timeout(timeout, work)
        .await
        .map_err(|_| ClientError::Unreachable(format!("{name} timed out")))?
}

async fn exchange<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    framed: &[u8],
) -> std::io::Result<Vec<u8>> {
    stream.write_all(framed).await?;
    stream.flush().await?;
    let mut raw = Vec::new();
    let limit = u64::try_from(MAX_RESPONSE).unwrap_or(u64::MAX) + 1;
    (&mut stream).take(limit).read_to_end(&mut raw).await?;
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[test]
    fn frames_byte_exact() {
        assert_eq!(
            frame(b"<x/>"),
            b"24:CONTENT_LENGTH\x004\x00SCGI\x001\x00,<x/>".to_vec()
        );
        assert_eq!(
            frame(b""),
            b"24:CONTENT_LENGTH\x000\x00SCGI\x001\x00,".to_vec()
        );
        let body = vec![b'a'; 1234];
        let framed = frame(&body);
        assert!(framed.starts_with(b"27:CONTENT_LENGTH\x001234\x00SCGI\x001\x00,a"));
        assert_eq!(framed.len(), 3 + 27 + 1 + 1234);
    }

    #[test]
    fn strips_response_headers() {
        let body = |r: &[u8]| r[body_range(r).expect("ok")].to_vec();
        let r = b"Status: 200 OK\r\nContent-Type: text/xml\r\nContent-Length: 4\r\n\r\n<x/>";
        assert_eq!(body(r), b"<x/>");
        assert_eq!(body(b"Content-Type: text/xml\n\n<y/>"), b"<y/>");
        assert_eq!(body(b"Content-Length: 2\r\n\r\nabcd"), b"ab");
    }

    #[test]
    fn rejects_bad_responses() {
        assert!(body_range(b"<x/>").is_err());
        assert!(body_range(b"Status: 500 Internal\r\n\r\n").is_err());
        assert!(body_range(b"Content-Length: 9\r\n\r\nab").is_err());
        assert!(body_range(b"Content-Length: x\r\n\r\nab").is_err());
    }

    #[tokio::test]
    async fn request_sends_frame_and_returns_body() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = ScgiAddr::Tcp(listener.local_addr().expect("addr").to_string());
        let server = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.expect("accept");
            let mut got = vec![0u8; 32];
            s.read_exact(&mut got).await.expect("read");
            s.write_all(b"Status: 200 OK\r\nContent-Length: 3\r\n\r\nyes")
                .await
                .expect("write");
            got
        });
        let body = request(&addr, b"<x/>", Duration::from_secs(5))
            .await
            .expect("answered");
        assert_eq!(body, b"yes");
        assert_eq!(
            server.await.expect("join"),
            b"24:CONTENT_LENGTH\x004\x00SCGI\x001\x00,<x/>".to_vec()
        );
    }

    #[tokio::test]
    async fn refused_and_silent_are_unreachable() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let open = ScgiAddr::Tcp(listener.local_addr().expect("addr").to_string());
        let err = request(&open, b"", Duration::from_millis(100))
            .await
            .expect_err("silent");
        assert!(matches!(err, ClientError::Unreachable(_)), "{err:?}");
        drop(listener);
        let err = request(&open, b"", Duration::from_secs(5))
            .await
            .expect_err("refused");
        assert!(matches!(err, ClientError::Unreachable(_)), "{err:?}");
    }
}
