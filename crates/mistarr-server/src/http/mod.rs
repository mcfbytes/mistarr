//! The axum application: `/api/v1` routes from `docs/API.md` and the embedded SPA.

pub mod catalog;
mod dats;
mod downloads;
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
use axum::http::{HeaderMap, Method, StatusCode};
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
        .layer(middleware::from_fn(refuse_cross_site))
        .layer(middleware::from_fn_with_state(key, require_key));
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

/// Refuses state-changing requests another site could have sent; see `docs/API.md`.
async fn refuse_cross_site(req: Request, next: Next) -> Response {
    if matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS) {
        return next.run(req).await;
    }
    match cross_site(req.headers()) {
        None => next.run(req).await,
        Some(message) => ApiError::new(StatusCode::FORBIDDEN, "forbidden", message).into_response(),
    }
}

/// Why a state-changing request with these headers is refused, or `None` to allow it.
fn cross_site(headers: &HeaderMap) -> Option<&'static str> {
    let get = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
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
