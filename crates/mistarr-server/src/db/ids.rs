//! Row id newtypes: one per table whose id crosses a function or a row.

/// Declares an `INTEGER PRIMARY KEY` newtype with the derives, `Display`, SQL and
/// transparent serde conversions every row id shares.
macro_rules! id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord,
            serde::Serialize, serde::Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub i64);

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }

        impl rusqlite::types::ToSql for $name {
            fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
                self.0.to_sql()
            }
        }

        impl rusqlite::types::FromSql for $name {
            fn column_result(
                value: rusqlite::types::ValueRef<'_>,
            ) -> rusqlite::types::FromSqlResult<Self> {
                i64::column_result(value).map(Self)
            }
        }
    };
}

id! {
    /// A `files.id`.
    FileId
}

id! {
    /// A `titles.id`.
    TitleId
}

id! {
    /// A `roms.id`.
    RomId
}

id! {
    /// A `sources.id`.
    SourceId
}

id! {
    /// A `jobs.id`.
    JobId
}

id! {
    /// A `downloads.id`.
    DownloadId
}

id! {
    /// A `dat_versions.id`.
    DatVersionId
}

id! {
    /// An `import_log.id`.
    ImportId
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn ids_round_trip_through_sql_serde_and_display() {
        let c = Connection::open_in_memory().expect("open");
        let back: RomId = c
            .query_row("SELECT ?1", [RomId(7)], |r| r.get(0))
            .expect("round trip");
        assert_eq!(back, RomId(7));
        let none: Option<TitleId> = c.query_row("SELECT NULL", [], |r| r.get(0)).expect("null");
        assert_eq!(none, None);
        assert!(c
            .query_row("SELECT 'x'", [], |r| r.get::<_, FileId>(0))
            .is_err());
        assert_eq!(serde_json::to_string(&JobId(3)).expect("json"), "3");
        let id: DownloadId = serde_json::from_str("4").expect("parse");
        assert_eq!(id, DownloadId(4));
        assert_eq!(SourceId(9).to_string(), "9");
        assert!(DatVersionId(1) < DatVersionId(2));
        assert_eq!(ImportId(5).to_string(), "5");
    }
}
