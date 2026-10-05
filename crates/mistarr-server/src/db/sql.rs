//! Conversions between Rust values and SQLite columns shared by every table module.

use rusqlite::{Connection, Row};
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::error::{Error, Result};

/// Declares an enum stored as text, with `ALL`, `as_str`, `parse`, `Display`, SQL and
/// serde conversions from one table of `Variant = "text"` pairs. Reading text the table
/// does not list fails with [`UnknownText`].
macro_rules! text_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $( $(#[$vmeta:meta])* $variant:ident = $text:literal, )+
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        $vis enum $name {
            $( $(#[$vmeta])* $variant, )+
        }

        impl $name {
            /// Every value, in declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// The column text.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $text,)+
                }
            }

            /// The value whose column text is `text`.
            #[must_use]
            pub fn parse(text: &str) -> Option<Self> {
                match text {
                    $($text => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl rusqlite::types::ToSql for $name {
            fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
                Ok(rusqlite::types::ToSqlOutput::from(self.as_str()))
            }
        }

        impl rusqlite::types::FromSql for $name {
            fn column_result(
                value: rusqlite::types::ValueRef<'_>,
            ) -> rusqlite::types::FromSqlResult<Self> {
                let text = value.as_str()?;
                Self::parse(text).ok_or_else(|| {
                    rusqlite::types::FromSqlError::Other(Box::new(
                        $crate::db::sql::UnknownText {
                            column: stringify!($name),
                            text: text.to_owned(),
                        },
                    ))
                })
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str())
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(
                d: D,
            ) -> std::result::Result<Self, D::Error> {
                let text = String::deserialize(d)?;
                Self::parse(&text).ok_or_else(|| {
                    serde::de::Error::unknown_variant(&text, &[$($text),+])
                })
            }
        }
    };
}

pub(crate) use text_enum;

/// Column text a [`text_enum!`] type does not list.
#[derive(Debug, thiserror::Error)]
#[error("unknown {column} `{text}`")]
pub struct UnknownText {
    /// The type that read it.
    pub column: &'static str,
    /// The text read.
    pub text: String,
}

/// `values` as an SQL list of quoted text, `('a', 'b')`, for checking a set's fragment.
///
/// ```
/// assert_eq!(mistarr_server::db::sql::text_list(&["a", "b"]), "('a', 'b')");
/// ```
#[must_use]
pub fn text_list(values: &[&str]) -> String {
    let quoted: Vec<String> = values.iter().map(|v| format!("'{v}'")).collect();
    format!("({})", quoted.join(", "))
}

/// `values` as the JSON array a statement reads with `IN (SELECT value FROM json_each(?))`,
/// the one way a list is bound.
///
/// # Errors
///
/// [`Error::Stored`] when a value cannot be written as JSON.
///
/// ```
/// let list = mistarr_server::db::sql::json_list(&["a", "b"]).unwrap();
/// assert_eq!(list, r#"["a","b"]"#);
/// assert_eq!(mistarr_server::db::sql::json_list(&[1, 2]).unwrap(), "[1,2]");
/// ```
pub fn json_list<T: Serialize>(values: &[T]) -> Result<String> {
    to_json("bound list", values)
}

/// `value` as the JSON text stored in column or settings key `key`.
///
/// # Errors
///
/// [`Error::Stored`] naming `key` when `value` cannot be written as JSON.
///
/// ```
/// let text = mistarr_server::db::sql::to_json("jobs.payload", &[1]).unwrap();
/// assert_eq!(text, "[1]");
/// ```
pub fn to_json<T: Serialize + ?Sized>(key: &str, value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|source| Error::Stored {
        key: key.to_owned(),
        source,
    })
}

/// The JSON text stored in column or settings key `key`, read as `T`.
///
/// # Errors
///
/// [`Error::Stored`] naming `key` when `text` is not the JSON `T` expects.
///
/// ```
/// use mistarr_server::db::sql::from_json;
/// assert_eq!(from_json::<Vec<u8>>("jobs.payload", "[1]").unwrap(), [1]);
/// assert!(from_json::<Vec<u8>>("jobs.payload", "{").is_err());
/// ```
pub fn from_json<T: DeserializeOwned>(key: &str, text: &str) -> Result<T> {
    serde_json::from_str(text).map_err(|source| Error::Stored {
        key: key.to_owned(),
        source,
    })
}

/// A JSON column a `FromSql` read refused; [`Error`] turns it into [`Error::Stored`].
#[derive(Debug, thiserror::Error)]
#[error("stored value `{key}` is invalid: {source}")]
pub struct StoredJson {
    /// The column.
    pub key: &'static str,
    /// Why it did not parse.
    pub source: serde_json::Error,
}

/// Reads the JSON text of column `key` as `T`, for a `FromSql` impl; SQL `NULL` reads
/// as JSON `null`, so an `Option<T>` of a nullable column is `None`.
///
/// # Errors
///
/// [`StoredJson`] when the value is not the JSON `T` expects, which a statement
/// returns as [`Error::Stored`].
///
/// ```
/// use rusqlite::types::ValueRef;
/// use mistarr_server::db::sql::json_from_sql;
/// let v: Vec<u8> = json_from_sql("t.c", ValueRef::Text(b"[1]")).unwrap();
/// assert_eq!(v, [1]);
/// assert!(json_from_sql::<Vec<u8>>("t.c", ValueRef::Text(b"{")).is_err());
/// assert_eq!(json_from_sql::<Option<u8>>("t.c", ValueRef::Null).unwrap(), None);
/// ```
pub fn json_from_sql<T: DeserializeOwned>(
    key: &'static str,
    value: rusqlite::types::ValueRef<'_>,
) -> rusqlite::types::FromSqlResult<T> {
    let text = match value {
        rusqlite::types::ValueRef::Null => "null",
        other => other.as_str()?,
    };
    serde_json::from_str(text).map_err(|source| {
        rusqlite::types::FromSqlError::Other(Box::new(StoredJson { key, source }))
    })
}

/// Column `i` of `r` read with [`json_from_sql`] as the JSON of column `key`.
///
/// # Errors
///
/// [`rusqlite::Error`] carrying [`StoredJson`] when the value is not the JSON `T`
/// expects, which [`Error`] turns into [`Error::Stored`].
///
/// ```
/// use mistarr_server::db::sql::get_json;
/// let conn = rusqlite::Connection::open_in_memory().unwrap();
/// let v = conn.query_row("SELECT '[1]', NULL", [], |r| {
///     Ok((get_json::<Vec<u8>>(r, 0, "t.a")?, get_json::<Option<u8>>(r, 1, "t.b")?))
/// });
/// assert_eq!(v.unwrap(), (vec![1], None));
/// ```
pub fn get_json<T: DeserializeOwned>(
    r: &Row<'_>,
    i: usize,
    key: &'static str,
) -> rusqlite::Result<T> {
    let value = r.get_ref(i)?;
    json_from_sql(key, value).map_err(|e| {
        let inner = match e {
            rusqlite::types::FromSqlError::Other(inner) => inner,
            e => Box::new(e),
        };
        rusqlite::Error::FromSqlConversionFailure(i, value.data_type(), inner)
    })
}

/// A count or size SQLite stored as an integer, as `u64`; a negative one reads as 0.
///
/// ```
/// assert_eq!(mistarr_server::db::sql::to_u64(5), 5);
/// assert_eq!(mistarr_server::db::sql::to_u64(-1), 0);
/// ```
#[must_use]
pub fn to_u64(n: i64) -> u64 {
    u64::try_from(n).unwrap_or(0)
}

/// A count or size as SQLite stores it; one beyond `i64::MAX` is stored as that.
///
/// ```
/// assert_eq!(mistarr_server::db::sql::to_i64(5_u64), 5);
/// assert_eq!(mistarr_server::db::sql::to_i64(u64::MAX), i64::MAX);
/// assert_eq!(mistarr_server::db::sql::to_i64(3_usize), 3);
/// ```
#[must_use]
pub fn to_i64(n: impl TryInto<i64>) -> i64 {
    n.try_into().unwrap_or(i64::MAX)
}

/// Column `i` of `r` read with [`to_u64`].
///
/// # Errors
///
/// [`rusqlite::Error`] when the column is missing or not an integer.
///
/// ```
/// let conn = rusqlite::Connection::open_in_memory().unwrap();
/// let n = conn.query_row("SELECT -3, 4", [], |r| {
///     Ok((mistarr_server::db::sql::get_u64(r, 0)?, mistarr_server::db::sql::get_u64(r, 1)?))
/// });
/// assert_eq!(n.unwrap(), (0, 4));
/// ```
pub fn get_u64(r: &Row<'_>, i: usize) -> rusqlite::Result<u64> {
    r.get::<_, i64>(i).map(to_u64)
}

/// `?LIMIT` and `?OFFSET` of a list read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Page {
    /// Rows at most.
    pub limit: u32,
    /// Rows skipped.
    pub offset: u32,
}

impl Page {
    /// The page of `all`, with the length of `all` as its total.
    ///
    /// ```
    /// use mistarr_server::db::sql::Page;
    /// let p = Page { limit: 1, offset: 1 }.slice(vec![1, 2, 3]);
    /// assert_eq!((p.items, p.total), (vec![2], 3));
    /// ```
    #[must_use]
    pub fn slice<T>(self, all: Vec<T>) -> Paged<T> {
        let Paged { items, total } = Paged::all(all);
        let items = items
            .into_iter()
            .skip(usize::try_from(self.offset).unwrap_or(usize::MAX))
            .take(usize::try_from(self.limit).unwrap_or(usize::MAX))
            .collect();
        Paged { items, total }
    }
}

/// One page of a list with the number of rows across every page, `{ items, total }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Paged<T> {
    /// This page.
    pub items: Vec<T>,
    /// Rows across all pages.
    pub total: u64,
}

impl<T> Paged<T> {
    /// Every row of a list on one page, with their number as the total.
    ///
    /// ```
    /// let p = mistarr_server::db::sql::Paged::all(vec![1, 2]);
    /// assert_eq!((p.items, p.total), (vec![1, 2], 2));
    /// ```
    #[must_use]
    pub fn all(items: Vec<T>) -> Self {
        let total = to_u64(to_i64(items.len()));
        Self { items, total }
    }
}

/// Runs `f` inside one read transaction, so every statement it runs sees the same
/// snapshot; inside a transaction already open it runs in that one.
///
/// # Errors
///
/// Whatever `f` returns, or [`crate::Error::Db`] when the transaction cannot begin.
///
/// ```
/// let conn = rusqlite::Connection::open_in_memory().unwrap();
/// let n = mistarr_server::db::sql::snapshot(&conn, |c| {
///     Ok(c.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))?)
/// });
/// assert_eq!(n.unwrap(), 1);
/// ```
pub fn snapshot<T>(conn: &Connection, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    if !conn.is_autocommit() {
        return f(conn);
    }
    // Dropped, it rolls back: the reads need no commit.
    let tx = conn.unchecked_transaction()?;
    f(&tx)
}

#[cfg(test)]
mod tests {
    use super::*;

    text_enum! {
        /// A test enum.
        enum Shade {
            /// Light.
            Light = "light",
            /// Dark.
            Dark = "dark",
        }
    }

    #[test]
    fn text_enums_round_trip_and_refuse_unknown_text() {
        assert_eq!(Shade::ALL, [Shade::Light, Shade::Dark]);
        for s in Shade::ALL {
            assert_eq!(Shade::parse(s.as_str()), Some(*s));
        }
        assert_eq!(Shade::Dark.to_string(), "dark");
        let c = Connection::open_in_memory().expect("open");
        let back: Shade = c
            .query_row("SELECT ?1", [Shade::Dark], |r| r.get(0))
            .expect("round trip");
        assert_eq!(back, Shade::Dark);
        let err = c
            .query_row("SELECT 'dim'", [], |r| r.get::<_, Shade>(0))
            .expect_err("unknown text");
        assert!(err.to_string().contains("unknown Shade `dim`"), "{err}");
        assert_eq!(
            serde_json::to_string(&Shade::Light).expect("json"),
            "\"light\""
        );
        let parsed: Shade = serde_json::from_str("\"dark\"").expect("parse");
        assert_eq!(parsed, Shade::Dark);
        assert!(serde_json::from_str::<Shade>("\"dim\"").is_err());
    }

    #[test]
    fn a_snapshot_runs_in_a_transaction_and_ends_it() {
        let c = Connection::open_in_memory().expect("open");
        let inside = snapshot(&c, |c| Ok(c.is_autocommit())).expect("snapshot");
        assert!(!inside);
        assert!(c.is_autocommit());
        let tx = c.unchecked_transaction().expect("begin");
        assert!(!snapshot(&tx, |c| Ok(c.is_autocommit())).expect("nested"));
    }

    #[test]
    fn lists_and_conversions() {
        assert_eq!(text_list(&[]), "()");
        assert_eq!(json_list::<&str>(&[]).expect("list"), "[]");
        let err = from_json::<u8>("settings.k", "x").expect_err("invalid");
        assert!(matches!(err, Error::Stored { ref key, .. } if key == "settings.k"));
        let c = Connection::open_in_memory().expect("open");
        let err = c
            .query_row("SELECT '{'", [], |r| get_json::<u8>(r, 0, "t.c"))
            .map_err(Error::from)
            .expect_err("invalid");
        assert!(matches!(err, Error::Stored { ref key, .. } if key == "t.c"));
        assert_eq!(to_i64(-2_i32), -2);
        let p = Page {
            limit: 10,
            offset: 5,
        }
        .slice(vec![1, 2]);
        assert!(p.items.is_empty() && p.total == 2);
    }
}
