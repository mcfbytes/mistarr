//! The `settings` key-value table.

use rusqlite::{params, Connection, OptionalExtension};
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::error::{Error, Result};

/// Keys the server stores.
pub mod keys {
    /// JSON [`crate::jobs::detect_client::ClientStatus`] from the last detection.
    pub const CLIENT_DETECTED: &str = "client.detected";
    /// JSON [`crate::config::RuntimeSettings`] saved through the API.
    pub const RUNTIME: &str = "config.runtime";
    /// JSON `bool`: whether the wizard-completion scan already fired.
    pub const WIZARD_SCAN_DONE: &str = "wizard.scan_done";
    /// JSON `bool`: whether the user finished or dismissed the wizard once.
    pub const WIZARD_DISMISSED: &str = "wizard.dismissed";
    /// JSON `u64`: decoded CHD bytes per active second, from the last image decoded.
    pub const CHD_RATE: &str = "chd.rate";
    /// JSON [`crate::jobs::core_limits::SavedLimits`]: the client's own limits
    /// in each direction the core gate replaced, until they are put back.
    pub const CLIENT_SAVED_LIMITS: &str = "client.saved_limits";
    /// JSON list of [`crate::db::deferred::Entry`]: client work waiting for the client.
    pub const CLIENT_DEFERRED: &str = "client.deferred";
    /// JSON list of [`crate::jobs::core_limits::PreviousLimits`]: limits saved
    /// for clients no longer in use, until they are put back or dropped.
    pub const CLIENT_PREVIOUS_LIMITS: &str = "client.previous_limits";
}

/// Reads a value.
///
/// # Errors
///
/// [`Error::Db`] on SQLite failure.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert_eq!(mistarr_server::db::settings::get(&conn, "none").unwrap(), None);
/// ```
pub fn get(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
            r.get(0)
        })
        .optional()?)
}

/// Inserts or replaces a value.
///
/// # Errors
///
/// [`Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::settings;
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// settings::set(&conn, "k", "v").unwrap();
/// assert_eq!(settings::get(&conn, "k").unwrap().as_deref(), Some("v"));
/// ```
pub fn set(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

/// Reads and deserialises a JSON value.
///
/// # Errors
///
/// [`Error::Db`] on SQLite failure, [`Error::Stored`] when the value is not a `T`.
///
/// ```
/// use mistarr_server::db::settings;
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// settings::set_json(&conn, "n", &3u32).unwrap();
/// assert_eq!(settings::get_json::<u32>(&conn, "n").unwrap(), Some(3));
/// ```
pub fn get_json<T: DeserializeOwned>(conn: &Connection, key: &str) -> Result<Option<T>> {
    get(conn, key)?
        .map(|text| {
            serde_json::from_str(&text).map_err(|source| Error::Stored {
                key: key.to_owned(),
                source,
            })
        })
        .transpose()
}

/// Serialises and stores a JSON value.
///
/// # Errors
///
/// [`Error::Db`] on SQLite failure, [`Error::Stored`] if `value` cannot be serialised.
///
/// ```
/// use mistarr_server::db::settings;
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// settings::set_json(&conn, "v", &vec!["a"]).unwrap();
/// assert_eq!(settings::get(&conn, "v").unwrap().as_deref(), Some(r#"["a"]"#));
/// ```
pub fn set_json<T: Serialize>(conn: &Connection, key: &str, value: &T) -> Result<()> {
    let text = serde_json::to_string(value).map_err(|source| Error::Stored {
        key: key.to_owned(),
        source,
    })?;
    set(conn, key, &text)
}

/// Deletes a value; absent keys are fine.
///
/// # Errors
///
/// [`Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::settings;
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// settings::set(&conn, "k", "v").unwrap();
/// settings::remove(&conn, "k").unwrap();
/// assert_eq!(settings::get(&conn, "k").unwrap(), None);
/// ```
pub fn remove(conn: &Connection, key: &str) -> Result<()> {
    conn.execute("DELETE FROM settings WHERE key = ?1", [key])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let mut c = Connection::open_in_memory().expect("open");
        crate::db::migrate::apply(&mut c).expect("migrate");
        c
    }

    #[test]
    fn set_replaces() {
        let c = conn();
        set(&c, "k", "1").expect("set");
        set(&c, "k", "2").expect("set");
        assert_eq!(get(&c, "k").expect("get").as_deref(), Some("2"));
        assert_eq!(get(&c, "other").expect("get"), None);
        remove(&c, "k").expect("remove");
        remove(&c, "k").expect("remove again");
        assert_eq!(get(&c, "k").expect("get"), None);
    }

    #[test]
    fn json_round_trip_and_bad_value() {
        let c = conn();
        set_json(&c, "j", &serde_json::json!({"a": 1})).expect("set");
        let v: serde_json::Value = get_json(&c, "j").expect("get").expect("some");
        assert_eq!(v["a"], 1);
        set(&c, "bad", "not json").expect("set");
        assert!(matches!(
            get_json::<u32>(&c, "bad"),
            Err(Error::Stored { .. })
        ));
        assert_eq!(get_json::<u32>(&c, "absent").expect("get"), None);
    }
}
