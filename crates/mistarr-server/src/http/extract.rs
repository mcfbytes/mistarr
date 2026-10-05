//! Request extractors that refuse with [`ApiError`], and the paging every list takes.

use axum::body::Bytes;
use axum::extract::{FromRequest, FromRequestParts, Path, Query, Request};
use axum::http::request::Parts;
use serde::de::{self, DeserializeOwned, Deserializer, Visitor};
use serde::Deserialize;

use super::ApiError;
use crate::db::sql::Page;

/// Default and maximum page size of list endpoints.
const DEFAULT_LIMIT: u32 = 100;
const MAX_LIMIT: u32 = 1000;

/// Path parameters, a 400 when they do not parse; ids arrive as their newtypes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiPath<T>(pub T);

/// The query string, a 400 when it does not parse; unknown parameters are ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiQuery<T>(pub T);

/// A JSON body, a 400 when it does not parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiJson<T>(pub T);

/// A JSON body that may be left out: an empty or blank body is `T::default()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionalJson<T>(pub T);

impl<T: DeserializeOwned + Send, S: Send + Sync> FromRequestParts<S> for ApiPath<T> {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, ApiError> {
        match Path::<T>::from_request_parts(parts, state).await {
            Ok(Path(value)) => Ok(Self(value)),
            Err(e) => Err(ApiError::bad_request(e.body_text())),
        }
    }
}

impl<T: DeserializeOwned, S: Send + Sync> FromRequestParts<S> for ApiQuery<T> {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, ApiError> {
        match Query::<T>::from_request_parts(parts, state).await {
            Ok(Query(value)) => Ok(Self(value)),
            Err(e) => Err(ApiError::bad_request(e.body_text())),
        }
    }
}

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for ApiJson<T> {
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, ApiError> {
        body(req, state).await.and_then(|b| parse(&b)).map(Self)
    }
}

impl<T: DeserializeOwned + Default, S: Send + Sync> FromRequest<S> for OptionalJson<T> {
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, ApiError> {
        let bytes = body(req, state).await?;
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(Self(T::default()));
        }
        parse(&bytes).map(Self)
    }
}

async fn body<S: Send + Sync>(req: Request, state: &S) -> Result<Bytes, ApiError> {
    Bytes::from_request(req, state)
        .await
        .map_err(|e| ApiError::bad_request(e.body_text()))
}

/// `bytes` as JSON `T`, the parser's complaint in a 400 when it is not.
fn parse<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, ApiError> {
    serde_json::from_slice(bytes).map_err(|e| invalid(&e))
}

/// A body read as [`serde_json::Value`] as `T`, refused as [`ApiJson`] refuses one.
pub(crate) fn from_value<T: DeserializeOwned>(value: serde_json::Value) -> Result<T, ApiError> {
    serde_json::from_value(value).map_err(|e| invalid(&e))
}

fn invalid(e: &serde_json::Error) -> ApiError {
    ApiError::bad_request(format!("The body is not valid: {e}"))
}

/// `?limit=&offset=` of list endpoints; a query struct embeds it with `#[serde(flatten)]`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub struct Paging {
    /// Page size; defaults to 100, capped at 1000.
    #[serde(default, deserialize_with = "number")]
    pub limit: Option<u32>,
    /// Rows to skip.
    #[serde(default, deserialize_with = "number")]
    pub offset: Option<u32>,
}

impl Paging {
    /// The effective page: a limit of 100 by default, capped at 1000.
    ///
    /// ```
    /// use mistarr_server::http::Paging;
    /// let p = Paging { limit: Some(1), offset: Some(1) }.resolve().slice(vec![1, 2, 3]);
    /// assert_eq!((p.items, p.total), (vec![2], 3));
    /// ```
    #[must_use]
    pub fn resolve(self) -> Page {
        Page {
            limit: self.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT),
            offset: self.offset.unwrap_or(0),
        }
    }
}

/// A count given as a number or, as a flattened query field arrives, as a string.
fn number<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
    struct Count;

    impl Visitor<'_> for Count {
        type Value = u32;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a whole number")
        }

        fn visit_u64<E: de::Error>(self, v: u64) -> Result<u32, E> {
            u32::try_from(v).map_err(|_| E::invalid_value(de::Unexpected::Unsigned(v), &self))
        }

        fn visit_i64<E: de::Error>(self, v: i64) -> Result<u32, E> {
            u32::try_from(v).map_err(|_| E::invalid_value(de::Unexpected::Signed(v), &self))
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<u32, E> {
            v.parse()
                .map_err(|_| E::invalid_value(de::Unexpected::Str(v), &self))
        }
    }

    d.deserialize_any(Count).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::StatusCode;

    fn parts(uri: &str) -> Parts {
        Request::builder()
            .uri(uri)
            .body(Body::empty())
            .expect("request")
            .into_parts()
            .0
    }

    #[derive(Debug, Default, Deserialize, PartialEq)]
    #[serde(deny_unknown_fields)]
    struct Body1 {
        a: Option<u8>,
    }

    #[derive(Debug, Deserialize)]
    struct ListQuery {
        q: Option<String>,
        #[serde(flatten)]
        paging: Paging,
    }

    fn post(body: &'static str) -> Request {
        Request::builder()
            .method("POST")
            .body(Body::from(body))
            .expect("request")
    }

    #[tokio::test]
    async fn bodies_parse_or_refuse_with_a_400() {
        let ApiJson(b) = ApiJson::<Body1>::from_request(post(r#"{"a":1}"#), &())
            .await
            .expect("json");
        assert_eq!(b.a, Some(1));
        for bad in ["", " ", r#"{"b":1}"#, "{"] {
            let e = ApiJson::<Body1>::from_request(post(bad), &())
                .await
                .expect_err(bad);
            assert_eq!(e.status(), StatusCode::BAD_REQUEST, "{bad}");
        }
        for blank in ["", " \n"] {
            let OptionalJson(b) = OptionalJson::<Body1>::from_request(post(blank), &())
                .await
                .expect("blank");
            assert_eq!(b, Body1::default());
        }
        let refused = OptionalJson::<Body1>::from_request(post(r#"{"b":1}"#), &()).await;
        assert!(refused.is_err(), "bodies deny unknown fields");
        let b: Body1 = from_value(serde_json::json!({ "a": 2 })).expect("value");
        assert_eq!(b.a, Some(2));
        let e = from_value::<Body1>(serde_json::json!({ "b": 1 })).expect_err("unknown");
        assert_eq!(e.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn queries_page_ignore_unknown_keys_and_refuse_bad_numbers() {
        let mut p = parts("/x?q=a&limit=5&offset=2&apikey=k");
        let ApiQuery(q) = ApiQuery::<ListQuery>::from_request_parts(&mut p, &())
            .await
            .expect("query");
        assert_eq!(q.q.as_deref(), Some("a"));
        assert_eq!(
            q.paging.resolve(),
            Page {
                limit: 5,
                offset: 2
            }
        );
        let mut p = parts("/x?limit=many");
        let e = ApiQuery::<ListQuery>::from_request_parts(&mut p, &())
            .await
            .expect_err("not a number");
        assert_eq!(e.status(), StatusCode::BAD_REQUEST);
        let mut p = parts("/x?offset=9");
        let ApiQuery(paging) = ApiQuery::<Paging>::from_request_parts(&mut p, &())
            .await
            .expect("plain paging");
        assert_eq!(
            paging.resolve(),
            Page {
                limit: 100,
                offset: 9
            }
        );
    }

    #[tokio::test]
    async fn path_parameters_outside_a_route_are_a_400() {
        let mut p = parts("/x/1");
        let e = ApiPath::<i64>::from_request_parts(&mut p, &())
            .await
            .expect_err("no route");
        assert_eq!(e.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn paging_defaults_and_caps() {
        assert_eq!(
            Paging::default().resolve(),
            Page {
                limit: 100,
                offset: 0
            }
        );
        let p = Paging {
            limit: Some(5000),
            offset: Some(3),
        };
        assert_eq!(
            p.resolve(),
            Page {
                limit: 1000,
                offset: 3
            }
        );
        let json: Paging = serde_json::from_str(r#"{"limit":2}"#).expect("json");
        assert_eq!(json.limit, Some(2));
        assert!(serde_json::from_str::<Paging>(r#"{"limit":-1}"#).is_err());
        assert!(parse::<Paging>(br#"{"limit":"x"}"#).is_err());
    }
}
