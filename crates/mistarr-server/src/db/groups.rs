//! The maintained `title_groups` table, its visibility summary and the `title_search`
//! index; see `docs/DATA-MODEL.md` "Derived tables".

use rusqlite::types::Value;
use rusqlite::Connection;

use super::titles::TitleId;
use crate::error::Result;

/// Flags with their own bit in the group summary, in bit order; `known_flags` holds the same.
pub const KNOWN_FLAGS: [&str; 9] = [
    "bios", "beta", "proto", "demo", "sample", "unl", "pirate", "program", "baddump",
];

/// Regions with their own bit in the group summary, in bit order, matched ignoring
/// ASCII case; `known_regions` holds the same.
pub const KNOWN_REGIONS: [&str; 22] = [
    "World",
    "USA",
    "Europe",
    "Japan",
    "Asia",
    "Australia",
    "Brazil",
    "Canada",
    "China",
    "France",
    "Germany",
    "Hong Kong",
    "Italy",
    "Korea",
    "Netherlands",
    "Russia",
    "Scandinavia",
    "Spain",
    "Sweden",
    "Taiwan",
    "UK",
    "Latin America",
];

/// The bit for any flag or region outside the known lists.
pub const OTHER: i64 = 1 << 62;

/// Every known flag bit.
const KNOWN_FLAG_MASK: i64 = (1 << KNOWN_FLAGS.len()) - 1;

/// More dirty groups than this rebuild their whole platforms instead.
const REBUILD_OVER: i64 = 4096;

/// The columns of `title_groups`, in table order.
const COLUMNS: &str = "parent_id, platform_id, base_name, name, variants, have_verified, wanted,
    has_pick, pick_id, newest_id, source, unflagged, unflagged_regions, flag_union, region_union";

/// The bit of a known flag, compared exactly.
///
/// ```
/// use mistarr_server::db::groups::flag_bit;
/// assert_eq!(flag_bit("bios"), Some(1));
/// assert_eq!(flag_bit("BIOS"), None);
/// ```
#[must_use]
pub fn flag_bit(name: &str) -> Option<i64> {
    KNOWN_FLAGS
        .iter()
        .position(|f| *f == name)
        .map(|i| 1_i64 << i)
}

/// The bit of a known region, ignoring ASCII case as `COLLATE NOCASE` does.
///
/// ```
/// use mistarr_server::db::groups::region_bit;
/// assert_eq!(region_bit("usa"), Some(2));
/// assert_eq!(region_bit("Atlantis"), None);
/// ```
#[must_use]
pub fn region_bit(name: &str) -> Option<i64> {
    KNOWN_REGIONS
        .iter()
        .position(|r| r.eq_ignore_ascii_case(name))
        .map(|i| 1_i64 << i)
}

/// `(MAX(expr & b1) | MAX(expr & b2) | ...)`: the bitwise OR of `expr` over a group.
fn or_all(expr: &str, count: usize) -> String {
    let parts: Vec<String> = (0..count)
        .map(|i| format!("MAX({expr} & {})", 1_i64 << i))
        .chain(std::iter::once(format!("MAX({expr} & {OTHER})")))
        .collect();
    format!("({})", parts.join(" | "))
}

/// Which titles a recomputation covers.
#[derive(Clone, Copy)]
enum Scope {
    /// Groups whose parent is in `title_groups_dirty`.
    Dirty,
    /// Groups on the platform bound to `?1`.
    Platform,
    /// Every group.
    All,
}

/// The `title_groups` rows of `scope`, computed from `titles`, `roms`, `files` and the
/// flag and region tables.
fn select(scope: Scope) -> String {
    let (members, parents) = match scope {
        Scope::Dirty => (
            "t.parent_id IN (SELECT parent_id FROM title_groups_dirty)",
            "t.parent_id IN (SELECT parent_id FROM title_groups_dirty)",
        ),
        Scope::Platform => (
            "t.platform_id = ?1",
            "t.parent_id IN (SELECT parent_id FROM titles WHERE platform_id = ?1 AND retired = 0)",
        ),
        Scope::All => ("1", "1"),
    };
    let unflagged_regions = or_all(
        &format!("(CASE WHEN b.fb & {KNOWN_FLAG_MASK} = 0 THEN b.rb ELSE 0 END)"),
        KNOWN_REGIONS.len(),
    );
    let flag_union = or_all("b.fb", KNOWN_FLAGS.len());
    let region_union = or_all("b.rb", KNOWN_REGIONS.len());
    format!(
        "SELECT g.parent_id, g.platform_id, p.base_name, p.name, g.variants, g.have_verified,
                g.wanted, g.has_pick, g.pick_id, g.newest_id, p.source,
                u.unflagged, u.unflagged_regions, u.flag_union, u.region_union
         FROM (
           SELECT v.platform_id, v.parent_id, COUNT(*) AS variants,
                  SUM(v.roms > 0 AND v.roms_verified = v.roms) AS have_verified,
                  SUM(v.wanted) AS wanted, MAX(v.is_1g1r_pick) AS has_pick,
                  MAX(CASE WHEN v.is_1g1r_pick = 1 THEN v.id END) AS pick_id,
                  MAX(v.id) AS newest_id
           FROM (
             SELECT t.platform_id, t.parent_id, t.id, t.wanted, t.is_1g1r_pick,
                    (SELECT COUNT(*) FROM roms r WHERE r.title_id = t.id AND r.retired = 0) AS roms,
                    (SELECT COUNT(*) FROM roms r WHERE r.title_id = t.id AND r.retired = 0
                       AND ((r.present = 1
                             AND COALESCE(t.mra_check, '') NOT IN ('mismatch', 'missing_part'))
                            OR EXISTS (SELECT 1 FROM files f
                                       WHERE f.rom_id = r.id AND f.state = 'verified')))
                      AS roms_verified
             FROM titles t WHERE t.retired = 0 AND t.parent_id IS NOT NULL AND {members}
           ) v
           GROUP BY v.platform_id, v.parent_id
         ) g
         JOIN titles p ON p.id = g.parent_id
         JOIN (
           WITH b AS MATERIALIZED (
             SELECT t.parent_id,
                    (SELECT COALESCE(SUM(DISTINCT COALESCE(k.bit, {OTHER})), 0)
                     FROM title_flags f LEFT JOIN known_flags k ON k.name = f.flag
                     WHERE f.title_id = t.id) AS fb,
                    (SELECT COALESCE(SUM(DISTINCT COALESCE(k.bit, {OTHER})), 0)
                     FROM title_regions r
                     LEFT JOIN known_regions k ON k.name = r.region COLLATE NOCASE
                     WHERE r.title_id = t.id) AS rb
             FROM titles t WHERE t.retired = 0 AND t.parent_id IS NOT NULL AND {parents}
           )
           SELECT b.parent_id, MAX(b.fb & {KNOWN_FLAG_MASK} = 0) AS unflagged,
                  {unflagged_regions} AS unflagged_regions,
                  {flag_union} AS flag_union, {region_union} AS region_union
           FROM b GROUP BY b.parent_id
         ) u ON u.parent_id = g.parent_id"
    )
}

/// Recomputes the rows of the groups in `title_groups_dirty` and empties it.
fn refresh_dirty(conn: &Connection) -> Result<usize> {
    conn.prepare_cached(
        "DELETE FROM title_groups WHERE parent_id IN (SELECT parent_id FROM title_groups_dirty)",
    )?
    .execute([])?;
    let rows = conn
        .prepare_cached(&format!(
            "INSERT INTO title_groups ({COLUMNS}) {}",
            select(Scope::Dirty)
        ))?
        .execute([])?;
    conn.prepare_cached("DELETE FROM title_groups_dirty")?
        .execute([])?;
    Ok(rows)
}

/// Recomputes the `title_groups` rows of the groups rooted at `parent_ids`, on every
/// platform, with any other dirty group, and returns how many rows they have now.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::{groups, titles::TitleId};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert_eq!(groups::refresh_groups(&conn, &[TitleId(1)]).unwrap(), 0);
/// ```
pub fn refresh_groups(conn: &Connection, parent_ids: &[TitleId]) -> Result<usize> {
    let mut mark = conn.prepare_cached(
        "INSERT INTO title_groups_dirty (parent_id) SELECT ?1
         WHERE ?1 NOT IN (SELECT parent_id FROM title_groups_dirty)",
    )?;
    for id in parent_ids {
        mark.execute([id.0])?;
    }
    refresh_dirty(conn)
}

/// Recomputes every `title_groups` row of `platform` and returns how many it has.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert_eq!(mistarr_server::db::groups::rebuild_platform(&conn, "nes").unwrap(), 0);
/// ```
pub fn rebuild_platform(conn: &Connection, platform: &str) -> Result<usize> {
    conn.execute(
        "DELETE FROM title_groups WHERE platform_id = ?1",
        [platform],
    )?;
    Ok(conn.execute(
        &format!(
            "INSERT INTO title_groups ({COLUMNS}) {}",
            select(Scope::Platform)
        ),
        [platform],
    )?)
}

/// Recomputes the whole table and the search index, and empties the dirty list;
/// returns the row count.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert_eq!(mistarr_server::db::groups::rebuild(&conn).unwrap(), 0);
/// ```
pub fn rebuild(conn: &Connection) -> Result<usize> {
    conn.execute(
        "INSERT INTO title_search (title_search) VALUES ('rebuild')",
        [],
    )?;
    conn.execute("DELETE FROM title_groups", [])?;
    let rows = conn.execute(
        &format!(
            "INSERT INTO title_groups ({COLUMNS}) {}",
            select(Scope::All)
        ),
        [],
    )?;
    conn.execute("DELETE FROM title_groups_dirty", [])?;
    Ok(rows)
}

/// Refreshes the groups the triggers marked dirty in this transaction and empties the
/// list; returns how many groups were dirty. [`super::commit`] calls it before committing.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert_eq!(mistarr_server::db::groups::flush(&conn).unwrap(), 0);
/// ```
pub fn flush(conn: &Connection) -> Result<usize> {
    let dirty: i64 = conn
        .prepare_cached("SELECT COUNT(*) FROM (SELECT 1 FROM title_groups_dirty LIMIT ?1)")?
        .query_row([REBUILD_OVER + 1], |r| r.get(0))?;
    if dirty == 0 {
        return Ok(0);
    }
    if dirty <= REBUILD_OVER {
        refresh_dirty(conn)?;
        return Ok(usize::try_from(dirty).unwrap_or(0));
    }
    let dirty: i64 = conn.query_row("SELECT COUNT(*) FROM title_groups_dirty", [], |r| r.get(0))?;
    let platforms: Vec<String> = conn
        .prepare(
            "SELECT platform_id FROM titles
             WHERE parent_id IN (SELECT parent_id FROM title_groups_dirty)
             UNION
             SELECT platform_id FROM title_groups
             WHERE parent_id IN (SELECT parent_id FROM title_groups_dirty)",
        )?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for p in &platforms {
        rebuild_platform(conn, p)?;
    }
    conn.execute("DELETE FROM title_groups_dirty", [])?;
    Ok(usize::try_from(dirty).unwrap_or(0))
}

/// Whether writes are waiting for [`flush`]; readers never see this outside a crash
/// between a write and its commit, which rolls both back.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert!(!mistarr_server::db::groups::pending(&conn).unwrap());
/// ```
pub fn pending(conn: &Connection) -> Result<bool> {
    Ok(conn
        .prepare_cached("SELECT EXISTS (SELECT 1 FROM title_groups_dirty)")?
        .query_row([], |r| r.get(0))?)
}

/// How far `title_groups` and `title_search` are from a fresh computation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Drift {
    /// Rows of the table no fresh computation produces.
    pub stale: u64,
    /// Fresh rows the table lacks.
    pub missing: u64,
    /// Groups still marked dirty.
    pub dirty: u64,
    /// Whether the search index disagrees with the titles it indexes.
    pub search: bool,
}

impl Drift {
    /// True when the table and the index match their inputs exactly.
    ///
    /// ```
    /// assert!(mistarr_server::db::groups::Drift::default().is_consistent());
    /// ```
    #[must_use]
    pub fn is_consistent(&self) -> bool {
        *self == Self::default()
    }
}

/// Compares the table and the search index with their inputs, for `mistarr doctor`
/// and tests. It reads every title, rom and file, so it runs for seconds on the board.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure other than a failed index check.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert!(mistarr_server::db::groups::check(&conn).unwrap().is_consistent());
/// ```
pub fn check(conn: &Connection) -> Result<Drift> {
    let fresh = select(Scope::All);
    let count = |sql: &str| -> Result<u64> {
        let n: i64 = conn.query_row(sql, [], |r| r.get(0))?;
        Ok(u64::try_from(n).unwrap_or(0))
    };
    let search = match conn.execute(
        "INSERT INTO title_search (title_search, rank) VALUES ('integrity-check', 1)",
        [],
    ) {
        Ok(_) => false,
        Err(rusqlite::Error::SqliteFailure(e, _))
            if e.code == rusqlite::ErrorCode::DatabaseCorrupt =>
        {
            true
        }
        Err(e) => return Err(e.into()),
    };
    Ok(Drift {
        stale: count(&format!(
            "SELECT COUNT(*) FROM (SELECT {COLUMNS} FROM title_groups EXCEPT {fresh})"
        ))?,
        missing: count(&format!(
            "SELECT COUNT(*) FROM ({fresh} EXCEPT SELECT {COLUMNS} FROM title_groups)"
        ))?,
        dirty: count("SELECT COUNT(*) FROM title_groups_dirty")?,
        search,
    })
}

/// A `WHERE` clause built with positional `?` parameters in text order.
#[derive(Debug, Default)]
pub(crate) struct Clause {
    /// The conditions joined with `AND`, or empty.
    pub text: String,
    /// The values for the `?`s in `text`.
    pub args: Vec<Value>,
}

impl Clause {
    /// Adds a condition whose `?`s take `args`, in order.
    pub fn and(&mut self, cond: &str, args: impl IntoIterator<Item = Value>) {
        if !self.text.is_empty() {
            self.text.push_str(" AND ");
        }
        self.text.push('(');
        self.text.push_str(cond);
        self.text.push(')');
        self.args.extend(args);
    }

    /// The clause, or `1` when it has no condition.
    pub fn sql(&self) -> &str {
        if self.text.is_empty() {
            "1"
        } else {
            &self.text
        }
    }
}

/// `?, ?, ?` for `n` parameters.
pub(crate) fn placeholders(n: usize) -> String {
    vec!["?"; n].join(", ")
}

/// Adds to `clause` the browse visibility of group `g`: a live variant of its parent
/// carries none of `hidden`, has `region` when given, and carries every one of `required`.
/// The summary columns decide most groups; the rest check each variant's flag and region rows.
pub(crate) fn visible(
    clause: &mut Clause,
    hidden: &[String],
    region: Option<&str>,
    required: &[String],
) {
    let known = |names: &[String]| -> (i64, bool) {
        names
            .iter()
            .fold((0, false), |(bits, other), n| match flag_bit(n) {
                Some(b) => (bits | b, other),
                None => (bits, true),
            })
    };
    let (_, hidden_other) = known(hidden);
    let (rk, required_other) = known(required);
    let region_bit = region.map(region_bit);
    if hidden.is_empty() && required.is_empty() {
        match region_bit {
            None => return,
            Some(Some(b)) => {
                clause.and(&format!("g.region_union & {b} != 0"), []);
                return;
            }
            Some(None) => {}
        }
    }
    let mut necessary = Vec::new();
    match region_bit {
        Some(Some(b)) => necessary.push(format!("g.region_union & {b} != 0")),
        Some(None) => necessary.push(format!("g.region_union & {OTHER} != 0")),
        None => {}
    }
    if rk != 0 {
        necessary.push(format!("g.flag_union & {rk} = {rk}"));
    }
    if required_other {
        necessary.push(format!("g.flag_union & {OTHER} != 0"));
    }
    let sufficient = if required.is_empty() && !hidden_other {
        match region_bit {
            None => Some("g.unflagged".to_owned()),
            Some(Some(b)) => Some(format!("g.unflagged_regions & {b} != 0")),
            Some(None) => None,
        }
    } else {
        None
    };
    let mut variant = String::from("v.parent_id = g.parent_id AND v.retired = 0");
    let mut args = Vec::new();
    if !hidden.is_empty() {
        variant.push_str(
            " AND NOT EXISTS (SELECT 1 FROM title_flags f
                              WHERE f.title_id = v.id AND f.flag IN (",
        );
        variant.push_str(&placeholders(hidden.len()));
        variant.push_str("))");
        args.extend(hidden.iter().map(|h| Value::Text(h.clone())));
    }
    if let Some(r) = region {
        variant.push_str(
            " AND EXISTS (SELECT 1 FROM title_regions r
                          WHERE r.title_id = v.id AND r.region = ? COLLATE NOCASE)",
        );
        args.push(Value::Text(r.to_owned()));
    }
    for flag in required {
        variant.push_str(
            " AND EXISTS (SELECT 1 FROM title_flags f WHERE f.title_id = v.id AND f.flag = ?)",
        );
        args.push(Value::Text(flag.clone()));
    }
    let exists = format!("EXISTS (SELECT 1 FROM titles v WHERE {variant})");
    let tail = match sufficient {
        Some(s) => format!("{s} OR {exists}"),
        None => exists,
    };
    necessary.push(format!("({tail})"));
    clause.and(&necessary.join(" AND "), args);
}

#[cfg(test)]
mod tests;
