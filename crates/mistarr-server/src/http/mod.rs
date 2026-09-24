//! The axum application: `/api/v1` routes from `docs/API.md` and the embedded SPA.

pub mod catalog;
mod dats;
mod events;
mod platforms;
mod sources;
mod spa;
mod stubs;
mod system;

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::app::AppState;

/// Header carrying the API key.
pub const API_KEY_HEADER: &str = "x-api-key";

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
        .merge(stubs::routes())
        .fallback(api_not_found)
        .method_not_allowed_fallback(method_not_allowed)
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
    }
}
