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
        ..Limits::default()
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

#[tokio::test]
async fn an_http_link_may_redirect_to_https() {
    let (config, pem) = self_signed();
    let secure = FileServer::start_tls(config).await.expect("bind");
    secure.route("/a.dat", FileRoute::ok(b"<datafile/>".to_vec()));
    let plain = FileServer::start().await.expect("bind");
    plain.route("/up", FileRoute::redirect(301, &secure.url("/a.dat")));
    let dir = tempfile::tempdir().expect("tempdir");
    let ca = dir.path().join("ca.pem");
    std::fs::write(&ca, pem).expect("write");
    let f = Fetcher::new(Roots::from_pem_file(&ca).expect("roots"), Limits::default())
        .expect("fetcher");
    let r = get(&f, &plain.url("/up")).await.expect("followed");
    assert_eq!(body_of(r).await.expect("body"), b"<datafile/>");
}

#[tokio::test]
async fn a_public_link_may_not_redirect_to_a_local_address() {
    let server = FileServer::start().await.expect("bind");
    let port = server.addr().port();
    server.route(
        "/in",
        FileRoute::redirect(302, &format!("http://127.0.0.2:{port}/a.dat")),
    );
    server.route("/a.dat", FileRoute::ok(b"x".to_vec()));
    let public_first = fetcher().with_local(|ip| ip != IpAddr::from([127, 0, 0, 1]));
    let e = get(&public_first, &server.url("/in"))
        .await
        .err()
        .expect("refused");
    assert!(matches!(e, FetchError::LocalRedirect), "{e:?}");
    assert_eq!(server.hits(), vec!["/in".to_owned()]);
    let lan = fetcher();
    server.route("/in", FileRoute::redirect(302, &server.url("/a.dat")));
    assert!(
        get(&lan, &server.url("/in")).await.is_ok(),
        "a typed LAN link may"
    );
}

#[tokio::test]
async fn a_body_under_the_minimum_rate_fails() {
    let server = FileServer::start().await.expect("bind");
    server.route(
        "/slow",
        FileRoute::ok(vec![7; 256]).paced(4, Duration::from_millis(40)),
    );
    let limits = Limits {
        min_rate: 1 << 20,
        window: Duration::from_millis(150),
        ..Limits::default()
    };
    let f = Fetcher::new(Roots::bundled(), limits).expect("fetcher");
    let e = body_of(get(&f, &server.url("/slow")).await.expect("head"))
        .await
        .expect_err("slow");
    assert!(matches!(e, FetchError::TooSlow(_)), "{e:?}");
    let e = FetchError::TooSlow(MIN_RATE);
    assert_eq!(
        e.to_string(),
        "The server sent the file too slowly, under 1024 bytes a second."
    );
}

#[tokio::test]
async fn a_compressed_body_is_refused() {
    let server = FileServer::start().await.expect("bind");
    server.route(
        "/gz",
        FileRoute::ok(vec![0x1f, 0x8b, 8]).with_header("Content-Encoding", "gzip"),
    );
    server.route(
        "/id",
        FileRoute::ok(b"x".to_vec()).with_header("Content-Encoding", "identity"),
    );
    let e = get(&fetcher(), &server.url("/gz"))
        .await
        .err()
        .expect("gzip");
    assert_eq!(
        e.to_string(),
        "The server sent a compressed file mistarr can't read."
    );
    assert!(get(&fetcher(), &server.url("/id")).await.is_ok());
}

#[test]
fn local_addresses_are_recognised() {
    for local in [
        "10.1.2.3",
        "172.16.0.1",
        "169.254.1.1",
        "100.64.0.1",
        "0.0.0.0",
        "::1",
        "fd00::1",
        "fe80::1",
        "::ffff:192.168.0.1",
    ] {
        assert!(is_local(local.parse().expect("ip")), "{local}");
    }
    for public in [
        "192.0.2.1",
        "100.128.0.1",
        "2001:db8::1",
        "::ffff:198.51.100.1",
    ] {
        assert!(!is_local(public.parse().expect("ip")), "{public}");
    }
}
