//! The error body of `docs/API.md`: `{ error: { code, message } }`.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

use crate::incoming::place::PlaceError;
use crate::Error;

/// The machine-readable `code` of an API error; each code has one HTTP status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Code {
    /// 400: the request itself is wrong.
    BadRequest,
    /// 401: the API key is missing or wrong.
    Unauthorized,
    /// 403: a state-changing request another site could have sent.
    Forbidden,
    /// 404: no such route or item.
    NotFound,
    /// 405: the route takes another method.
    MethodNotAllowed,
    /// 409: the item's state refuses the request.
    Conflict,
    /// 409: the same action ran moments ago or is still running.
    Busy,
    /// 503: something outside the server cannot take the request.
    Unavailable,
    /// 500: the server failed.
    Internal,
}

impl Code {
    /// The HTTP status answered with this code.
    ///
    /// ```
    /// use mistarr_server::http::Code;
    /// assert_eq!(Code::Busy.status(), axum::http::StatusCode::CONFLICT);
    /// ```
    #[must_use]
    pub fn status(self) -> StatusCode {
        match self {
            Self::BadRequest => StatusCode::BAD_REQUEST,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
            Self::Conflict | Self::Busy => StatusCode::CONFLICT,
            Self::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// The code as the body names it.
    ///
    /// ```
    /// assert_eq!(mistarr_server::http::Code::NotFound.as_str(), "not_found");
    /// ```
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BadRequest => "bad_request",
            Self::Unauthorized => "unauthorized",
            Self::Forbidden => "forbidden",
            Self::NotFound => "not_found",
            Self::MethodNotAllowed => "method_not_allowed",
            Self::Conflict => "conflict",
            Self::Busy => "busy",
            Self::Unavailable => "unavailable",
            Self::Internal => "internal",
        }
    }
}

/// An API error: a [`Code`] and a message written as a sentence, per
/// `docs/PRINCIPLES.md` section 5.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    code: Code,
    message: String,
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
    fn new(code: Code, message: impl Into<String>) -> Self {
        Self {
            code,
            message: sentence(&message.into()),
        }
    }

    /// The error's code.
    ///
    /// ```
    /// use mistarr_server::http::{ApiError, Code};
    /// assert_eq!(ApiError::busy("Wait.").code(), Code::Busy);
    /// ```
    #[must_use]
    #[cfg(any(test, feature = "test-support"))]
    pub fn code(&self) -> Code {
        self.code
    }

    /// The HTTP status of its code.
    ///
    /// ```
    /// let e = mistarr_server::http::ApiError::conflict("Not now.");
    /// assert_eq!(e.status(), axum::http::StatusCode::CONFLICT);
    /// ```
    #[must_use]
    pub fn status(&self) -> StatusCode {
        self.code.status()
    }

    /// The message, a sentence ending in a full stop.
    ///
    /// ```
    /// let e = mistarr_server::http::ApiError::bad_request("the body is empty");
    /// assert_eq!(e.message(), "The body is empty.");
    /// ```
    #[must_use]
    #[cfg(any(test, feature = "test-support"))]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// 400 `bad_request`.
    #[must_use]
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(Code::BadRequest, message)
    }

    /// 401 `unauthorized`.
    #[must_use]
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(Code::Unauthorized, message)
    }

    /// 403 `forbidden`.
    #[must_use]
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(Code::Forbidden, message)
    }

    /// 404 `not_found`, "No such `what`."
    ///
    /// ```
    /// let e = mistarr_server::http::ApiError::no_such("platform");
    /// assert_eq!((e.status().as_u16(), e.message()), (404, "No such platform."));
    /// ```
    #[must_use]
    pub fn no_such(what: &str) -> Self {
        Self::new(Code::NotFound, format!("No such {what}."))
    }

    /// 405 `method_not_allowed`.
    #[must_use]
    pub fn method_not_allowed() -> Self {
        Self::new(
            Code::MethodNotAllowed,
            "This route does not take that method.",
        )
    }

    /// 409 `conflict`.
    #[must_use]
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(Code::Conflict, message)
    }

    /// 409 `busy`: the same action ran moments ago.
    #[must_use]
    pub fn busy(message: impl Into<String>) -> Self {
        Self::new(Code::Busy, message)
    }

    /// 503 `unavailable`: something outside the server cannot take the request.
    #[must_use]
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(Code::Unavailable, message)
    }

    /// 500 `internal`.
    #[must_use]
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(Code::Internal, message)
    }
}

/// `message` trimmed, with a closing full stop and a capital first letter when it opens
/// with a plain word, so an interpolated path or value keeps its case.
fn sentence(message: &str) -> String {
    let trimmed = message.trim();
    let first_word = trimmed
        .split(char::is_whitespace)
        .next()
        .unwrap_or("")
        .trim_end_matches([',', ':', ';', '.', '!', '?']);
    let mut out = String::with_capacity(trimmed.len() + 1);
    let mut chars = trimmed.chars();
    match chars.next() {
        Some(first) if first_word.chars().all(char::is_alphabetic) => {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
        _ => out.push_str(trimmed),
    }
    if !out.ends_with(['.', '!', '?']) {
        out.push('.');
    }
    out
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ErrorBody {
            error: ErrorInner {
                code: self.code.as_str(),
                message: &self.message,
            },
        };
        (self.status(), Json(body)).into_response()
    }
}

impl From<Error> for ApiError {
    /// The code a caller can act on for each failure; any other is `internal` and logged.
    fn from(e: Error) -> Self {
        use mistarr_mister::Error as Main;
        let code = match &e {
            Error::Place(PlaceError::Duplicate) | Error::Settings(_) => Code::BadRequest,
            Error::Place(PlaceError::NoFreeName) | Error::Mister(Main::UnsafePath(_)) => {
                Code::Conflict
            }
            Error::NoRoom(_)
            | Error::Cancelled
            | Error::Reopen(_)
            | Error::Client(_)
            | Error::Mister(Main::CommandAbsent | Main::NotListening | Main::CommandBusy) => {
                Code::Unavailable
            }
            _ => Code::Internal,
        };
        if code == Code::Internal {
            tracing::error!(error = %e, "request failed");
        }
        Self::new(code, e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mistarr_mister::Error as Main;

    #[test]
    fn codes_match_their_statuses() {
        for (code, status, name) in [
            (Code::BadRequest, 400, "bad_request"),
            (Code::Unauthorized, 401, "unauthorized"),
            (Code::Forbidden, 403, "forbidden"),
            (Code::NotFound, 404, "not_found"),
            (Code::MethodNotAllowed, 405, "method_not_allowed"),
            (Code::Conflict, 409, "conflict"),
            (Code::Busy, 409, "busy"),
            (Code::Unavailable, 503, "unavailable"),
            (Code::Internal, 500, "internal"),
        ] {
            assert_eq!((code.status().as_u16(), code.as_str()), (status, name));
        }
    }

    #[test]
    fn constructors_pick_their_code_and_word_a_sentence() {
        let cases = [
            (ApiError::bad_request("x"), Code::BadRequest),
            (ApiError::unauthorized("x"), Code::Unauthorized),
            (ApiError::forbidden("x"), Code::Forbidden),
            (ApiError::no_such("x"), Code::NotFound),
            (ApiError::method_not_allowed(), Code::MethodNotAllowed),
            (ApiError::conflict("x"), Code::Conflict),
            (ApiError::busy("x"), Code::Busy),
            (ApiError::unavailable("x"), Code::Unavailable),
            (ApiError::internal("x"), Code::Internal),
        ];
        for (e, code) in cases {
            assert_eq!(e.code(), code);
            assert!(e.message().ends_with('.'), "{}", e.message());
            assert!(
                e.message().starts_with(char::is_uppercase),
                "{}",
                e.message()
            );
        }
        assert_eq!(sentence(" done! "), "Done!");
        assert_eq!(sentence(""), ".");
        assert_eq!(sentence("not a torrent: x"), "Not a torrent: x.");
        assert_eq!(sentence("nes/x.nes exists"), "nes/x.nes exists.");
        let r = ApiError::bad_request("nope").into_response();
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn server_errors_map_to_what_the_caller_can_do() {
        let code = |e: Error| ApiError::from(e).code();
        assert_eq!(code(Error::Poisoned), Code::Internal);
        assert_eq!(code(Error::NoRoom("full".into())), Code::Unavailable);
        assert_eq!(code(Error::Cancelled), Code::Unavailable);
        assert_eq!(code(PlaceError::Duplicate.into()), Code::BadRequest);
        assert_eq!(code(PlaceError::NoFreeName.into()), Code::Conflict);
        let io = PlaceError::Io(std::io::Error::other("x"));
        assert_eq!(code(io.into()), Code::Internal);
        let refused = mistarr_clients::Error::NotFound;
        assert_eq!(code(refused.into()), Code::Unavailable);
        assert_eq!(code(Main::CommandBusy.into()), Code::Unavailable);
        assert_eq!(code(Main::UnsafePath("x".into()).into()), Code::Conflict);
        let settings = Error::Settings(crate::config::ConfigProblem::PathMap);
        assert_eq!(code(settings), Code::BadRequest);
        let e = ApiError::from(Error::NoRoom("the card is full".into()));
        assert_eq!(e.message(), "The card is full.");
    }
}
