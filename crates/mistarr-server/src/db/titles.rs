//! The `titles` and `roms` tables, clone groups, 1G1R picks and the browse
//! query over `title_groups`; see `docs/DATA-MODEL.md` and `docs/VERIFICATION.md`.

use std::collections::HashMap;
use std::fmt;

use mistarr_core::naming::{parse_name, ParsedName};
use mistarr_core::select::{infer_groups, select_1g1r, Prefs, Variant};
use mistarr_core::PlatformId;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use super::arcade::MraInfo;
use super::dats::DatVersionId;
use crate::error::Result;

/// A `titles.id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TitleId(pub i64);

impl fmt::Display for TitleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// One `<game>` with its name already parsed.
#[derive(Debug, Clone, Copy)]
pub struct TitleInput<'a> {
    /// Full DAT game name.
    pub name: &'a str,
    /// Name without tags.
    pub base_name: &'a str,
    /// `naming::group_key` of the name.
    pub group_key: &'a str,
    /// The DAT's `cloneof`, if any.
    pub clone_of: Option<&'a str>,
    /// Region names.
    pub regions: &'a [String],
    /// Language codes.
    pub languages: &'a [String],
    /// Revision label.
    pub revision: Option<&'a str>,
    /// Flag labels.
    pub flags: &'a [String],
}

/// One `<rom>` of a game.
#[derive(Debug, Clone, Copy)]
pub struct RomInput<'a> {
    /// File name in the DAT.
    pub name: &'a str,
    /// Size in bytes.
    pub size: u64,
    /// Lowercase hex CRC32.
    pub crc32: Option<&'a str>,
    /// Lowercase hex MD5.
    pub md5: Option<&'a str>,
    /// Lowercase hex SHA1.
    pub sha1: Option<&'a str>,
    /// `good`, `baddump`, `nodump` or `verified`.
    pub status: &'a str,
    /// The DAT's `header` attribute, verbatim.
    pub header: Option<&'a str>,
}

fn json(xs: &[String]) -> String {
    serde_json::to_string(xs).unwrap_or_else(|_| "[]".to_owned())
}

/// A non-negative SQLite integer as `u64`.
fn unsigned(n: i64) -> u64 {
    u64::try_from(n).unwrap_or(0)
}

fn parse_list(text: &str) -> Vec<String> {
    serde_json::from_str(text).unwrap_or_default()
}

/// Stores a game under `version`, reusing the title of the same name from any
/// version of its DAT family so ids, `wanted` and file provenance survive a new
/// version. Roms the game no longer lists are marked retired.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::{dats, titles};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// mistarr_server::db::platforms::seed(&mut conn, &mistarr_mister::platforms::PLATFORMS).unwrap();
/// let v = dats::NewVersion { dat_name: "Maker - Game Boy", version: "1", source_file: "a.dat", platform: None, now: 1 };
/// let version = dats::upsert_version(&conn, &v).unwrap().id;
/// let t = titles::TitleInput { name: "Example Quest (USA)", base_name: "Example Quest",
///     group_key: "example quest", clone_of: None, regions: &[], languages: &[], revision: None, flags: &[] };
/// let id = titles::upsert_title(&conn, "gb", version, &t, &[]).unwrap();
/// assert_eq!(titles::upsert_title(&conn, "gb", version, &t, &[]).unwrap(), id);
/// ```
pub fn upsert_title(
    conn: &Connection,
    platform: &str,
    version: DatVersionId,
    t: &TitleInput<'_>,
    roms: &[RomInput<'_>],
) -> Result<TitleId> {
    let existing: Option<i64> = conn
        .prepare_cached(
            "SELECT t.id FROM dat_versions d JOIN titles t ON t.dat_version_id = d.id AND t.name = ?1
             WHERE d.family = (SELECT family FROM dat_versions WHERE id = ?2) AND t.source = 'dat'
             ORDER BY d.id = ?2 DESC, d.loaded_at DESC LIMIT 1",
        )?
        .query_row(params![t.name, version.0], |r| r.get(0))
        .optional()?;
    let (regions, languages, flags) = (json(t.regions), json(t.languages), json(t.flags));
    let id = if let Some(id) = existing {
        conn.prepare_cached(
            "UPDATE titles SET platform_id = ?1, dat_version_id = ?2, base_name = ?3,
               regions = ?4, languages = ?5, revision = ?6, flags = ?7, clone_of = ?8,
               group_key = ?9, retired = 0
             WHERE id = ?10",
        )?
        .execute(params![
            platform,
            version.0,
            t.base_name,
            regions,
            languages,
            t.revision,
            flags,
            t.clone_of,
            t.group_key,
            id
        ])?;
        conn.prepare_cached("UPDATE roms SET retired = 1 WHERE title_id = ?1")?
            .execute([id])?;
        id
    } else {
        conn.prepare_cached(
            "INSERT INTO titles (platform_id, dat_version_id, name, base_name, regions,
               languages, revision, flags, clone_of, group_key)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )?
        .execute(params![
            platform,
            version.0,
            t.name,
            t.base_name,
            regions,
            languages,
            t.revision,
            flags,
            t.clone_of,
            t.group_key
        ])?;
        let id = conn.last_insert_rowid();
        conn.prepare_cached("UPDATE titles SET parent_id = id WHERE id = ?1")?
            .execute([id])?;
        id
    };
    let mut stmt = conn.prepare_cached(
        "INSERT INTO roms (title_id, name, size, crc32, md5, sha1, status, header, retired)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0)
         ON CONFLICT(title_id, name) DO UPDATE SET size = excluded.size, crc32 = excluded.crc32,
           md5 = excluded.md5, sha1 = excluded.sha1, status = excluded.status,
           header = excluded.header, retired = 0",
    )?;
    for r in roms {
        let size = i64::try_from(r.size).unwrap_or(i64::MAX);
        stmt.execute(params![
            id, r.name, size, r.crc32, r.md5, r.sha1, r.status, r.header
        ])?;
    }
    Ok(TitleId(id))
}

/// Sets clone parents for the titles of `version`. With `use_clone_of` each
/// title's parent is the game its `cloneof` names, else itself; without it the
/// titles are marked `inferred` and [`recompute_platform`] groups them.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::{dats::DatVersionId, titles};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// titles::link_parents(&conn, DatVersionId(1), true).unwrap();
/// ```
pub fn link_parents(conn: &Connection, version: DatVersionId, use_clone_of: bool) -> Result<()> {
    if use_clone_of {
        conn.execute(
            "UPDATE titles SET inferred = 0, parent_id = COALESCE(
               (SELECT p.id FROM titles p
                WHERE p.dat_version_id = titles.dat_version_id AND p.name = titles.clone_of),
               id)
             WHERE dat_version_id = ?1",
            [version.0],
        )?;
    } else {
        conn.execute(
            "UPDATE titles SET inferred = 1 WHERE dat_version_id = ?1",
            [version.0],
        )?;
    }
    Ok(())
}

/// What [`recompute_platform`] changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Recomputed {
    /// Clone groups on the platform.
    pub groups: u64,
    /// Groups with a pick.
    pub picks: u64,
}

/// A title as input to selection.
struct Candidate {
    id: i64,
    name: String,
    bad_dump: bool,
}

fn variants(group: &[Candidate]) -> Vec<Variant> {
    let parsed: Vec<_> = group.iter().map(|c| parse_name(&c.name)).collect();
    let mut ranks: Vec<_> = parsed.iter().map(ParsedName::revision_rank).collect();
    ranks.sort_unstable();
    ranks.dedup();
    group
        .iter()
        .zip(parsed)
        .map(|(c, p)| {
            // Dense rank within the group keeps the revision order exactly.
            let rank = ranks.binary_search(&p.revision_rank()).unwrap_or(0);
            Variant {
                id: u64::try_from(c.id).unwrap_or(0),
                name: c.name.clone(),
                regions: p.regions.iter().map(|r| r.name().to_owned()).collect(),
                languages: p.languages.clone(),
                revision_rank: u32::try_from(rank).ok(),
                flags: p.flag_labels(),
                good_dump: !c.bad_dump && !p.flags.contains(&mistarr_core::naming::Flag::BadDump),
            }
        })
        .collect()
}

/// Reads `(group key, candidate)` rows in key order, one group at a time.
fn grouped(
    conn: &Connection,
    sql: &str,
    platform: &str,
    mut each: impl FnMut(&[Candidate]),
) -> Result<()> {
    let mut stmt = conn.prepare(sql)?;
    let mut rows = stmt.query([platform])?;
    let mut key: Option<String> = None;
    let mut group = Vec::new();
    while let Some(r) = rows.next()? {
        let k: String = r.get(0)?;
        if key.as_ref().is_some_and(|prev| *prev != k) {
            each(&group);
            group.clear();
        }
        key = Some(k);
        group.push(Candidate {
            id: r.get(1)?,
            name: r.get(2)?,
            bad_dump: r.get(3)?,
        });
    }
    if !group.is_empty() {
        each(&group);
    }
    Ok(())
}

const BAD_DUMP: &str =
    "EXISTS (SELECT 1 FROM roms r WHERE r.title_id = t.id AND r.retired = 0 AND r.status = 'baddump')";

/// Re-elects inferred clone parents under default preferences, joins the clone groups
/// of titles that different DAT versions list with the same roms, then stores the 1G1R
/// pick of every live clone group on `platform` under `prefs`. Run it inside a transaction.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let prefs = mistarr_core::select::Prefs::default();
/// let r = mistarr_server::db::titles::recompute_platform(&conn, "nes", &prefs).unwrap();
/// assert_eq!(r.groups, 0);
/// ```
pub fn recompute_platform(conn: &Connection, platform: &str, prefs: &Prefs) -> Result<Recomputed> {
    let defaults = Prefs::default();
    let mut parents: Vec<(i64, i64)> = Vec::new();
    grouped(
        conn,
        &format!(
            "SELECT t.group_key, t.id, t.name, {BAD_DUMP} FROM titles t
             WHERE t.platform_id = ?1 AND t.inferred = 1 AND t.retired = 0
             ORDER BY t.group_key, t.id"
        ),
        platform,
        |group| {
            let key = String::new();
            let items = variants(group).into_iter().map(|v| (key.clone(), v));
            if let Some(g) = infer_groups(items, &defaults).into_iter().next() {
                let parent = g.member_ids.first().copied().unwrap_or(0);
                let parent = i64::try_from(parent).unwrap_or(0);
                parents.extend(
                    g.member_ids
                        .iter()
                        .map(|&m| (i64::try_from(m).unwrap_or(0), parent)),
                );
            }
        },
    )?;
    {
        let mut stmt = conn.prepare_cached(
            "UPDATE titles SET parent_id = ?2 WHERE id = ?1 AND parent_id IS NOT ?2",
        )?;
        for (id, parent) in &parents {
            stmt.execute([id, parent])?;
        }
    }
    join_duplicate_groups(conn, platform)?;

    let mut out = Recomputed::default();
    let mut picks: Vec<i64> = Vec::new();
    grouped(
        conn,
        &format!(
            "SELECT CAST(t.parent_id AS TEXT), t.id, t.name, {BAD_DUMP} FROM titles t
             WHERE t.platform_id = ?1 AND t.retired = 0 ORDER BY t.parent_id, t.id"
        ),
        platform,
        |group| {
            out.groups += 1;
            let vs = variants(group);
            if let Some(pick) = select_1g1r(&vs, prefs) {
                picks.push(i64::try_from(pick.id).unwrap_or(0));
            }
        },
    )?;
    conn.execute(
        "UPDATE titles SET is_1g1r_pick = 0 WHERE platform_id = ?1 AND is_1g1r_pick = 1",
        [platform],
    )?;
    let mut stmt = conn.prepare_cached("UPDATE titles SET is_1g1r_pick = 1 WHERE id = ?1")?;
    for id in &picks {
        stmt.execute([id])?;
    }
    out.picks = u64::try_from(picks.len()).unwrap_or(u64::MAX);
    Ok(out)
}

/// Joins clone groups across DAT versions: a title whose live roms, by hash, equal those
/// of a title from another live version moves its whole group under the older group's
/// parent. Groups joined by an earlier run return to their own DAT's parents first. Only
/// the versions other than the largest are held in memory; the largest is streamed.
fn join_duplicate_groups(conn: &Connection, platform: &str) -> Result<()> {
    conn.execute(
        "UPDATE titles SET parent_id = COALESCE(
           (SELECT p.id FROM titles p
            WHERE p.dat_version_id = titles.dat_version_id AND p.name = titles.clone_of),
           id)
         WHERE platform_id = ?1 AND source = 'dat' AND inferred = 0
           AND (SELECT q.dat_version_id FROM titles q WHERE q.id = titles.parent_id)
               IS NOT dat_version_id",
        [platform],
    )?;
    let versions: Vec<i64> = conn
        .prepare(
            "SELECT id FROM dat_versions
             WHERE platform_id = ?1 AND source = 'dat' AND retired = 0 AND superseded_by IS NULL
             ORDER BY game_count DESC, id",
        )?
        .query_map([platform], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let Some((&largest, rest)) = versions.split_first() else {
        return Ok(());
    };
    if rest.is_empty() {
        return Ok(());
    }
    let mut first: HashMap<u64, (i64, i64)> = HashMap::new();
    let mut root: HashMap<i64, i64> = HashMap::new();
    for &version in rest {
        each_signature(conn, version, |parent, sig| {
            let (seen, seen_parent) = *first.entry(sig).or_insert((version, parent));
            if seen != version {
                union(&mut root, parent, seen_parent);
            }
        })?;
    }
    each_signature(conn, largest, |parent, sig| {
        if let Some(&(_, seen_parent)) = first.get(&sig) {
            union(&mut root, parent, seen_parent);
        }
    })?;
    let mut update =
        conn.prepare_cached("UPDATE titles SET parent_id = ?2 WHERE parent_id = ?1")?;
    let groups: Vec<i64> = root.keys().copied().collect();
    for group in groups {
        update.execute([group, find(&root, group)])?;
    }
    Ok(())
}

/// Calls `each(parent, signature)` for every live title of `version` with roms that all
/// carry a hash; the signature hashes the rom keys whatever their order.
fn each_signature(conn: &Connection, version: i64, mut each: impl FnMut(i64, u64)) -> Result<()> {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut stmt = conn.prepare(
        "SELECT t.id, t.parent_id, COALESCE(r.sha1, r.md5, r.crc32 || ':' || r.size)
         FROM titles t JOIN roms r ON r.title_id = t.id AND r.retired = 0
         WHERE t.dat_version_id = ?1 AND t.retired = 0 AND t.source = 'dat'
         ORDER BY t.id",
    )?;
    let mut rows = stmt.query([version])?;
    // Per title: id, parent, sum of key hashes, rom count, whether every rom had a key.
    let mut open: Option<(i64, i64, u64, u64, bool)> = None;
    let mut settle = |t: Option<(i64, i64, u64, u64, bool)>| {
        if let Some((_, parent, sum, n, true)) = t {
            let mut h = DefaultHasher::new();
            (sum, n).hash(&mut h);
            each(parent, h.finish());
        }
    };
    while let Some(r) = rows.next()? {
        let (id, parent): (i64, i64) = (r.get(0)?, r.get(1)?);
        let key: Option<String> = r.get(2)?;
        if open.is_none_or(|o| o.0 != id) {
            settle(open.take());
            open = Some((id, parent, 0, 0, true));
        }
        if let Some(o) = open.as_mut() {
            match key {
                Some(k) => {
                    let mut h = DefaultHasher::new();
                    k.hash(&mut h);
                    o.2 = o.2.wrapping_add(h.finish());
                    o.3 += 1;
                }
                None => o.4 = false,
            }
        }
    }
    settle(open.take());
    Ok(())
}

/// Joins the trees of `a` and `b` under the smaller root.
fn union(root: &mut HashMap<i64, i64>, a: i64, b: i64) {
    let (a, b) = (find(root, a), find(root, b));
    if a != b {
        root.insert(a.max(b), a.min(b));
    }
}

/// The root of `id` in a union-find forest stored as child to parent.
fn find(root: &HashMap<i64, i64>, mut id: i64) -> i64 {
    while let Some(&up) = root.get(&id) {
        id = up;
    }
    id
}

/// Keeps a group of `title_groups g` only when it is an MRA title or its platform has no
/// live MRA title, so a platform with MRAs browses its MRA catalogue alone.
const MRA_ONLY: &str =
    "(EXISTS (SELECT 1 FROM titles s WHERE s.id = g.parent_id AND s.source = 'mra')
    OR NOT EXISTS (SELECT 1 FROM titles m WHERE m.platform_id = g.platform_id
                   AND m.source = 'mra' AND m.retired = 0))";

/// Catalog counts of one platform for `GET /platforms`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct Counts {
    /// Clone groups with a visible live variant.
    pub titles: u64,
    /// Groups with at least one fully verified variant.
    pub have: u64,
    /// Groups with at least one wanted variant.
    pub wanted: u64,
    /// Files on disk that match no rom.
    pub unverified: u64,
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
/// assert!(mistarr_server::db::titles::counts(&conn, &[]).unwrap().is_empty());
/// ```
pub fn counts(conn: &Connection, hidden: &[String]) -> Result<HashMap<String, Counts>> {
    let mut out: HashMap<String, Counts> = HashMap::new();
    let mut stmt = conn.prepare(&format!(
        "SELECT g.platform_id, COUNT(*), SUM(g.have_verified > 0), SUM(g.wanted > 0)
         FROM title_groups g
         WHERE {MRA_ONLY} AND EXISTS (
           SELECT 1 FROM titles v WHERE v.parent_id = g.parent_id AND v.retired = 0
             AND NOT EXISTS (SELECT 1 FROM json_each(v.flags) f
                             WHERE f.value IN (SELECT value FROM json_each(?1))))
         GROUP BY g.platform_id"
    ))?;
    let mut rows = stmt.query([json(hidden)])?;
    while let Some(r) = rows.next()? {
        let e = out.entry(r.get(0)?).or_default();
        e.titles = unsigned(r.get(1)?);
        e.have = unsigned(r.get(2)?);
        e.wanted = unsigned(r.get(3)?);
    }
    let mut stmt = conn.prepare(
        "SELECT platform_id, COUNT(*) FROM files WHERE state = 'unverified' GROUP BY platform_id",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        out.entry(r.get(0)?).or_default().unverified = unsigned(r.get(1)?);
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

impl Tri {
    fn as_str(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::Yes => "yes",
            Self::No => "no",
        }
    }
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
}

const BROWSE_WHERE: &str = "
    g.platform_id = ?1
    AND (?2 IS NULL OR g.base_name LIKE ?2 ESCAPE '\\')
    AND (?3 = 'any' OR (?3 = 'yes') = (g.have_verified > 0))
    AND (?4 = 'any' OR (?4 = 'yes') = (g.wanted > 0))
    AND EXISTS (
      SELECT 1 FROM titles v WHERE v.parent_id = g.parent_id AND v.retired = 0
        AND NOT EXISTS (SELECT 1 FROM json_each(v.flags) f
                        WHERE f.value IN (SELECT value FROM json_each(?5)))
        AND (?6 IS NULL OR EXISTS (SELECT 1 FROM json_each(v.regions) r
                                   WHERE r.value = ?6 COLLATE NOCASE))
        AND NOT EXISTS (SELECT 1 FROM json_each(?7) w
                        WHERE NOT EXISTS (SELECT 1 FROM json_each(v.flags) f
                                          WHERE f.value = w.value)))";

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
/// use mistarr_server::db::titles::{browse, Browse};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let (rows, total) = browse(&conn, "nes", &Browse::default(), 10, 0).unwrap();
/// assert!(rows.is_empty() && total == 0);
/// ```
pub fn browse(
    conn: &Connection,
    platform: &str,
    filter: &Browse,
    limit: u32,
    offset: u32,
) -> Result<(Vec<GroupRow>, u64)> {
    let q = filter
        .q
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .map(like_pattern);
    let hidden = json(&filter.hidden);
    let flags = json(&filter.flags);
    let args = params![
        platform,
        q,
        filter.have.as_str(),
        filter.wanted.as_str(),
        hidden,
        filter.region,
        flags,
        limit,
        offset
    ];
    let total: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM title_groups g WHERE {BROWSE_WHERE} AND {MRA_ONLY}"),
        &args[..7],
        |r| r.get(0),
    )?;
    let order = match filter.sort {
        Sort::Name => "g.base_name COLLATE NOCASE, g.parent_id",
        Sort::Have => "g.have_verified > 0 DESC, g.base_name COLLATE NOCASE, g.parent_id",
        Sort::Recent => "g.newest_id DESC",
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT g.parent_id, g.platform_id, g.base_name, g.name, g.pick_id, k.name,
                g.variants, g.have_verified, g.wanted, g.has_pick
         FROM title_groups g LEFT JOIN titles k ON k.id = g.pick_id
         WHERE {BROWSE_WHERE} AND {MRA_ONLY} ORDER BY {order} LIMIT ?8 OFFSET ?9"
    ))?;
    let rows = stmt
        .query_map(args, |r| {
            Ok(GroupRow {
                parent_id: TitleId(r.get(0)?),
                platform_id: PlatformId(r.get(1)?),
                base_name: r.get(2)?,
                name: r.get(3)?,
                pick_id: r.get::<_, Option<i64>>(4)?.map(TitleId),
                pick_name: r.get(5)?,
                variants: unsigned(r.get(6)?),
                have_verified: unsigned(r.get(7)?),
                wanted: unsigned(r.get(8)?),
                has_pick: r.get(9)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok((rows, unsigned(total)))
}

/// The clone-group root of a title, or `None` when there is no such title.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::titles::{group_of, TitleId};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert!(group_of(&conn, TitleId(1)).unwrap().is_none());
/// ```
pub fn group_of(conn: &Connection, id: TitleId) -> Result<Option<TitleId>> {
    Ok(conn
        .query_row(
            "SELECT COALESCE(parent_id, id) FROM titles WHERE id = ?1",
            [id.0],
            |r| r.get(0).map(TitleId),
        )
        .optional()?)
}

/// A rom of a variant with the best file on disk for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RomRow {
    /// `roms.id`.
    pub id: i64,
    /// File name in the DAT.
    pub name: String,
    /// Size in bytes.
    pub size: u64,
    /// Lowercase hex CRC32.
    pub crc32: Option<String>,
    /// Lowercase hex MD5.
    pub md5: Option<String>,
    /// Lowercase hex SHA1.
    pub sha1: Option<String>,
    /// DAT status.
    pub status: String,
    /// The file matched to this rom, verified first.
    pub file_id: Option<i64>,
    /// That file's state.
    pub file_state: Option<String>,
    /// That file's path relative to the platform's games directory.
    pub file_path: Option<String>,
}

/// One variant of a clone group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[allow(clippy::struct_excessive_bools)] // One flag per title column is the JSON shape.
pub struct VariantRow {
    /// `titles.id`.
    pub id: TitleId,
    /// Full DAT name.
    pub name: String,
    /// Region names.
    pub regions: Vec<String>,
    /// Language codes.
    pub languages: Vec<String>,
    /// Revision label.
    pub revision: Option<String>,
    /// Flag labels.
    pub flags: Vec<String>,
    /// The group's 1G1R pick.
    pub is_1g1r_pick: bool,
    /// Marked wanted.
    pub wanted: bool,
    /// No longer in the newest DAT version.
    pub retired: bool,
    /// Grouped by name rather than `cloneof`.
    pub inferred: bool,
    /// The DAT version the entry was last read from.
    pub dat_version_id: DatVersionId,
    /// Live roms.
    pub roms: Vec<RomRow>,
    /// Torrent files from bound sources matched to any of the roms.
    pub torrent_files_available: u64,
    /// `dat` for a DAT entry, `mra` for an arcade title read from an MRA file.
    pub source: String,
    /// MRA details, for an MRA title.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mra: Option<MraInfo>,
    /// Where the romset stands on disk, for a Neo Geo entry; filled by the HTTP layer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub romset: Option<RomsetState>,
}

/// A Neo Geo entry's romset on disk and in the core's `romsets.xml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RomsetState {
    /// Whether `romsets.xml` lists it; `None` when the file is absent.
    pub listed: Option<bool>,
    /// Whether `games/NeoGeo` holds it as a directory or zip.
    pub present: bool,
}

/// A clone group with every variant, retired ones last.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GroupDetail {
    /// Group root.
    pub parent_id: TitleId,
    /// Platform.
    pub platform_id: PlatformId,
    /// The parent's base name.
    pub base_name: String,
    /// The 1G1R pick, if any.
    pub pick_variant_id: Option<TitleId>,
    /// Every variant.
    pub variants: Vec<VariantRow>,
}

fn variant_row(r: &Row<'_>) -> rusqlite::Result<VariantRow> {
    Ok(VariantRow {
        id: TitleId(r.get(0)?),
        name: r.get(1)?,
        regions: parse_list(&r.get::<_, String>(2)?),
        languages: parse_list(&r.get::<_, String>(3)?),
        revision: r.get(4)?,
        flags: parse_list(&r.get::<_, String>(5)?),
        is_1g1r_pick: r.get(6)?,
        wanted: r.get(7)?,
        retired: r.get(8)?,
        inferred: r.get(9)?,
        dat_version_id: DatVersionId(r.get(10)?),
        roms: Vec::new(),
        torrent_files_available: unsigned(r.get(11)?),
        source: r.get(12)?,
        mra: None,
        romset: None,
    })
}

/// The clone group containing title `id`, or `None` when there is no such title.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::titles::{group_detail, TitleId};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert!(group_detail(&conn, TitleId(1)).unwrap().is_none());
/// ```
pub fn group_detail(conn: &Connection, id: TitleId) -> Result<Option<GroupDetail>> {
    let Some(gid) = group_of(conn, id)? else {
        return Ok(None);
    };
    let Some((platform, base_name)): Option<(String, String)> = conn
        .query_row(
            "SELECT platform_id, base_name FROM titles WHERE id = ?1",
            [gid.0],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
    else {
        return Ok(None);
    };
    let mut stmt = conn.prepare(
        "SELECT t.id, t.name, t.regions, t.languages, t.revision, t.flags, t.is_1g1r_pick,
                t.wanted, t.retired, t.inferred, t.dat_version_id,
                (SELECT COUNT(*) FROM torrent_files tf JOIN roms r ON r.id = tf.rom_id
                 WHERE r.title_id = t.id AND r.retired = 0),
                t.source
         FROM titles t WHERE t.parent_id = ?1 OR t.id = ?1
         ORDER BY t.retired, t.is_1g1r_pick DESC, t.name",
    )?;
    let mut variants: Vec<VariantRow> = stmt
        .query_map([gid.0], variant_row)?
        .collect::<rusqlite::Result<_>>()?;
    let mut stmt = conn.prepare(
        "SELECT r.title_id, r.id, r.name, r.size, r.crc32, r.md5, r.sha1, r.status,
                f.id, f.state, f.rel_path
         FROM titles t JOIN roms r ON r.title_id = t.id AND r.retired = 0
         LEFT JOIN files f ON f.id = (
           SELECT x.id FROM files x WHERE x.rom_id = r.id
           ORDER BY CASE x.state WHEN 'verified' THEN 0 WHEN 'misnamed' THEN 1
                                 WHEN 'bad' THEN 2 ELSE 3 END, x.id
           LIMIT 1)
         WHERE t.parent_id = ?1 OR t.id = ?1
         ORDER BY r.title_id, r.name",
    )?;
    let mut rows = stmt.query([gid.0])?;
    while let Some(r) = rows.next()? {
        let title: i64 = r.get(0)?;
        let rom = RomRow {
            id: r.get(1)?,
            name: r.get(2)?,
            size: unsigned(r.get(3)?),
            crc32: r.get(4)?,
            md5: r.get(5)?,
            sha1: r.get(6)?,
            status: r.get(7)?,
            file_id: r.get(8)?,
            file_state: r.get(9)?,
            file_path: r.get(10)?,
        };
        if let Some(v) = variants.iter_mut().find(|v| v.id.0 == title) {
            v.roms.push(rom);
        }
    }
    for v in variants.iter_mut().filter(|v| v.source == "mra") {
        v.mra = super::arcade::info(conn, v.id)?;
    }
    let pick_variant_id = variants.iter().find(|v| v.is_1g1r_pick).map(|v| v.id);
    Ok(Some(GroupDetail {
        parent_id: gid,
        platform_id: PlatformId(platform),
        base_name,
        pick_variant_id,
        variants,
    }))
}

/// Why a variant could not be marked wanted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WantRefused {
    /// No such title.
    Missing,
    /// The entry is retired.
    Retired,
    /// BIOS entries are never selectable.
    Bios,
}

/// Marks one variant wanted.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::titles::{want, TitleId, WantRefused};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert_eq!(want(&conn, TitleId(1)).unwrap(), Err(WantRefused::Missing));
/// ```
pub fn want(conn: &Connection, id: TitleId) -> Result<std::result::Result<(), WantRefused>> {
    let row: Option<(bool, bool)> = conn
        .query_row(
            "SELECT retired, EXISTS (SELECT 1 FROM json_each(flags) WHERE value = 'bios')
             FROM titles WHERE id = ?1",
            [id.0],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match row {
        None => Ok(Err(WantRefused::Missing)),
        Some((true, _)) => Ok(Err(WantRefused::Retired)),
        Some((_, true)) => Ok(Err(WantRefused::Bios)),
        Some(_) => {
            conn.execute("UPDATE titles SET wanted = 1 WHERE id = ?1", [id.0])?;
            Ok(Ok(()))
        }
    }
}

/// Unmarks every variant of the group rooted at `parent` and cancels its
/// downloads that have not started. Returns how many variants were wanted.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::titles::{unwant_group, TitleId};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert_eq!(unwant_group(&conn, TitleId(1), 0).unwrap(), 0);
/// ```
pub fn unwant_group(conn: &Connection, parent: TitleId, now: i64) -> Result<usize> {
    let n = conn.execute(
        "UPDATE titles SET wanted = 0 WHERE (parent_id = ?1 OR id = ?1) AND wanted = 1",
        [parent.0],
    )?;
    conn.execute(
        "UPDATE downloads SET state = 'cancelled', updated_at = ?2
         WHERE state IN ('wanted', 'queued')
           AND title_id IN (SELECT id FROM titles WHERE parent_id = ?1 OR id = ?1)",
        params![parent.0, now],
    )?;
    Ok(n)
}

#[cfg(test)]
mod tests;
