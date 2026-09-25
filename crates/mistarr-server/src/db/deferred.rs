//! Client work waiting for the client; see `docs/DOWNLOAD-CLIENTS.md` "Core gate".

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::db::settings::{self, keys};
use crate::db::sources::SourceId;
use crate::error::Result;

/// Failed tries after which, once [`DROP_AFTER_SECS`] have also passed, an entry is dropped.
pub const DROP_AFTER_TRIES: u32 = 20;

/// Seconds after an entry was kept after which, once it has also failed
/// [`DROP_AFTER_TRIES`] times, it is dropped.
pub const DROP_AFTER_SECS: i64 = 24 * 3600;

/// One piece of client work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", content = "source", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Op {
    /// Detect the client again.
    Detect,
    /// Apply every source's seed policy again.
    Seed,
    /// Apply a source's selection again.
    Deselect(SourceId),
    /// Release a source's finished torrent.
    Release(SourceId),
}

/// Work waiting for the client, stored in a list under [`keys::CLIENT_DEFERRED`]
/// so a restart keeps it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// The work.
    pub op: Op,
    /// When it was kept, in seconds since the epoch.
    pub since: i64,
    /// Tries the client refused so far.
    pub tries: u32,
}

/// The work waiting now, oldest first.
///
/// # Errors
///
/// [`crate::Error::Db`] or [`crate::Error::Stored`] when it cannot be read.
pub fn get(conn: &Connection) -> Result<Vec<Entry>> {
    Ok(settings::get_json(conn, keys::CLIENT_DEFERRED)?.unwrap_or_default())
}

fn put(conn: &Connection, list: &[Entry]) -> Result<()> {
    if list.is_empty() {
        settings::remove(conn, keys::CLIENT_DEFERRED)
    } else {
        settings::set_json(conn, keys::CLIENT_DEFERRED, &list)
    }
}

/// Keeps `op` from `now` on; true when it was not waiting already, false
/// when it was and nothing was written.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn add(conn: &Connection, op: Op, now: i64) -> Result<bool> {
    let mut list = get(conn)?;
    if list.iter().any(|e| e.op == op) {
        return Ok(false);
    }
    list.push(Entry {
        op,
        since: now,
        tries: 0,
    });
    put(conn, &list)?;
    Ok(true)
}

/// Records how the tried work went: an op in `done` is removed, one in
/// `refused` counts a try and is dropped once it has failed
/// [`DROP_AFTER_TRIES`] times and waited [`DROP_AFTER_SECS`]. Work added since
/// it was read is kept. Returns the dropped entries.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn finish(conn: &Connection, done: &[Op], refused: &[Op], now: i64) -> Result<Vec<Entry>> {
    let mut dropped = Vec::new();
    let mut list = get(conn)?;
    list.retain_mut(|e| {
        if done.contains(&e.op) {
            return false;
        }
        if refused.contains(&e.op) {
            e.tries += 1;
            if e.tries >= DROP_AFTER_TRIES && now - e.since >= DROP_AFTER_SECS {
                dropped.push(*e);
                return false;
            }
        }
        true
    });
    put(conn, &list)?;
    Ok(dropped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let mut c = Connection::open_in_memory().expect("open");
        crate::db::migrate::apply(&mut c).expect("migrate");
        c
    }

    fn ops(c: &Connection) -> Vec<Op> {
        get(c).expect("get").into_iter().map(|e| e.op).collect()
    }

    #[test]
    fn work_is_added_once_and_removed_only_as_done() {
        let c = conn();
        assert!(get(&c).expect("get").is_empty());
        assert!(add(&c, Op::Deselect(SourceId(1)), 5).expect("add"));
        assert!(!add(&c, Op::Deselect(SourceId(1)), 6).expect("again"));
        assert!(add(&c, Op::Release(SourceId(2)), 5).expect("add"));
        assert!(add(&c, Op::Detect, 5).expect("add"));
        assert!(
            !add(&c, Op::Detect, 7).expect("again"),
            "an unchanged queue is not written"
        );
        assert_eq!(get(&c).expect("get")[0].since, 5);
        let dropped = finish(&c, &[Op::Detect], &[Op::Release(SourceId(2))], 9).expect("finish");
        assert!(dropped.is_empty());
        assert_eq!(
            ops(&c),
            [Op::Deselect(SourceId(1)), Op::Release(SourceId(2))]
        );
        assert_eq!(get(&c).expect("get")[1].tries, 1);
        finish(
            &c,
            &[Op::Deselect(SourceId(1)), Op::Release(SourceId(2))],
            &[],
            9,
        )
        .expect("finish");
        assert_eq!(settings::get(&c, keys::CLIENT_DEFERRED).expect("get"), None);
    }

    #[test]
    fn refused_work_is_dropped_after_a_day_and_twenty_tries() {
        let c = conn();
        add(&c, Op::Seed, 0).expect("add");
        for _ in 0..DROP_AFTER_TRIES + 5 {
            assert!(finish(&c, &[], &[Op::Seed], 60).expect("finish").is_empty());
        }
        assert_eq!(ops(&c), [Op::Seed], "tries alone do not drop it");
        let dropped = finish(&c, &[], &[Op::Seed], DROP_AFTER_SECS).expect("finish");
        assert_eq!(dropped.len(), 1);
        assert!(ops(&c).is_empty());

        add(&c, Op::Detect, 0).expect("add");
        let late = DROP_AFTER_SECS * 2;
        for _ in 1..DROP_AFTER_TRIES {
            assert!(finish(&c, &[], &[Op::Detect], late)
                .expect("finish")
                .is_empty());
        }
        assert_eq!(ops(&c), [Op::Detect], "a day alone does not drop it");
        assert_eq!(
            finish(&c, &[], &[Op::Detect], late).expect("finish").len(),
            1
        );
    }

    #[test]
    fn ops_are_stored_by_name() {
        let json = serde_json::to_string(&Op::Release(SourceId(4))).expect("json");
        assert_eq!(json, r#"{"op":"release","source":4}"#);
        assert_eq!(
            serde_json::to_string(&Op::Seed).expect("json"),
            r#"{"op":"seed"}"#
        );
    }
}
