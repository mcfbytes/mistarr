//! Row id newtypes: one per table whose id crosses a function or a row. `roms.id` is
//! `mistarr_core::RomId`, since source binding names roms too.

/// Declares an `INTEGER PRIMARY KEY` newtype with core's [`mistarr_core::row_id!`] and its
/// SQL conversions.
macro_rules! id {
    ($(#[$meta:meta])* $name:ident) => {
        mistarr_core::row_id! { $(#[$meta])* $name }
        mistarr_core::row_id_sql!($name);
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
        let back: FileId = c
            .query_row("SELECT ?1", [FileId::new(7)], |r| r.get(0))
            .expect("round trip");
        assert_eq!(back, FileId::new(7));
        let none: Option<TitleId> = c.query_row("SELECT NULL", [], |r| r.get(0)).expect("null");
        assert_eq!(none, None);
        assert!(c
            .query_row("SELECT 'x'", [], |r| r.get::<_, FileId>(0))
            .is_err());
        assert_eq!(serde_json::to_string(&JobId::new(3)).expect("json"), "3");
        let id: DownloadId = serde_json::from_str("4").expect("parse");
        assert_eq!(id, DownloadId::new(4));
        assert_eq!(SourceId::new(9).to_string(), "9");
        assert!(DatVersionId::new(1) < DatVersionId::new(2));
        assert_eq!(ImportId::new(5).get(), 5);
    }
}
