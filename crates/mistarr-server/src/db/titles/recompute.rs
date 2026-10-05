//! Clone parents, groups and 1G1R picks of a platform's titles; see `docs/VERIFICATION.md`
//! "Clone groups".

use std::collections::HashMap;

use mistarr_core::naming::{parse_name, ParsedName};
use mistarr_core::select::{infer_groups, select_1g1r, Prefs, Variant};
use mistarr_core::PlatformId;
use rusqlite::{params, Connection};

use crate::db::ids::{DatVersionId, TitleId};
use crate::db::sql;
use crate::error::Result;

/// Sets clone parents for the titles of `version`. With `use_clone_of` each
/// title's parent is the game its `cloneof` names, else itself; without it the
/// titles are marked `inferred` and [`recompute_platform`] groups them.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::{ids::DatVersionId, titles};
/// let mut conn = rusqlite::Connection::open_in_memory().unwrap();
/// mistarr_server::db::migrate::apply(&mut conn).unwrap();
/// titles::recompute::link_parents(&conn, DatVersionId::new(1), true).unwrap();
/// ```
pub fn link_parents(conn: &Connection, version: DatVersionId, use_clone_of: bool) -> Result<()> {
    if use_clone_of {
        conn.execute(
            "UPDATE titles SET inferred = 0, parent_id = COALESCE(
               (SELECT p.id FROM titles p
                WHERE p.dat_version_id = titles.dat_version_id AND p.name = titles.clone_of),
               id)
             WHERE dat_version_id = ?1",
            [version],
        )?;
    } else {
        conn.execute(
            "UPDATE titles SET inferred = 1 WHERE dat_version_id = ?1",
            [version],
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
    id: TitleId,
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
                id: sql::to_u64(c.id.get()),
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
    platform: &PlatformId,
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
/// let r = mistarr_server::db::titles::recompute::recompute_platform(&conn, &mistarr_core::PlatformId::new("nes"), &prefs).unwrap();
/// assert_eq!(r.groups, 0);
/// ```
pub fn recompute_platform(
    conn: &Connection,
    platform: &PlatformId,
    prefs: &Prefs,
) -> Result<Recomputed> {
    let defaults = Prefs::default();
    let mut parents: Vec<(TitleId, TitleId)> = Vec::new();
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
                let parent = TitleId::new(sql::to_i64(g.member_ids.first().copied().unwrap_or(0)));
                parents.extend(
                    g.member_ids
                        .iter()
                        .map(|&m| (TitleId::new(sql::to_i64(m)), parent)),
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
    let mut picks: Vec<TitleId> = Vec::new();
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
                picks.push(TitleId::new(sql::to_i64(pick.id)));
            }
        },
    )?;
    out.picks = sql::to_u64(sql::to_i64(picks.len()));
    store_picks(conn, platform, picks)?;
    Ok(out)
}

/// Sets `is_1g1r_pick` on exactly `picks` among `platform`'s titles, writing only the
/// rows that change so unchanged groups stay clean.
fn store_picks(conn: &Connection, platform: &PlatformId, mut picks: Vec<TitleId>) -> Result<()> {
    picks.sort_unstable();
    picks.dedup();
    let current: Vec<TitleId> = conn
        .prepare_cached(
            "SELECT id FROM titles WHERE platform_id = ?1 AND is_1g1r_pick = 1 ORDER BY id",
        )?
        .query_map([platform], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut set = conn.prepare_cached("UPDATE titles SET is_1g1r_pick = ?2 WHERE id = ?1")?;
    for &id in current.iter().filter(|id| picks.binary_search(id).is_err()) {
        set.execute(params![id, false])?;
    }
    for &id in picks.iter().filter(|id| current.binary_search(id).is_err()) {
        set.execute(params![id, true])?;
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
fn link_shared_titles(conn: &Connection, platform: &PlatformId) -> Result<()> {
    let versions: Vec<DatVersionId> = conn
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
    platform: &PlatformId,
    largest: DatVersionId,
    rest: &[DatVersionId],
) -> Result<Vec<(TitleId, TitleId)>> {
    // Titles outside the largest version by signature, in version order: (version, id, parent).
    let mut others: HashMap<u64, Vec<(DatVersionId, TitleId, TitleId)>> = HashMap::new();
    for &version in rest {
        each_signature(conn, platform, version, |id, parent, sig| {
            others.entry(sig).or_default().push((version, id, parent));
        })?;
    }
    let mut anchors: HashMap<u64, TitleId> = HashMap::new();
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
pub(super) struct Node {
    pub(super) id: TitleId,
    /// The group it is to belong to.
    pub(super) target: Option<TitleId>,
    /// Its stored `group_root`.
    pub(super) current: Option<TitleId>,
    pub(super) live: bool,
}

/// Every title of `platform` with `target` at its `parent_id`, and every title elsewhere
/// in a group a title of `platform` roots with `target` at that group, sorted by id.
fn group_nodes(conn: &Connection, platform: &PlatformId) -> Result<Vec<Node>> {
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
        let root: Option<TitleId> = r.get(1)?;
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
pub(super) fn reroot(nodes: &mut [Node]) {
    let stranded = |label: TitleId| {
        nodes
            .binary_search_by_key(&label, |n| n.id)
            .is_ok_and(|i| nodes[i].target != Some(label))
    };
    // Stranded group -> (lowest live member, lowest member); ids come in ascending order.
    let mut roots: HashMap<TitleId, (Option<TitleId>, TitleId)> = HashMap::new();
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
    platform: &PlatformId,
    version: DatVersionId,
    mut each: impl FnMut(TitleId, TitleId, u64),
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
    let mut open: Option<(TitleId, TitleId, u64, u64, bool)> = None;
    let mut settle = |t: Option<(TitleId, TitleId, u64, u64, bool)>| {
        if let Some((id, parent, sum, n, true)) = t {
            let mut h = DefaultHasher::new();
            (sum, n).hash(&mut h);
            each(id, parent, h.finish());
        }
    };
    while let Some(r) = rows.next()? {
        let (id, parent): (TitleId, TitleId) = (r.get(0)?, r.get(1)?);
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
