//! Documented routes without an implementation answer 501 with the API error body.
//! A package implementing a route removes it from [`STUBS`] and adds its own router.

use std::sync::Arc;

use axum::http::StatusCode;
use axum::routing::{on, MethodFilter};
use axum::Router;

use super::ApiError;
use crate::app::AppState;

/// `(method, path)` of every documented route not implemented yet.
pub(super) const STUBS: &[(&str, &str)] = &[("GET", "/imports")];

pub(super) fn routes() -> Router<Arc<AppState>> {
    let mut router = Router::new();
    for &(method, path) in STUBS {
        let Some(filter) = filter(method) else {
            continue;
        };
        let message = format!("{method} /api/v1{path} is not implemented in this build");
        router = router.route(
            path,
            on(filter, move || {
                let message = message.clone();
                async move { ApiError::new(StatusCode::NOT_IMPLEMENTED, "not_implemented", message) }
            }),
        );
    }
    router
}

/// The filter for a method name the stub table uses.
fn filter(method: &str) -> Option<MethodFilter> {
    match method {
        "GET" => Some(MethodFilter::GET),
        "PUT" => Some(MethodFilter::PUT),
        "POST" => Some(MethodFilter::POST),
        "DELETE" => Some(MethodFilter::DELETE),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_table_uses_known_methods_and_unique_routes() {
        let mut seen = std::collections::HashSet::new();
        for &(m, p) in STUBS {
            assert!(filter(m).is_some(), "{m}");
            assert!(seen.insert((m, p)), "{m} {p} twice");
        }
        let _ = routes();
    }
}
