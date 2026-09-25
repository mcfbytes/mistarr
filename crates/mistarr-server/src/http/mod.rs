//! The axum application: `/api/v1` routes from `docs/API.md` and the embedded SPA.

pub mod catalog;
mod dats;
pub(crate) mod downloads;
mod events;
mod imports;
mod launch;
mod platforms;
mod sources;
mod spa;
mod stubs;
mod system;

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::app::AppState;

/// Header carrying the API key.
pub const API_KEY_HEADER: &str = "x-api-key";

/// Header every state-changing request must carry as `1`; a cross-site form cannot set it.
pub const GUARD_HEADER: &str = "x-mistarr";

/// Default and maximum page size of list endpoints.
const DEFAULT_LIMIT: u32 = 100;
const MAX_LIMIT: u32 = 1000;

/// Builds the whole application.
pub fn router(app: Arc<AppState>) -> Router {
    let key = Some(app.config().server.api_key).filter(|k| !k.is_empty());
    let api = Router::new()
        .merge(system::routes())
        .merge(events::routes())
        .merge(sources::routes())
        .merge(platforms::routes())
        .merge(catalog::routes())
        .merge(dats::routes())
        .merge(imports::routes())
        .merge(downloads::routes())
        .merge(launch::routes())
        .merge(stubs::routes())
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

/// The error body of `docs/API.md`: `{ error: { code, message } }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    /// HTTP status.
    pub status: StatusCode,
    /// Machine-readable code.
    pub code: &'static str,
    /// Human-readable message.
    pub message: String,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: ErrorInner<'a>,
}

#[derive(Serialize)]
struct ErrorInner<'a> {
    code: &'a str,
    message: &'a str,
}

impl ApiError {
    /// An error with any status.
    #[must_use]
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    /// 400 `bad_request`.
    #[must_use]
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", message)
    }

    /// 404 `not_found`.
    #[must_use]
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", message)
    }

    /// 409 `conflict`.
    #[must_use]
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "conflict", message)
    }

    /// 409 `busy`: the same action ran moments ago.
    #[must_use]
    pub fn busy(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "busy", message)
    }

    /// 503 `unavailable`: something outside the server cannot take the request.
    #[must_use]
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, "unavailable", message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ErrorBody {
            error: ErrorInner {
                code: self.code,
                message: &self.message,
            },
        };
        (self.status, Json(body)).into_response()
    }
}

impl From<crate::Error> for ApiError {
    fn from(e: crate::Error) -> Self {
        tracing::error!(error = %e, "request failed");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string())
    }
}

/// `?limit=&offset=` of list endpoints.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct Paging {
    /// Page size; defaults to 100, capped at 1000.
    pub limit: Option<u32>,
    /// Rows to skip.
    pub offset: Option<u32>,
}

impl Paging {
    /// The effective `(limit, offset)`.
    #[must_use]
    pub fn resolve(self) -> (u32, u32) {
        (
            self.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT),
            self.offset.unwrap_or(0),
        )
    }
}

/// `{ items, total }` of list endpoints.
#[derive(Debug, Clone, Serialize)]
pub struct Page<T> {
    /// This page.
    pub items: Vec<T>,
    /// Rows across all pages.
    pub total: u64,
}

impl<T> Page<T> {
    /// The page of `all` that `paging` asks for.
    ///
    /// ```
    /// use mistarr_server::http::{Page, Paging};
    /// let p = Page::slice(vec![1, 2, 3], &Paging { limit: Some(1), offset: Some(1) });
    /// assert_eq!((p.items, p.total), (vec![2], 3));
    /// ```
    #[must_use]
    pub fn slice(all: Vec<T>, paging: &Paging) -> Self {
        let (limit, offset) = paging.resolve();
        let total = u64::try_from(all.len()).unwrap_or(u64::MAX);
        let items = all
            .into_iter()
            .skip(usize::try_from(offset).unwrap_or(usize::MAX))
            .take(usize::try_from(limit).unwrap_or(usize::MAX))
            .collect();
        Self { items, total }
    }
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
        ApiError::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "missing or wrong API key",
        )
        .into_response()
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
        Some(message) => ApiError::new(StatusCode::FORBIDDEN, "forbidden", message).into_response(),
    }
}

/// Why a state-changing request with these headers is refused, or `None` to allow it.
fn cross_site(headers: &HeaderMap, allow: &HostAllowlist) -> Option<&'static str> {
    let get = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    if !get("host").is_some_and(|h| allow.allows(h)) {
        return Some("the request's Host is not a name this server answers to");
    }
    if get("sec-fetch-site").is_some_and(|v| v.eq_ignore_ascii_case("cross-site")) {
        return Some("requests from another site are refused");
    }
    if let Some(origin) = get("origin") {
        let origin_host = origin.split_once("://").map(|(_, host)| host);
        let same = origin_host
            .zip(get("host"))
            .is_some_and(|(o, h)| o.eq_ignore_ascii_case(h));
        if !same {
            return Some("the request's Origin does not match its Host");
        }
    }
    if get(GUARD_HEADER) != Some("1") {
        return Some("state-changing requests need the X-Mistarr: 1 header");
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
    ApiError::not_found("no such API route")
}

async fn method_not_allowed() -> ApiError {
    ApiError::new(
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
        "method not allowed on this route",
    )
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

    #[test]
    fn paging_defaults_and_caps() {
        assert_eq!(Paging::default().resolve(), (100, 0));
        let p = Paging {
            limit: Some(5000),
            offset: Some(3),
        };
        assert_eq!(p.resolve(), (1000, 3));
    }

    #[test]
    fn error_shape() {
        let r = ApiError::bad_request("nope").into_response();
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        let e = ApiError::from(crate::Error::Poisoned);
        assert_eq!(e.code, "internal");
        assert_eq!(ApiError::not_found("x").status, StatusCode::NOT_FOUND);
        assert_eq!(ApiError::conflict("x").code, "conflict");
        let e = ApiError::unavailable("x");
        assert_eq!(
            (e.status, e.code),
            (StatusCode::SERVICE_UNAVAILABLE, "unavailable")
        );
    }
}
