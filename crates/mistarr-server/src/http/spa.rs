//! Serves `web/dist` embedded at build time, falling back to `index.html`.

use axum::http::{header, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

use super::ApiError;

/// The built SPA. Absent at build time, every lookup misses and the placeholder is served.
#[derive(RustEmbed)]
#[folder = "../../web/dist"]
#[allow_missing = true]
struct Assets;

/// Served when the binary was built without `web/dist`.
const PLACEHOLDER: &str = "<!doctype html><meta charset=utf-8><title>mistarr</title><p>mistarr is running. This build does not include the web UI; the API is at /api/v1.</p>\n";

/// Fallback for every path outside `/api/v1`.
pub(super) async fn serve(method: Method, uri: Uri) -> Response {
    let path = uri.path();
    if path == "/api" || path.starts_with("/api/") {
        return ApiError::not_found("no such API route").into_response();
    }
    if method != Method::GET && method != Method::HEAD {
        return ApiError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "only GET and HEAD are served outside the API",
        )
        .into_response();
    }
    let name = path.trim_start_matches('/');
    if !name.is_empty() {
        if let Some(file) = Assets::get(name) {
            let cache = if name.starts_with("assets/") {
                "public, max-age=31536000, immutable"
            } else {
                "no-cache"
            };
            return file_response(file.data.into_owned(), content_type(name), cache);
        }
    }
    match Assets::get("index.html") {
        Some(index) => file_response(
            index.data.into_owned(),
            "text/html; charset=utf-8",
            "no-cache",
        ),
        None => file_response(
            PLACEHOLDER.as_bytes().to_vec(),
            "text/html; charset=utf-8",
            "no-cache",
        ),
    }
}

fn file_response(body: Vec<u8>, content_type: &'static str, cache: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
            (header::CACHE_CONTROL, HeaderValue::from_static(cache)),
        ],
        body,
    )
        .into_response()
}

/// Content type by extension for the files Vite emits.
fn content_type(name: &str) -> &'static str {
    let ext = name.rsplit_once('.').map_or("", |(_, e)| e);
    match ext.to_ascii_lowercase().as_str() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "webmanifest" => "application/manifest+json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_types_by_extension() {
        assert_eq!(
            content_type("assets/index-abc.js"),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(content_type("a.CSS"), "text/css; charset=utf-8");
        assert_eq!(content_type("noext"), "application/octet-stream");
        assert_eq!(content_type("favicon.svg"), "image/svg+xml");
    }

    #[tokio::test]
    async fn api_paths_never_fall_back_to_the_page() {
        let r = serve(Method::GET, Uri::from_static("/api/v2/x")).await;
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
        let r = serve(Method::GET, Uri::from_static("/p/nes")).await;
        assert_eq!(r.status(), StatusCode::OK);
        let r = serve(Method::POST, Uri::from_static("/p/nes")).await;
        assert_eq!(r.status(), StatusCode::METHOD_NOT_ALLOWED);
    }
}
