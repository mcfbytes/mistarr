//! The `titles` and `roms` tables, clone groups, 1G1R picks and the browse
//! query over `title_groups`; see `docs/DATA-MODEL.md` and `docs/VERIFICATION.md`.

use std::collections::HashMap;
use std::fmt;

use mistarr_core::naming::{parse_name, ParsedName};
use mistarr_core::select::{infer_groups, select_1g1r, Prefs, Variant};
use mistarr_core::PlatformId;
use rusqlite::types::Value;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use super::arcade::MraInfo;
use super::candidates::Availability;
use super::dats::DatVersionId;
use super::groups::{self, Clause};
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

/// A non-negative SQLite integer as `u64`.
fn unsigned(n: i64) -> u64 {
    u64::try_from(n).unwrap_or(0)
}

/// A title's list stored in its own table, one row per value in DAT order.
#[derive(Debug, Clone, Copy)]
enum Tag {
    Flags,
    Regions,
    Languages,
}

impl Tag {
    /// The table and its value column.
    fn table(self) -> (&'static str, &'static str) {
        match self {
            Self::Flags => ("title_flags", "flag"),
            Self::Regions => ("title_regions", "region"),
            Self::Languages => ("title_languages", "language"),
        }
    }
}

/// Title `id`'s `tag` values in order.
fn tags_of(conn: &Connection, id: i64, tag: Tag) -> Result<Vec<String>> {
    let (table, column) = tag.table();
    let rows = conn
        .prepare_cached(&format!(
            "SELECT {column} FROM {table} WHERE title_id = ?1 ORDER BY pos"
        ))?
        .query_map([id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// Replaces title `id`'s `tag` values with `values`, keeping the first of repeats, and
/// writes nothing when they are unchanged so the title's group stays clean.
fn store_tags(conn: &Connection, id: i64, tag: Tag, values: &[String]) -> Result<()> {
    let mut wanted: Vec<&str> = Vec::with_capacity(values.len());
    for v in values {
        if !wanted.contains(&v.as_str()) {
            wanted.push(v);
        }
    }
    if tags_of(conn, id, tag)?
        .iter()
        .map(String::as_str)
        .eq(wanted.iter().copied())
    {
        return Ok(());
    }
    let (table, column) = tag.table();
    conn.prepare_cached(&format!("DELETE FROM {table} WHERE title_id = ?1"))?
        .execute([id])?;
    let mut insert = conn.prepare_cached(&format!(
        "INSERT INTO {table} (title_id, pos, {column}) VALUES (?1, ?2, ?3)"
    ))?;
    for (pos, v) in wanted.iter().enumerate() {
        insert.execute(params![id, i64::try_from(pos).unwrap_or(i64::MAX), v])?;
    }
    Ok(())
}

/// Stores a title's regions, languages and flags in their tables.
pub(crate) fn store_lists(
    conn: &Connection,
    id: i64,
    regions: &[String],
    languages: &[String],
    flags: &[String],
) -> Result<()> {
    store_tags(conn, id, Tag::Regions, regions)?;
    store_tags(conn, id, Tag::Languages, languages)?;
    store_tags(conn, id, Tag::Flags, flags)
}

/// Replaces the flags of title `id`, in order.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure, including a title that does not exist.
///
/// ```
/// use mistarr_server::db::titles::{set_flags, TitleId};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert!(set_flags(&conn, TitleId(1), &[]).is_ok());
/// ```
pub fn set_flags(conn: &Connection, id: TitleId, flags: &[String]) -> Result<()> {
    store_tags(conn, id.0, Tag::Flags, flags)
}

/// The flags of title `id`, in order.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::titles::{flags_of, TitleId};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// assert!(flags_of(&conn, TitleId(1)).unwrap().is_empty());
/// ```
pub fn flags_of(conn: &Connection, id: TitleId) -> Result<Vec<String>> {
    tags_of(conn, id.0, Tag::Flags)
}

/// Stores a game under `version`, reusing the title of the same name from any version
/// of its DAT family on the same platform, so ids, `wanted` and file provenance survive
/// a new version. Roms the game no longer lists are marked retired.
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
            // Unary plus keeps the lookup on (dat_version_id, name), not a platform-wide index.
            "SELECT t.id FROM dat_versions d JOIN titles t ON t.dat_version_id = d.id AND t.name = ?1
             WHERE d.family = (SELECT family FROM dat_versions WHERE id = ?2)
               AND +t.source = 'dat' AND +t.platform_id = ?3
             ORDER BY d.id = ?2 DESC, d.loaded_at DESC LIMIT 1",
        )?
        .query_row(params![t.name, version.0, platform], |r| r.get(0))
        .optional()?;
    let id = if let Some(id) = existing {
        conn.prepare_cached(
            "UPDATE titles SET platform_id = ?1, dat_version_id = ?2, base_name = ?3,
               revision = ?4, clone_of = ?5, group_key = ?6, retired = 0
             WHERE id = ?7",
        )?
        .execute(params![
            platform,
            version.0,
            t.base_name,
            t.revision,
            t.clone_of,
            t.group_key,
            id
        ])?;
        conn.prepare_cached("UPDATE roms SET retired = 1 WHERE title_id = ?1")?
            .execute([id])?;
        id
    } else {
        conn.prepare_cached(
            "INSERT INTO titles (platform_id, dat_version_id, name, base_name, revision,
               clone_of, group_key)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?
        .execute(params![
            platform,
            version.0,
            t.name,
            t.base_name,
            t.revision,
            t.clone_of,
            t.group_key
        ])?;
        let id = conn.last_insert_rowid();
        conn.prepare_cached("UPDATE titles SET parent_id = id WHERE id = ?1")?
            .execute([id])?;
        id
    };
    store_lists(conn, id, t.regions, t.languages, t.flags)?;
    let arcade = mistarr_mister::platforms::by_id(platform)
        .is_some_and(mistarr_mister::platforms::Platform::is_arcade);
    if existing.is_some() && !arcade {
        for r in roms {
            let size = i64::try_from(r.size).unwrap_or(i64::MAX);
            let listed = [r.crc32, r.md5, r.sha1];
            super::files::unmatch_changed_rom(conn, id, r.name, size, listed)?;
        }
    }
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

/// Re-elects inferred clone parents under default preferences, recomputes each title's
/// effective group (`group_root`) so titles that different DAT versions list with the same
/// roms share one, then stores the 1G1R pick of every live effective group on `platform`
/// under `prefs`. Run it inside a transaction.
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
    link_shared_titles(conn, platform)?;

    let mut out = Recomputed::default();
    let mut picks: Vec<i64> = Vec::new();
    grouped(
        conn,
        &format!(
            "SELECT CAST(t.group_root AS TEXT), t.id, t.name, {BAD_DUMP} FROM titles t
             WHERE t.platform_id = ?1 AND t.retired = 0 ORDER BY t.group_root, t.id"
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
    out.picks = u64::try_from(picks.len()).unwrap_or(u64::MAX);
    store_picks(conn, platform, picks)?;
    Ok(out)
}

/// Sets `is_1g1r_pick` on exactly `picks` among `platform`'s titles, writing only the
/// rows that change so unchanged groups stay clean.
fn store_picks(conn: &Connection, platform: &str, mut picks: Vec<i64>) -> Result<()> {
    picks.sort_unstable();
    picks.dedup();
    let current: Vec<i64> = conn
        .prepare_cached(
            "SELECT id FROM titles WHERE platform_id = ?1 AND is_1g1r_pick = 1 ORDER BY id",
        )?
        .query_map([platform], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut set = conn.prepare_cached("UPDATE titles SET is_1g1r_pick = ?2 WHERE id = ?1")?;
    for &id in current.iter().filter(|id| picks.binary_search(id).is_err()) {
        set.execute([id, 0])?;
    }
    for &id in picks.iter().filter(|id| current.binary_search(id).is_err()) {
        set.execute([id, 1])?;
    }
    Ok(())
}

/// Recomputes `group_root` on `platform` from scratch. Every title starts in its own
/// DAT's group (`parent_id`). A title of a live version whose live roms equal, by hash,
/// those of a title in the largest live version then links to that title's group; one
/// shared only among the other versions links to the group of its earliest version. Only
/// a single title ever links, never a group, so two groups of one DAT never merge; the
/// members left behind by a root that linked away take their lowest live id as root.
/// The groups are worked out in memory and only rows whose `group_root` changes are
/// written, so a recompute that changes nothing leaves every group clean.
fn link_shared_titles(conn: &Connection, platform: &str) -> Result<()> {
    let versions: Vec<i64> = conn
        .prepare_cached(
            "SELECT id FROM dat_versions
             WHERE platform_id = ?1 AND source = 'dat' AND retired = 0 AND superseded_by IS NULL
             ORDER BY game_count DESC, id",
        )?
        .query_map([platform], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let (largest, mut rest) = match versions.split_first() {
        Some((&largest, rest)) if !rest.is_empty() => (largest, rest.to_vec()),
        _ => {
            conn.execute(
                "UPDATE titles SET group_root = parent_id
                 WHERE platform_id = ?1 AND group_root IS NOT parent_id",
                [platform],
            )?;
            return Ok(());
        }
    };
    rest.sort_unstable();
    let links = shared_links(conn, platform, largest, &rest)?;
    let mut nodes = group_nodes(conn, platform)?;
    for (id, root) in links {
        if let Ok(i) = nodes.binary_search_by_key(&id, |n| n.id) {
            nodes[i].target = Some(root);
        }
    }
    reroot(&mut nodes);
    let mut set = conn.prepare_cached("UPDATE titles SET group_root = ?2 WHERE id = ?1")?;
    for n in nodes.iter().filter(|n| n.target != n.current) {
        set.execute(params![n.id, n.target])?;
    }
    Ok(())
}

/// The `(title, group)` links of [`link_shared_titles`]: titles of `rest` whose roms
/// equal those of a title in `largest`, or of a title in an earlier version of `rest`.
fn shared_links(
    conn: &Connection,
    platform: &str,
    largest: i64,
    rest: &[i64],
) -> Result<Vec<(i64, i64)>> {
    // Titles outside the largest version by signature, in version order: (version, id, parent).
    let mut others: HashMap<u64, Vec<(i64, i64, i64)>> = HashMap::new();
    for &version in rest {
        each_signature(conn, platform, version, |id, parent, sig| {
            others.entry(sig).or_default().push((version, id, parent));
        })?;
    }
    let mut anchors: HashMap<u64, i64> = HashMap::new();
    each_signature(conn, platform, largest, |_, parent, sig| {
        if others.contains_key(&sig) {
            anchors.entry(sig).or_insert(parent);
        }
    })?;
    let mut links = Vec::new();
    for (sig, titles) in &others {
        let (first_version, root) = match anchors.get(sig) {
            Some(&root) => (largest, root),
            None => (titles[0].0, titles[0].2),
        };
        links.extend(
            titles
                .iter()
                .filter(|t| t.0 != first_version)
                .map(|t| (t.1, root)),
        );
    }
    Ok(links)
}

/// A title as [`link_shared_titles`] regroups it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Node {
    id: i64,
    /// The group it is to belong to.
    target: Option<i64>,
    /// Its stored `group_root`.
    current: Option<i64>,
    live: bool,
}

/// Every title of `platform` with `target` at its `parent_id`, and every title elsewhere
/// in a group a title of `platform` roots with `target` at that group, sorted by id.
fn group_nodes(conn: &Connection, platform: &str) -> Result<Vec<Node>> {
    let mut nodes: Vec<Node> = conn
        .prepare_cached(
            "SELECT id, parent_id, group_root, retired = 0 FROM titles WHERE platform_id = ?1",
        )?
        .query_map([platform], |r| {
            Ok(Node {
                id: r.get(0)?,
                target: r.get(1)?,
                current: r.get(2)?,
                live: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut away = conn.prepare_cached(
        "SELECT m.id, m.group_root, m.retired = 0
         FROM titles r JOIN titles m ON m.group_root = r.id
         WHERE r.platform_id = ?1 AND m.platform_id IS NOT ?1",
    )?;
    let away = away.query_map([platform], |r| {
        let root: Option<i64> = r.get(1)?;
        Ok(Node {
            id: r.get(0)?,
            target: root,
            current: root,
            live: r.get(2)?,
        })
    })?;
    for n in away {
        nodes.push(n?);
    }
    nodes.sort_unstable_by_key(|n| n.id);
    Ok(nodes)
}

/// Gives every group whose root title is among `nodes` but belongs to another group a
/// new root: its lowest live member, or its lowest member when none is live. Every move
/// is worked out before any is applied, so no group gains or loses members whatever
/// order they come in. `nodes` must be sorted by id.
fn reroot(nodes: &mut [Node]) {
    let stranded = |label: i64| {
        nodes
            .binary_search_by_key(&label, |n| n.id)
            .is_ok_and(|i| nodes[i].target != Some(label))
    };
    // Stranded group -> (lowest live member, lowest member); ids come in ascending order.
    let mut roots: HashMap<i64, (Option<i64>, i64)> = HashMap::new();
    for n in nodes.iter() {
        let Some(label) = n.target.filter(|&l| l != n.id) else {
            continue;
        };
        if let Some(e) = roots.get_mut(&label) {
            if e.0.is_none() && n.live {
                e.0 = Some(n.id);
            }
        } else if stranded(label) {
            roots.insert(label, (n.live.then_some(n.id), n.id));
        }
    }
    for n in nodes.iter_mut() {
        if let Some(&(live, lowest)) = n.target.and_then(|t| roots.get(&t)) {
            n.target = Some(live.unwrap_or(lowest));
        }
    }
}

/// Calls `each(id, parent, signature)` for every live title of `version` on `platform`
/// with roms that all carry a hash; the signature hashes the rom keys whatever their order.
fn each_signature(
    conn: &Connection,
    platform: &str,
    version: i64,
    mut each: impl FnMut(i64, i64, u64),
) -> Result<()> {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut stmt = conn.prepare_cached(
        "SELECT t.id, t.parent_id, COALESCE(r.sha1, r.md5, r.crc32 || ':' || r.size)
         FROM titles t JOIN roms r ON r.title_id = t.id AND r.retired = 0
         WHERE t.dat_version_id = ?1 AND t.retired = 0 AND t.source = 'dat'
           AND t.platform_id = ?2
         ORDER BY t.id",
    )?;
    let mut rows = stmt.query(params![version, platform])?;
    // Per title: id, parent, sum of key hashes, rom count, whether every rom had a key.
    let mut open: Option<(i64, i64, u64, u64, bool)> = None;
    let mut settle = |t: Option<(i64, i64, u64, u64, bool)>| {
        if let Some((id, parent, sum, n, true)) = t {
            let mut h = DefaultHasher::new();
            (sum, n).hash(&mut h);
            each(id, parent, h.finish());
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
/// assert!(mistarr_server::db::titles::counts(&conn, &[]).unwrap().is_empty());
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
        e.titles = unsigned(r.get(1)?);
        e.have = unsigned(r.get(2)?);
        e.wanted = unsigned(r.get(3)?);
    }
    let mut stmt = conn.prepare(
        "SELECT platform_id, COUNT(*) FROM files
         WHERE state = 'unverified' AND platform_id <> 'arcade' GROUP BY platform_id",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        out.entry(r.get(0)?).or_default().unmatched_files = unsigned(r.get(1)?);
    }
    let mut stmt = conn.prepare(
        "SELECT platform_id, COUNT(*) FROM files WHERE state = 'unidentified' GROUP BY platform_id",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        out.entry(r.get(0)?).or_default().unidentified_files = unsigned(r.get(1)?);
    }
    // Per visible MRA title, failing its md5 check or partly present, by clone group;
    // a group with a have-verified variant in title_groups counts as neither.
    let mut stmt = conn.prepare(&format!(
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
                             WHERE f.title_id = t.id AND f.flag IN ({}))
           GROUP BY t.platform_id, t.group_root
         )
         SELECT g.platform_id,
                COALESCE(SUM(m.any_failing AND g.have_verified = 0), 0),
                COALESCE(SUM(m.any_partial AND g.have_verified = 0), 0)
         FROM mra m JOIN title_groups g ON g.platform_id = m.platform_id AND g.parent_id = m.parent_id
         GROUP BY g.platform_id",
        groups::placeholders(hidden.len())
    ))?;
    let mut rows = stmt.query(params_from_iter(hidden))?;
    while let Some(r) = rows.next()? {
        let e = out.entry(r.get(0)?).or_default();
        e.failing_check = unsigned(r.get(1)?);
        e.partial = unsigned(r.get(2)?);
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
    /// use mistarr_server::db::titles::SearchShape;
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
    /// use mistarr_server::db::titles::SearchShape;
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
fn browse_clause(
    conn: &Connection,
    platform: &str,
    filter: &Browse,
    shape: SearchShape,
) -> Result<Clause> {
    let mut clause = Clause::default();
    clause.and("g.platform_id = ?", [Value::Text(platform.to_owned())]);
    let mra: bool = conn
        .prepare_cached(
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
                            "platform : \"\u{1f}{platform}\u{1f}\" AND base_name : {}",
                            phrase(q)
                        )),
                        Value::Text(platform.to_owned()),
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
fn page_sql(clause: &Clause, sort: Sort) -> String {
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
    browse_with(conn, platform, filter, limit, offset, SEARCH_SHAPE)
}

/// [`browse`] with the search done by `shape`, for comparing shapes.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::titles::{browse_with, Browse, SearchShape};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// let filter = Browse { q: Some("quest".into()), ..Browse::default() };
/// let (rows, total) = browse_with(&conn, "nes", &filter, 10, 0, SearchShape::Like).unwrap();
/// assert!(rows.is_empty() && total == 0);
/// ```
pub fn browse_with(
    conn: &Connection,
    platform: &str,
    filter: &Browse,
    limit: u32,
    offset: u32,
    shape: SearchShape,
) -> Result<(Vec<GroupRow>, u64)> {
    let clause = browse_clause(conn, platform, filter, shape)?;
    let total: i64 = conn
        .prepare_cached(&format!(
            "SELECT COUNT(*) FROM title_groups g WHERE {}",
            clause.sql()
        ))?
        .query_row(params_from_iter(&clause.args), |r| r.get(0))?;
    let mut stmt = conn.prepare_cached(&page_sql(&clause, filter.sort))?;
    let args = clause
        .args
        .iter()
        .cloned()
        .chain([Value::from(limit), Value::from(offset)]);
    let rows = stmt
        .query_map(params_from_iter(args), |r| {
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
                bios: r.get(10)?,
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
            "SELECT COALESCE(group_root, parent_id, id) FROM titles WHERE id = ?1",
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
    /// Distinct files of bound sources in [`VariantRow::availability`].
    pub torrent_files_available: u64,
    /// Files of bound sources mapped to a live rom or a candidate for one, strongest first.
    pub availability: Vec<Availability>,
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
        regions: Vec::new(),
        languages: Vec::new(),
        revision: r.get(2)?,
        flags: Vec::new(),
        is_1g1r_pick: r.get(3)?,
        wanted: r.get(4)?,
        retired: r.get(5)?,
        inferred: r.get(6)?,
        dat_version_id: DatVersionId(r.get(7)?),
        roms: Vec::new(),
        torrent_files_available: 0,
        availability: Vec::new(),
        source: r.get(8)?,
        mra: None,
        romset: None,
    })
}

/// Fills the regions, languages and flags of the variants of the group rooted at `gid`.
fn fill_lists(conn: &Connection, gid: TitleId, variants: &mut [VariantRow]) -> Result<()> {
    for tag in [Tag::Regions, Tag::Languages, Tag::Flags] {
        let (table, column) = tag.table();
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT x.title_id, x.{column} FROM titles t JOIN {table} x ON x.title_id = t.id
             WHERE t.group_root = ?1 OR (t.id = ?1 AND t.group_root IS NULL)
             ORDER BY x.title_id, x.pos"
        ))?;
        let mut rows = stmt.query([gid.0])?;
        while let Some(r) = rows.next()? {
            let (title, value): (i64, String) = (r.get(0)?, r.get(1)?);
            if let Some(v) = variants.iter_mut().find(|v| v.id.0 == title) {
                match tag {
                    Tag::Regions => v.regions.push(value),
                    Tag::Languages => v.languages.push(value),
                    Tag::Flags => v.flags.push(value),
                }
            }
        }
    }
    Ok(())
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
        "SELECT t.id, t.name, t.revision, t.is_1g1r_pick,
                t.wanted, t.retired, t.inferred, t.dat_version_id, t.source
         FROM titles t WHERE t.group_root = ?1 OR (t.id = ?1 AND t.group_root IS NULL)
         ORDER BY t.retired, t.is_1g1r_pick DESC, t.name",
    )?;
    let mut variants: Vec<VariantRow> = stmt
        .query_map([gid.0], variant_row)?
        .collect::<rusqlite::Result<_>>()?;
    fill_lists(conn, gid, &mut variants)?;
    let mut stmt = conn.prepare(
        "SELECT r.title_id, r.id, r.name, r.size, r.crc32, r.md5, r.sha1, r.status,
                f.id, f.state, f.rel_path
         FROM titles t JOIN roms r ON r.title_id = t.id AND r.retired = 0
         LEFT JOIN files f ON f.id = (
           SELECT x.id FROM files x WHERE x.rom_id = r.id
           ORDER BY CASE x.state WHEN 'verified' THEN 0 WHEN 'misnamed' THEN 1
                                 WHEN 'bad' THEN 2 ELSE 3 END, x.id
           LIMIT 1)
         WHERE t.group_root = ?1 OR (t.id = ?1 AND t.group_root IS NULL)
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
    for (title, found) in super::candidates::for_group(conn, gid)? {
        if let Some(v) = variants.iter_mut().find(|v| v.id == title) {
            let seen = |a: &Availability| {
                (a.source_id, a.file_index) == (found.source_id, found.file_index)
            };
            if !v.availability.iter().any(seen) {
                v.torrent_files_available += 1;
            }
            v.availability.push(found);
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
            "SELECT retired, EXISTS (SELECT 1 FROM title_flags f WHERE f.title_id = titles.id AND f.flag = 'bios')
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
        "UPDATE titles SET wanted = 0 WHERE (group_root = ?1 OR (id = ?1 AND group_root IS NULL)) AND wanted = 1",
        [parent.0],
    )?;
    conn.execute(
        "UPDATE downloads SET state = 'cancelled', updated_at = ?2
         WHERE state IN ('wanted', 'queued')
           AND title_id IN (SELECT id FROM titles WHERE group_root = ?1 OR (id = ?1 AND group_root IS NULL))",
        params![parent.0, now],
    )?;
    Ok(n)
}

#[cfg(test)]
mod tests;
