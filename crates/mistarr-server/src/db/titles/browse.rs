//! The browse query over `title_groups`; see `docs/API.md` "Titles".

use std::collections::HashMap;

use mistarr_core::PlatformId;
use rusqlite::types::Value;
use rusqlite::{params_from_iter, Connection, OptionalExtension};
use serde::Serialize;

use crate::db::groups::{self, Clause};
use crate::db::ids::TitleId;
use crate::db::sql::{self, Page, Paged};
use crate::error::Result;

/// Keeps a group of `title_groups g` only when its parent is an MRA title or its platform
/// has no live MRA title, so a platform with MRAs browses its MRA catalogue alone.
const MRA_ONLY: &str = "(g.source = 'mra' OR g.platform_id NOT IN (
    SELECT p.id FROM platforms p WHERE EXISTS (
      SELECT 1 FROM titles m WHERE m.platform_id = p.id AND m.source = 'mra' AND m.retired = 0)))";

/// Catalog counts of one platform for `GET /platforms`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct Counts {
    /// Clone groups with a visible live variant.
    pub titles: u64,
    /// Of those, groups with at least one fully verified live variant, hidden or not.
    pub have: u64,
    /// Of those, groups with at least one wanted live variant, hidden or not.
    pub wanted: u64,
    /// `unverified` files on disk, outside arcade; always 0 for arcade.
    pub unmatched_files: u64,
    /// `unidentified` files on disk: disc images not identified by their tracks.
    pub unidentified_files: u64,
    /// Groups with a visible MRA variant whose md5 check is `mismatch` or
    /// `missing_part` and no fully verified live variant, hidden or not; 0 outside arcade.
    pub failing_check: u64,
    /// Groups with a visible MRA variant that has some, but not every, named zip
    /// present and no fully verified live variant, hidden or not; 0 outside arcade.
    pub partial: u64,
}

/// [`Counts`] per platform id, counting the groups the default browse shows
/// when variants flagged with any of `hidden` do not count; platforms with nothing are absent.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert!(mistarr_server::db::titles::browse::counts(&conn, &[]).unwrap().is_empty());
/// ```
pub fn counts(conn: &Connection, hidden: &[String]) -> Result<HashMap<String, Counts>> {
    let mut out: HashMap<String, Counts> = HashMap::new();
    let mut clause = Clause::default();
    clause.and(MRA_ONLY, []);
    groups::visible(&mut clause, hidden, None, &[]);
    let mut stmt = conn.prepare(&format!(
        "SELECT g.platform_id, COUNT(*), SUM(g.have_verified > 0), SUM(g.wanted > 0)
         FROM title_groups g WHERE {} GROUP BY g.platform_id",
        clause.sql()
    ))?;
    let mut rows = stmt.query(params_from_iter(&clause.args))?;
    while let Some(r) = rows.next()? {
        let e = out.entry(r.get(0)?).or_default();
        e.titles = sql::get_u64(r, 1)?;
        e.have = sql::get_u64(r, 2)?;
        e.wanted = sql::get_u64(r, 3)?;
    }
    let mut stmt = conn.prepare(
        "SELECT platform_id, COUNT(*) FROM files
         WHERE state = 'unverified' AND platform_id <> 'arcade' GROUP BY platform_id",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        out.entry(r.get(0)?).or_default().unmatched_files = sql::get_u64(r, 1)?;
    }
    let mut stmt = conn.prepare(
        "SELECT platform_id, COUNT(*) FROM files WHERE state = 'unidentified' GROUP BY platform_id",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        out.entry(r.get(0)?).or_default().unidentified_files = sql::get_u64(r, 1)?;
    }
    // Per visible MRA title, failing its md5 check or partly present, by clone group;
    // a group with a have-verified variant in title_groups counts as neither.
    let mut stmt = conn.prepare(
        "WITH mra AS (
           SELECT t.platform_id, t.group_root AS parent_id,
                  MAX(COALESCE(t.mra_check IN ('mismatch', 'missing_part'), 0)) AS any_failing,
                  MAX(EXISTS (SELECT 1 FROM roms r
                              WHERE r.title_id = t.id AND r.retired = 0 AND r.present = 1)
                      AND EXISTS (SELECT 1 FROM roms r
                                  WHERE r.title_id = t.id AND r.retired = 0 AND r.present = 0)) AS any_partial
           FROM titles t
           WHERE t.source = 'mra' AND t.retired = 0
             AND NOT EXISTS (SELECT 1 FROM title_flags f
                             WHERE f.title_id = t.id AND f.flag IN (SELECT value FROM json_each(?1)))
           GROUP BY t.platform_id, t.group_root
         )
         SELECT g.platform_id,
                COALESCE(SUM(m.any_failing AND g.have_verified = 0), 0),
                COALESCE(SUM(m.any_partial AND g.have_verified = 0), 0)
         FROM mra m JOIN title_groups g ON g.platform_id = m.platform_id AND g.parent_id = m.parent_id
         GROUP BY g.platform_id",
    )?;
    let mut rows = stmt.query([sql::json_list(hidden)?])?;
    while let Some(r) = rows.next()? {
        let e = out.entry(r.get(0)?).or_default();
        e.failing_check = sql::get_u64(r, 1)?;
        e.partial = sql::get_u64(r, 2)?;
    }
    Ok(out)
}

/// A yes, no or any filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tri {
    /// No filtering.
    #[default]
    Any,
    /// Only matching groups.
    Yes,
    /// Only non-matching groups.
    No,
}

/// Browse order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sort {
    /// By base name.
    #[default]
    Name,
    /// Groups with verified variants first, then by name.
    Have,
    /// Most recently added first.
    Recent,
}

/// Filters of `GET /platforms/{id}/titles`.
#[derive(Debug, Clone, Default)]
pub struct Browse {
    /// Case-insensitive substring of the base name.
    pub q: Option<String>,
    /// Groups with a verified variant.
    pub have: Tri,
    /// Groups with a wanted variant.
    pub wanted: Tri,
    /// A live variant has this region.
    pub region: Option<String>,
    /// A live variant carries every one of these flags.
    pub flags: Vec<String>,
    /// Variants carrying any of these flags do not count as visible.
    pub hidden: Vec<String>,
    /// Order.
    pub sort: Sort,
}

/// One browse row: a clone group from `title_groups`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GroupRow {
    /// Group root.
    pub parent_id: TitleId,
    /// Platform.
    pub platform_id: PlatformId,
    /// The parent's base name.
    pub base_name: String,
    /// The parent's full name.
    pub name: String,
    /// The 1G1R pick, if any variant is selectable.
    pub pick_id: Option<TitleId>,
    /// The pick's full name.
    pub pick_name: Option<String>,
    /// Live variants.
    pub variants: u64,
    /// Variants with every rom verified.
    pub have_verified: u64,
    /// Wanted variants.
    pub wanted: u64,
    /// Whether a pick exists.
    pub has_pick: bool,
    /// Every live variant is flagged `bios`, so none can be wanted.
    pub bios: bool,
}

/// How a search of three or more characters finds its groups; shorter ones always use
/// [`SearchShape::Like`]. `mistarr bench-search` times each on a real database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SearchShape {
    /// `LIKE` over the platform's `title_groups_name` range.
    Like,
    /// The trigram index over every platform, probed while walking the platform's groups.
    Fts,
    /// The trigram index, filtered to the platform's sentinel-wrapped id in the same `MATCH`,
    /// plus the platform's groups whose parent title is on another platform.
    FtsPlatform,
}

impl SearchShape {
    /// Every shape, in [`SearchShape::name`] order.
    pub const ALL: [Self; 3] = [Self::Like, Self::Fts, Self::FtsPlatform];

    /// The shape's command-line name.
    ///
    /// ```
    /// use mistarr_server::db::titles::browse::SearchShape;
    /// assert_eq!(SearchShape::from_name(SearchShape::Fts.name()), Some(SearchShape::Fts));
    /// ```
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Like => "like",
            Self::Fts => "fts",
            Self::FtsPlatform => "fts-platform",
        }
    }

    /// The shape named `name`.
    ///
    /// ```
    /// use mistarr_server::db::titles::browse::SearchShape;
    /// assert_eq!(SearchShape::from_name("like"), Some(SearchShape::Like));
    /// assert_eq!(SearchShape::from_name("grep"), None);
    /// ```
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.name() == name)
    }
}

/// The shape [`browse`] uses: the best worst case on real DATs on the board; see
/// `docs/TESTING.md` "Browse speed".
pub const SEARCH_SHAPE: SearchShape = SearchShape::FtsPlatform;

/// The conditions of a browse request on `platform`, over `title_groups g`.
pub(super) fn browse_clause(
    conn: &Connection,
    platform: &PlatformId,
    filter: &Browse,
    shape: SearchShape,
) -> Result<Clause> {
    let mut clause = Clause::default();
    clause.and(
        "g.platform_id = ?",
        [Value::Text(platform.as_str().to_owned())],
    );
    let mra: bool = conn
        .prepare(
            "SELECT EXISTS (SELECT 1 FROM titles
             WHERE platform_id = ?1 AND source = 'mra' AND retired = 0)",
        )?
        .query_row([platform], |r| r.get(0))?;
    if mra {
        clause.and("g.source = 'mra'", []);
    }
    if let Some(q) = filter.q.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
        if q.chars().count() >= TRIGRAM {
            match shape {
                SearchShape::Like => {}
                SearchShape::Fts => clause.and(SEARCH, [Value::Text(phrase(q))]),
                SearchShape::FtsPlatform => clause.and(
                    SEARCH_PLATFORM,
                    [
                        Value::Text(format!(
                            "platform : \"\u{1f}{}\u{1f}\" AND base_name : {}",
                            platform.as_str(),
                            phrase(q)
                        )),
                        Value::Text(platform.as_str().to_owned()),
                    ],
                ),
            }
        }
        // The index folds all of Unicode's case; LIKE keeps the ASCII-only match exact.
        clause.and(
            "g.base_name LIKE ? ESCAPE '\\'",
            [Value::Text(like_pattern(q))],
        );
    }
    match filter.have {
        Tri::Any => {}
        Tri::Yes => clause.and("g.have_verified > 0", []),
        Tri::No => clause.and("g.have_verified = 0", []),
    }
    match filter.wanted {
        Tri::Any => {}
        Tri::Yes => clause.and("g.wanted > 0", []),
        Tri::No => clause.and("g.wanted = 0", []),
    }
    groups::visible(
        &mut clause,
        &filter.hidden,
        filter.region.as_deref(),
        &filter.flags,
    );
    Ok(clause)
}

/// The page query for `clause` in `sort` order, taking `LIMIT` and `OFFSET` last.
pub(super) fn page_sql(clause: &Clause, sort: Sort) -> String {
    let order = match sort {
        Sort::Name => "g.base_name COLLATE NOCASE, g.parent_id",
        Sort::Have => "g.have_verified > 0 DESC, g.base_name COLLATE NOCASE, g.parent_id",
        Sort::Recent => "g.newest_id DESC",
    };
    // Only groups with a BIOS variant look at their variants' flags.
    format!(
        "SELECT g.parent_id, g.platform_id, g.base_name, g.name, g.pick_id, k.name,
                g.variants, g.have_verified, g.wanted, g.has_pick,
                CASE WHEN g.flag_union & {bios} = 0 THEN 0 ELSE NOT EXISTS (
                    SELECT 1 FROM titles v WHERE v.group_root = g.parent_id AND v.retired = 0
                    AND NOT EXISTS (SELECT 1 FROM title_flags f
                                    WHERE f.title_id = v.id AND f.flag = 'bios')) END
         FROM title_groups g LEFT JOIN titles k ON k.id = g.pick_id
         WHERE {} ORDER BY {order} LIMIT ? OFFSET ?",
        clause.sql(),
        bios = groups::BIOS_BIT
    )
}

/// Shortest search the trigram index can serve; shorter ones scan the platform's names.
const TRIGRAM: usize = 3;

/// Keeps groups whose parent's base name contains the phrase bound to `?`, found through
/// `title_search` and probed while the platform's name index is walked in order.
const SEARCH: &str = "g.parent_id IN (SELECT rowid FROM title_search WHERE title_search MATCH ?)";

/// [`SEARCH`] with a `MATCH` limited to the platform, plus the platform's groups whose
/// parent is elsewhere, found through the partial `title_groups_split` index.
const SEARCH_PLATFORM: &str = "g.parent_id IN (
    SELECT rowid FROM title_search WHERE title_search MATCH ?
    UNION ALL SELECT s.parent_id FROM title_groups s WHERE s.platform_id = ? AND s.split)";

/// `q` as one FTS5 phrase, so every character is literal.
fn phrase(q: &str) -> String {
    format!("\"{}\"", q.replace('"', "\"\""))
}

fn like_pattern(q: &str) -> String {
    let mut out = String::with_capacity(q.len() + 2);
    out.push('%');
    for c in q.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

/// One page of clone groups on `platform` matching `filter`, with the total.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::sql::Page;
/// use mistarr_server::db::titles::browse::{browse, Browse};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let page = Page { limit: 10, offset: 0 };
/// let nes = mistarr_core::PlatformId::new("nes");
/// let got = browse(&conn, &nes, &Browse::default(), page).unwrap();
/// assert!(got.items.is_empty() && got.total == 0);
/// ```
pub fn browse(
    conn: &Connection,
    platform: &PlatformId,
    filter: &Browse,
    page: Page,
) -> Result<Paged<GroupRow>> {
    browse_with(conn, platform, filter, page, SEARCH_SHAPE)
}

/// [`browse`] with the search done by `shape`, for comparing shapes.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::sql::Page;
/// use mistarr_server::db::titles::browse::{browse_with, Browse, SearchShape};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let filter = Browse { q: Some("quest".into()), ..Browse::default() };
/// let page = Page { limit: 10, offset: 0 };
/// let got = browse_with(&conn, &mistarr_core::PlatformId::new("nes"), &filter, page, SearchShape::Like).unwrap();
/// assert!(got.items.is_empty() && got.total == 0);
/// ```
pub fn browse_with(
    conn: &Connection,
    platform: &PlatformId,
    filter: &Browse,
    page: Page,
    shape: SearchShape,
) -> Result<Paged<GroupRow>> {
    sql::snapshot(conn, |conn| browse_in(conn, platform, filter, page, shape))
}

fn browse_in(
    conn: &Connection,
    platform: &PlatformId,
    filter: &Browse,
    page: Page,
    shape: SearchShape,
) -> Result<Paged<GroupRow>> {
    let clause = browse_clause(conn, platform, filter, shape)?;
    let total = conn
        .prepare(&format!(
            "SELECT COUNT(*) FROM title_groups g WHERE {}",
            clause.sql()
        ))?
        .query_row(params_from_iter(&clause.args), |r| sql::get_u64(r, 0))?;
    let mut stmt = conn.prepare(&page_sql(&clause, filter.sort))?;
    let args = clause
        .args
        .iter()
        .cloned()
        .chain([Value::from(page.limit), Value::from(page.offset)]);
    let items = stmt
        .query_map(params_from_iter(args), |r| {
            Ok(GroupRow {
                parent_id: r.get(0)?,
                platform_id: r.get(1)?,
                base_name: r.get(2)?,
                name: r.get(3)?,
                pick_id: r.get(4)?,
                pick_name: r.get(5)?,
                variants: sql::get_u64(r, 6)?,
                have_verified: sql::get_u64(r, 7)?,
                wanted: sql::get_u64(r, 8)?,
                has_pick: r.get(9)?,
                bios: r.get(10)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(Paged { items, total })
}

/// The clone-group root of a title, or `None` when there is no such title.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::titles::browse::group_of;
/// use mistarr_server::db::ids::TitleId;
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert!(group_of(&conn, TitleId::new(1)).unwrap().is_none());
/// ```
pub fn group_of(conn: &Connection, id: TitleId) -> Result<Option<TitleId>> {
    Ok(conn
        .query_row(
            "SELECT COALESCE(group_root, parent_id, id) FROM titles WHERE id = ?1",
            [id],
            |r| r.get(0),
        )
        .optional()?)
}
