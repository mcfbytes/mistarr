use std::sync::Arc;
use std::time::Duration;

use super::*;
use crate::fake::{FileRoute, FileServer};

fn fetcher() -> Fetcher {
    Fetcher::new(Roots::bundled(), Limits::default()).expect("fetcher")
}

async fn body_of(mut r: Response) -> Result<Vec<u8>, FetchError> {
    let mut out = Vec::new();
    while let Some(c) = r.chunk().await? {
        out.extend_from_slice(&c);
    }
    Ok(out)
}

async fn get(f: &Fetcher, url: &str) -> Result<Response, FetchError> {
    f.get(&FetchUrl::parse(url).expect("url")).await
}

#[tokio::test]
async fn a_body_arrives_with_its_length_and_name() {
    let server = FileServer::start().await.expect("bind");
    server.route("/d/pack.zip?v=1", FileRoute::ok(b"PK\x03\x04rest".to_vec()));
    server.route(
        "/named",
        FileRoute::ok(b"x".to_vec())
            .with_header("Content-Disposition", "attachment; filename=\"Set A.dat\""),
    );
    let r = get(&fetcher(), &server.url("/d/pack.zip?v=1"))
        .await
        .expect("ok");
    assert_eq!(r.content_length(), Some(8));
    assert_eq!(r.file_name(), Some("pack.zip"));
    assert_eq!(body_of(r).await.expect("body"), b"PK\x03\x04rest");
    let r = get(&fetcher(), &server.url("/named")).await.expect("ok");
    assert_eq!(r.file_name(), Some("Set A.dat"));
    let e = get(&fetcher(), &server.url("/missing"))
        .await
        .err()
        .expect("404");
    assert!(matches!(e, FetchError::Status(404)), "{e:?}");
    assert_eq!(server.hits(), ["/d/pack.zip?v=1", "/named", "/missing"]);
}

#[tokio::test]
async fn five_redirects_are_followed_and_a_sixth_is_refused() {
    let server = FileServer::start().await.expect("bind");
    for i in 0..6 {
        server.route(
            &format!("/r{i}"),
            FileRoute::redirect(302, &format!("/r{}", i + 1)),
        );
    }
    server.route("/r6", FileRoute::ok(b"end".to_vec()));
    let r = get(&fetcher(), &server.url("/r1"))
        .await
        .expect("five hops");
    assert_eq!(r.file_name(), Some("r6"));
    assert_eq!(body_of(r).await.expect("body"), b"end");
    let e = get(&fetcher(), &server.url("/r0"))
        .await
        .err()
        .expect("six hops");
    assert!(matches!(e, FetchError::Redirects), "{e:?}");
    let hits = server.hits();
    assert_eq!(hits.iter().filter(|h| *h == "/r6").count(), 1, "{hits:?}");
    server.route(
        "/nowhere",
        FileRoute::redirect(301, "ftp://example.invalid/a"),
    );
    let e = get(&fetcher(), &server.url("/nowhere"))
        .await
        .err()
        .expect("bad target");
    assert!(matches!(e, FetchError::Url(_)), "{e:?}");
}

#[tokio::test]
async fn a_stalled_body_times_out() {
    let server = FileServer::start().await.expect("bind");
    server.route(
        "/slow",
        FileRoute::ok(vec![7; 64]).paced(8, Duration::from_millis(400)),
    );
    let limits = Limits {
        connect: CONNECT_TIMEOUT,
        idle: Duration::from_millis(100),
    };
    let f = Fetcher::new(Roots::bundled(), limits).expect("fetcher");
    let e = body_of(get(&f, &server.url("/slow")).await.expect("head"))
        .await
        .expect_err("stalled");
    assert!(matches!(e, FetchError::Stalled(_)), "{e:?}");
}

#[tokio::test]
async fn a_short_body_is_an_error() {
    let server = FileServer::start().await.expect("bind");
    let mut route = FileRoute::ok(vec![1; 10]);
    route.length = Some(20);
    server.route("/short", route);
    let e = body_of(get(&fetcher(), &server.url("/short")).await.expect("head"))
        .await
        .expect_err("short");
    assert!(matches!(e, FetchError::Transfer(_)), "{e:?}");
    server.route("/open", FileRoute::ok(vec![2; 5]).without_length());
    let r = get(&fetcher(), &server.url("/open")).await.expect("head");
    assert_eq!(r.content_length(), None);
    assert_eq!(body_of(r).await.expect("body"), vec![2; 5]);
}

#[tokio::test]
async fn a_refused_connection_names_no_host() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    drop(listener);
    let e = get(&fetcher(), &format!("http://{addr}/a.dat"))
        .await
        .err()
        .expect("refused");
    assert!(matches!(e, FetchError::Connect(_)), "{e:?}");
    assert!(!e.to_string().contains("127.0.0.1"), "{e}");
}

/// A server config for a self-signed certificate of `127.0.0.1`, and that certificate as PEM.
fn self_signed() -> (rustls::ServerConfig, String) {
    let key =
        rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_owned(), "localhost".to_owned()])
            .expect("cert");
    let der = rustls::pki_types::PrivateKeyDer::Pkcs8(key.signing_key.serialize_der().into());
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("versions")
        .with_no_client_auth()
        .with_single_cert(vec![key.cert.der().clone()], der)
        .expect("server config");
    (config, key.cert.pem())
}

#[tokio::test]
async fn https_works_with_an_injected_root_and_refuses_without_it() {
    let (config, pem) = self_signed();
    let server = FileServer::start_tls(config).await.expect("bind");
    server.route("/a.torrent", FileRoute::ok(b"d4:infod4:name1:aee".to_vec()));
    server.route(
        "/down",
        FileRoute::redirect(302, "http://127.0.0.1:9/a.torrent"),
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let ca = dir.path().join("ca.pem");
    std::fs::write(&ca, pem).expect("write");
    let trusted = Fetcher::new(Roots::from_pem_file(&ca).expect("roots"), Limits::default())
        .expect("fetcher");
    let r = get(&trusted, &server.url("/a.torrent")).await.expect("tls");
    assert_eq!(body_of(r).await.expect("body"), b"d4:infod4:name1:aee");
    let e = get(&trusted, &server.url("/down"))
        .await
        .err()
        .expect("downgrade");
    assert!(matches!(e, FetchError::Downgrade), "{e:?}");
    let e = get(&fetcher(), &server.url("/a.torrent"))
        .await
        .err()
        .expect("untrusted");
    assert!(matches!(e, FetchError::Tls(_)), "{e:?}");
    assert_eq!(e.to_string(), "The server's certificate is not trusted.");
    let by_name = server.url("/a.torrent").replace("127.0.0.1", "localhost");
    assert!(
        get(&trusted, &by_name).await.is_ok(),
        "the name is in the certificate too"
    );
}

#[test]
fn disposition_names_are_read() {
    assert_eq!(
        disposition_file_name("attachment; filename=a.dat").as_deref(),
        Some("a.dat")
    );
    assert_eq!(
        disposition_file_name("attachment; filename=\"\"").as_deref(),
        None
    );
    assert_eq!(
        disposition_file_name("attachment; FILENAME*=utf-8'en'x%20y.zip").as_deref(),
        Some("x y.zip")
    );
}
