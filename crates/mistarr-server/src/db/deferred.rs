//! Client work held while the client is frozen; see `docs/DOWNLOAD-CLIENTS.md` "Core gate".

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::db::settings::{self, keys};
use crate::db::sources::SourceId;
use crate::error::Result;

/// The work waiting for the client to resume, stored under [`keys::CLIENT_DEFERRED`]
/// so a restart keeps it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deferred {
    /// Client detection was asked for.
    #[serde(default)]
    pub detect: bool,
    /// A seed policy changed, so every source's is applied again.
    #[serde(default)]
    pub seed: bool,
    /// Sources whose selection is applied again.
    #[serde(default)]
    pub deselect: Vec<SourceId>,
    /// Sources whose finished torrent is released from the client.
    #[serde(default)]
    pub release: Vec<SourceId>,
}

impl Deferred {
    /// Whether nothing waits.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// One piece of deferred work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// The work waiting now.
///
/// # Errors
///
/// [`crate::Error::Db`] or [`crate::Error::Stored`] when it cannot be read.
pub fn get(conn: &Connection) -> Result<Deferred> {
    Ok(settings::get_json(conn, keys::CLIENT_DEFERRED)?.unwrap_or_default())
}

/// Adds `op`; a source already waiting is kept once. True when the waiting
/// work changed, false when `op` already waited and nothing was written.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn add(conn: &Connection, op: Op) -> Result<bool> {
    let before = get(conn)?;
    let mut d = before.clone();
    let push = |list: &mut Vec<SourceId>, id: SourceId| {
        if !list.contains(&id) {
            list.push(id);
        }
    };
    match op {
        Op::Detect => d.detect = true,
        Op::Seed => d.seed = true,
        Op::Deselect(id) => push(&mut d.deselect, id),
        Op::Release(id) => push(&mut d.release, id),
    }
    if d == before {
        return Ok(false);
    }
    settings::set_json(conn, keys::CLIENT_DEFERRED, &d)?;
    Ok(true)
}

/// Removes the work in `done`, keeping anything added since it was read.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn clear(conn: &Connection, done: &Deferred) -> Result<()> {
    let mut d = get(conn)?;
    d.detect &= !done.detect;
    d.seed &= !done.seed;
    d.deselect.retain(|id| !done.deselect.contains(id));
    d.release.retain(|id| !done.release.contains(id));
    if d.is_empty() {
        settings::remove(conn, keys::CLIENT_DEFERRED)
    } else {
        settings::set_json(conn, keys::CLIENT_DEFERRED, &d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_is_added_once_and_cleared_only_as_done() {
        let mut c = Connection::open_in_memory().expect("open");
        crate::db::migrate::apply(&mut c).expect("migrate");
        assert!(get(&c).expect("get").is_empty());
        assert!(add(&c, Op::Deselect(SourceId(1))).expect("add"));
        assert!(!add(&c, Op::Deselect(SourceId(1))).expect("again"));
        add(&c, Op::Release(SourceId(2))).expect("add");
        add(&c, Op::Detect).expect("add");
        assert!(
            !add(&c, Op::Detect).expect("again"),
            "an unchanged queue is not written"
        );
        let done = get(&c).expect("get");
        assert_eq!(done.deselect, [SourceId(1)]);
        assert!(done.detect && !done.seed);
        add(&c, Op::Seed).expect("add");
        add(&c, Op::Deselect(SourceId(3))).expect("add");
        clear(&c, &done).expect("clear");
        let left = get(&c).expect("get");
        assert_eq!(
            left,
            Deferred {
                seed: true,
                deselect: vec![SourceId(3)],
                ..Deferred::default()
            }
        );
        clear(&c, &left).expect("clear");
        assert_eq!(settings::get(&c, keys::CLIENT_DEFERRED).expect("get"), None);
    }
}
