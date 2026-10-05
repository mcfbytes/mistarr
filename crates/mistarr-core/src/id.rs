//! Id newtypes and the macros every crate declares its ids with; see `docs/ARCHITECTURE.md` "Ids".

use std::borrow::Cow;
use std::fmt;

/// Declares a database row id: a newtype over `i64` with a private field, `new` and `get`,
/// `Display`, ordering and transparent serde. `row_id_sql!`, under the `rusqlite` feature,
/// adds the SQLite conversions.
/// The serde derives expand in the caller, which must depend on `serde` with `derive`.
///
/// ```
/// mistarr_core::row_id! {
///     /// A `things.id`.
///     ThingId
/// }
/// assert_eq!(ThingId::new(7).get(), 7);
/// assert_eq!(ThingId::new(7).to_string(), "7");
/// assert_eq!(serde_json::to_string(&ThingId::new(7)).unwrap(), "7");
/// ```
#[macro_export]
macro_rules! row_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord,
            serde::Serialize, serde::Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(i64);

        impl $name {
            /// The id with this row number.
            #[must_use]
            pub const fn new(id: i64) -> Self {
                Self(id)
            }

            /// The row number.
            #[must_use]
            pub const fn get(self) -> i64 {
                self.0
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                ::std::fmt::Display::fmt(&self.0, f)
            }
        }
    };
}

/// Implements `ToSql` and `FromSql` for a [`row_id!`] id, stored as its integer.
#[cfg(feature = "rusqlite")]
#[macro_export]
macro_rules! row_id_sql {
    ($name:ident) => {
        impl $crate::__rusqlite::types::ToSql for $name {
            fn to_sql(
                &self,
            ) -> $crate::__rusqlite::Result<$crate::__rusqlite::types::ToSqlOutput<'_>> {
                Ok(self.get().into())
            }
        }

        impl $crate::__rusqlite::types::FromSql for $name {
            fn column_result(
                value: $crate::__rusqlite::types::ValueRef<'_>,
            ) -> $crate::__rusqlite::types::FromSqlResult<Self> {
                i64::column_result(value).map(Self::new)
            }
        }
    };
}

row_id! {
    /// A `roms.id`: a DAT entry's rom, as the catalogue and source binding name it.
    RomId
}

#[cfg(feature = "rusqlite")]
row_id_sql!(RomId);

row_id! {
    /// A title's clone group: the `titles.id` of the group's root, as fuzzy matching counts it.
    GroupId
}

#[cfg(feature = "rusqlite")]
row_id_sql!(GroupId);

/// Stable platform identifier, e.g. `nes`, `megadrive`, `psx`. The full table lives in
/// `docs/PLATFORMS.md` and in `mistarr-mister`; an id from that table borrows its text.
///
/// ```
/// use mistarr_core::PlatformId;
/// let nes = PlatformId::new("nes");
/// assert_eq!(nes, PlatformId::new(String::from("nes")));
/// assert_eq!((nes.as_str(), nes.to_string()), ("nes", "nes".to_owned()));
/// ```
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct PlatformId(Cow<'static, str>);

impl PlatformId {
    /// The id spelled `id`; a `&'static str` is borrowed, not copied.
    #[must_use]
    pub fn new(id: impl Into<Cow<'static, str>>) -> Self {
        Self(id.into())
    }

    /// The id as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PlatformId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Stored as its text.
#[cfg(feature = "rusqlite")]
impl rusqlite::types::ToSql for PlatformId {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(rusqlite::types::ToSqlOutput::Borrowed(
            rusqlite::types::ValueRef::Text(self.0.as_bytes()),
        ))
    }
}

#[cfg(feature = "rusqlite")]
impl rusqlite::types::FromSql for PlatformId {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        value.as_str().map(|s| Self::new(s.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_static_platform_id_borrows_and_equals_an_owned_one() {
        let borrowed = PlatformId::new("snes");
        assert!(matches!(borrowed.0, Cow::Borrowed(_)));
        let owned: PlatformId = serde_json::from_str("\"snes\"").expect("json");
        assert_eq!(owned, borrowed);
        assert_eq!(serde_json::to_string(&borrowed).expect("json"), "\"snes\"");
        assert!(PlatformId::new("a") < PlatformId::new("b"));
    }

    #[test]
    fn row_ids_order_display_and_serialize_as_their_number() {
        assert!(RomId::new(1) < RomId::new(2));
        assert_eq!(RomId::new(5).to_string(), "5");
        let id: RomId = serde_json::from_str("9").expect("json");
        assert_eq!(id.get(), 9);
    }

    #[cfg(feature = "rusqlite")]
    #[test]
    fn ids_round_trip_through_sql() {
        let c = rusqlite::Connection::open_in_memory().expect("open");
        let back: RomId = c
            .query_row("SELECT ?1", [RomId::new(7)], |r| r.get(0))
            .expect("rom");
        assert_eq!(back, RomId::new(7));
        assert!(c
            .query_row("SELECT 'x'", [], |r| r.get::<_, RomId>(0))
            .is_err());
        let nes: PlatformId = c
            .query_row("SELECT ?1", [PlatformId::new("nes")], |r| r.get(0))
            .expect("platform");
        assert_eq!(nes, PlatformId::new("nes"));
        assert!(c
            .query_row("SELECT 1", [], |r| r.get::<_, PlatformId>(0))
            .is_err());
    }
}
