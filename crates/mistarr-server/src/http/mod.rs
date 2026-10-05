//! The axum application: `/api/v1` routes from `docs/API.md` and the embedded SPA.

pub mod catalog;
mod dats;
mod downloads;
mod error;
mod events;
mod extract;
mod fetch;
mod imports;
mod launch;
mod platforms;
mod sources;
mod spa;
mod system;

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::Router;

use crate::app::AppState;

pub use error::{ApiError, Code};
pub use extract::{ApiJson, ApiPath, ApiQuery, OptionalJson, Paging};

/// Header carrying the API key.
pub const API_KEY_HEADER: &str = "x-api-key";

/// Header every state-changing request must carry as `1`; a cross-site form cannot set it.
pub const GUARD_HEADER: &str = "x-mistarr";

/// Builds the whole application.
pub fn router(app: Arc<AppState>) -> Router {
    let key = Some(app.config().server.api_key.clone()).filter(|k| !k.is_empty());
    let api = Router::new()
        .merge(system::routes())
        .merge(events::routes())
        .merge(sources::routes())
        .merge(platforms::routes())
        .merge(catalog::routes())
        .merge(dats::routes())
        .merge(fetch::routes())
        .merge(imports::routes())
        .merge(downloads::routes())
        .merge(launch::routes())
        .fallback(api_not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(middleware::from_fn_with_state(
            Arc::new(HostAllowlist::for_board(&app.config().server.allowed_hosts)),
            refuse_cross_site,
        ))
        .layer(middleware::from_fn_with_state(key, require_key))
        .layer(middleware::from_fn(no_store));
    Router::new()
        .nest("/api/v1", api)
        .fallback(spa::serve)
        .with_state(app)
}

/// Marks every API answer `Cache-Control: no-store`, so no browser or proxy ever
/// answers a later request with a copy from before the catalogue changed.
async fn no_store(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

async fn require_key(
    State(key): State<Option<String>>,
    headers: HeaderMap,
    req: Request,
    next: Next,
) -> Response {
    let Some(key) = key else {
        return next.run(req).await;
    };
    let from_header = headers.get(API_KEY_HEADER).and_then(|v| v.to_str().ok());
    // EventSource cannot send headers, so the key may also come as `?apikey=`.
    let from_query = req
        .uri()
        .query()
        .and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("apikey=")));
    if from_header
        .or(from_query)
        .is_some_and(|given| same(given, &key))
    {
        next.run(req).await
    } else {
        ApiError::unauthorized("The API key is missing or wrong.").into_response()
    }
}

/// The `Host` names a state-changing request may address, against DNS
/// rebinding: IP literals, `localhost`, `*.local`, `*.lan`, the board's own
/// host name and the configured `server.allowed_hosts`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostAllowlist {
    exact: Vec<String>,
    suffixes: Vec<String>,
}

impl HostAllowlist {
    /// The built-in names plus `hostname` and `extra`; a `*.name` entry allows subdomains.
    ///
    /// ```
    /// use mistarr_server::http::HostAllowlist;
    /// let allow = HostAllowlist::new(Some("mister"), &["*.home.arpa".to_owned()]);
    /// assert!(allow.allows("MiSTer:8420") && allow.allows("192.168.1.9:8420"));
    /// assert!(allow.allows("nas.home.arpa") && !allow.allows("evil.example"));
    /// ```
    #[must_use]
    pub fn new(hostname: Option<&str>, extra: &[String]) -> Self {
        let mut allow = Self {
            exact: vec!["localhost".to_owned()],
            suffixes: vec![
                ".local".to_owned(),
                ".lan".to_owned(),
                ".localhost".to_owned(),
            ],
        };
        for name in hostname.into_iter().chain(extra.iter().map(String::as_str)) {
            let name = name.trim().trim_end_matches('.').to_ascii_lowercase();
            match name.strip_prefix('*') {
                Some(suffix) if suffix.starts_with('.') => allow.suffixes.push(suffix.to_owned()),
                _ if !name.is_empty() => allow.exact.push(name),
                _ => {}
            }
        }
        allow
    }

    /// [`HostAllowlist::new`] with this machine's host name.
    ///
    /// ```
    /// let allow = mistarr_server::http::HostAllowlist::for_board(&[]);
    /// assert!(allow.allows("localhost"));
    /// ```
    #[must_use]
    pub fn for_board(extra: &[String]) -> Self {
        let hostname = std::fs::read_to_string("/proc/sys/kernel/hostname").ok();
        Self::new(hostname.as_deref(), extra)
    }

    /// True when a `Host` header value, with or without a port, is allowed.
    ///
    /// ```
    /// let allow = mistarr_server::http::HostAllowlist::new(None, &[]);
    /// assert!(allow.allows("[::1]:8420") && !allow.allows("example.com"));
    /// ```
    #[must_use]
    pub fn allows(&self, host: &str) -> bool {
        let host = host.trim();
        let name = if let Some(rest) = host.strip_prefix('[') {
            rest.split_once(']').map_or(rest, |(ip, _)| ip)
        } else {
            match host.rsplit_once(':') {
                Some((name, port))
                    if !name.contains(':') && port.bytes().all(|b| b.is_ascii_digit()) =>
                {
                    name
                }
                _ => host,
            }
        };
        let name = name.trim_end_matches('.').to_ascii_lowercase();
        name.parse::<std::net::IpAddr>().is_ok()
            || self.exact.contains(&name)
            || self
                .suffixes
                .iter()
                .any(|s| name.len() > s.len() && name.ends_with(s.as_str()))
    }
}

/// Refuses state-changing requests another site could have sent; see `docs/API.md`.
async fn refuse_cross_site(
    State(allow): State<Arc<HostAllowlist>>,
    req: Request,
    next: Next,
) -> Response {
    if matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS) {
        return next.run(req).await;
    }
    match cross_site(req.headers(), &allow) {
        None => next.run(req).await,
        Some(message) => ApiError::forbidden(message).into_response(),
    }
}

/// Why a state-changing request with these headers is refused, or `None` to allow it.
fn cross_site(headers: &HeaderMap, allow: &HostAllowlist) -> Option<&'static str> {
    let get = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    if !get("host").is_some_and(|h| allow.allows(h)) {
        return Some("The request's Host is not a name this server answers to.");
    }
    if get("sec-fetch-site").is_some_and(|v| v.eq_ignore_ascii_case("cross-site")) {
        return Some("Requests from another site are refused.");
    }
    if let Some(origin) = get("origin") {
        let origin_host = origin.split_once("://").map(|(_, host)| host);
        let same = origin_host
            .zip(get("host"))
            .is_some_and(|(o, h)| o.eq_ignore_ascii_case(h));
        if !same {
            return Some("The request's Origin does not match its Host.");
        }
    }
    if get(GUARD_HEADER) != Some("1") {
        return Some("State-changing requests need the X-Mistarr: 1 header.");
    }
    None
}

/// Compares without an early exit on the first differing byte.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0, |acc, (x, y)| acc | (x ^ y))
            == 0
}

async fn api_not_found() -> ApiError {
    ApiError::no_such("API route")
}

async fn method_not_allowed() -> ApiError {
    ApiError::method_not_allowed()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_comparison() {
        assert!(same("abc", "abc"));
        assert!(!same("abc", "abd"));
        assert!(!same("abc", "abcd"));
    }

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, v.parse().expect("value"));
        }
        h
    }

    fn cross_site(h: &HeaderMap) -> Option<&'static str> {
        super::cross_site(h, &HostAllowlist::new(Some("board"), &["b".to_owned()]))
    }

    #[test]
    fn hosts_outside_the_allowlist_are_refused() {
        let extra = ["nas.example".to_owned(), "*.home.arpa".to_owned()];
        let allow = HostAllowlist::new(Some("mister\n"), &extra);
        for ok in [
            "127.0.0.1",
            "10.0.0.5:8420",
            "[::1]:8420",
            "::1",
            "localhost:8420",
            "MiSTer.local",
            "mister.lan:80",
            "MISTER:8420",
            "nas.example",
            "a.home.arpa",
            "mister.",
        ] {
            assert!(allow.allows(ok), "{ok}");
        }
        for bad in [
            "evil.example",
            "local",
            ".lan",
            "home.arpa",
            "",
            "mister.evil.example",
        ] {
            assert!(!allow.allows(bad), "{bad}");
        }
        let rebound = [("host", "evil.example:8420"), ("x-mistarr", "1")];
        assert!(cross_site(&headers(&rebound)).is_some());
    }

    #[test]
    fn cross_site_writes_are_refused() {
        let ok = [("host", "board:8420"), ("x-mistarr", "1")];
        assert_eq!(cross_site(&headers(&ok)), None);
        let same_origin = [
            ("host", "board:8420"),
            ("origin", "http://Board:8420"),
            ("x-mistarr", "1"),
        ];
        assert_eq!(cross_site(&headers(&same_origin)), None);
        let same_site = [
            ("host", "b"),
            ("sec-fetch-site", "same-origin"),
            ("x-mistarr", "1"),
        ];
        assert_eq!(cross_site(&headers(&same_site)), None);
        for bad in [
            &[("host", "board:8420")][..],
            &[("host", "board:8420"), ("x-mistarr", "0")],
            &[
                ("host", "b"),
                ("x-mistarr", "1"),
                ("sec-fetch-site", "cross-site"),
            ],
            &[
                ("host", "b"),
                ("x-mistarr", "1"),
                ("origin", "http://other.example"),
            ],
            &[("host", "b"), ("x-mistarr", "1"), ("origin", "null")],
            &[("x-mistarr", "1"), ("origin", "http://b")],
        ] {
            assert!(cross_site(&headers(bad)).is_some(), "{bad:?}");
        }
    }
}
