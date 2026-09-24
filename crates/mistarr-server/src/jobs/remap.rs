//! Mapping a source's files to its platform's roms, and mapping bound sources
//! again when those roms change; see `docs/VERIFICATION.md` "Pre-download matching".

use std::sync::Arc;

use async_trait::async_trait;
use mistarr_core::PlatformId;
use mistarr_mister::platforms::{self, Kind};
use mistarr_sources::binding;
use mistarr_sources::fuzzy;
use mistarr_sources::torrent::TorrentFile;
use rusqlite::Connection;
use serde_json::{json, Value};

use super::source_import::publish_changed;
use super::{Job, JobContext, Lane};
use crate::app::AppState;
use crate::db::candidates::{self, Change, SqlSizeIndex};
use crate::db::sources::{self as rows, SourceId, SqlDatIndex};
use crate::error::Result;

/// The `jobs.kind` of [`RemapSources`].
pub const KIND: &str = "remap_sources";

/// Rows written per transaction when a source is mapped again.
const CHUNK: usize = 2_000;

/// The extensions the fuzzy and size-only tiers consider for `platform`:
/// those its core loads, for a cartridge platform only.
///
/// ```
/// use mistarr_core::PlatformId;
/// use mistarr_server::jobs::remap::fuzzy_extensions;
/// assert_eq!(fuzzy_extensions(&PlatformId("nes".into())), ["nes"]);
/// assert!(fuzzy_extensions(&PlatformId("psx".into())).is_empty());
/// ```
#[must_use]
pub fn fuzzy_extensions(platform: &PlatformId) -> Vec<&'static str> {
    platforms::by_id(&platform.0)
        .filter(|row| row.kind == Kind::Cartridge)
        .map(|row| row.load_extensions.to_vec())
        .unwrap_or_default()
}

/// A mapping worked out on a read connection, ready to write.
#[derive(Debug, Clone)]
pub struct Planned {
    /// The writes that store it.
    pub change: Change,
    /// Files the name tiers matched.
    pub hits: usize,
    /// [`candidates::rom_stamp`] of the platform it was worked out against.
    pub stamp: String,
}

/// Works out the mapping of `files` to the roms of `platform` by every tier:
/// the best name match per file, the other roms of that tier, and the fuzzy
/// and size-only candidates of the files left over. Reads only; run
/// [`rows::refresh_match_keys`] first.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn plan(
    conn: &Connection,
    id: SourceId,
    platform: &PlatformId,
    files: &[TorrentFile],
) -> Result<Planned> {
    let stamp = candidates::rom_stamp(conn, platform)?;
    let mapping = binding::map_files(files, platform, &SqlDatIndex::new(conn));
    let unmatched = mapping.unmatched(files);
    let hits = files.len() - unmatched.len();
    let extensions = fuzzy_extensions(platform);
    let size_index = SqlSizeIndex::new(conn, platform);
    let mut found = mapping.extra;
    found.extend(fuzzy::candidates(
        files,
        &unmatched,
        &extensions,
        &size_index,
    ));
    let change = candidates::diff(&candidates::stored(conn, id)?, &mapping.matches, &found);
    Ok(Planned {
        change,
        hits,
        stamp,
    })
}

/// Maps the source's `files` to `platform` in the caller's transaction and
/// records the stamp; returns how many files the name tiers matched.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn map_files(
    conn: &Connection,
    id: SourceId,
    platform: &PlatformId,
    files: &[TorrentFile],
) -> Result<usize> {
    rows::refresh_match_keys(conn)?;
    let planned = plan(conn, id, platform, files)?;
    candidates::apply(conn, id, &planned.change)?;
    rows::set_map_stamp(conn, id, Some(&planned.stamp))?;
    Ok(planned.hits)
}

/// Maps one source again when its platform's roms changed since it was last
/// mapped, writing only what changed, [`CHUNK`] rows per transaction, then
/// its stamp and hit rate. Publishes `source.changed` when the mapping
/// changed and returns whether it did.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub async fn remap_one(app: &AppState, id: SourceId) -> Result<bool> {
    let current = app
        .db
        .read(move |c| {
            let Some(row) = rows::get(c, id)? else {
                return Ok(None);
            };
            let Some(platform) = row.platform_id.clone() else {
                return Ok(None);
            };
            let fresh = rows::map_stamp(c, id)? == Some(candidates::rom_stamp(c, &platform)?);
            Ok((!fresh).then_some((row, platform)))
        })
        .await?;
    let Some((row, platform)) = current else {
        return Ok(false);
    };
    app.db.write(|c| rows::refresh_match_keys(c)).await?;
    let p = platform.clone();
    let (planned, total) = app
        .db
        .read(move |c| {
            let files = rows::torrent_files(c, id)?;
            Ok((plan(c, id, &p, &files)?, files.len()))
        })
        .await?;
    let changed = !planned.change.is_empty();
    for piece in planned.change.split(CHUNK) {
        let p = platform.clone();
        let written = app
            .db
            .write(move |c| {
                let tx = c.transaction()?;
                if !still_on(&tx, id, &p)? {
                    return Ok(false);
                }
                candidates::apply(&tx, id, &piece)?;
                tx.commit()?;
                Ok(true)
            })
            .await?;
        if !written {
            return Ok(false);
        }
    }
    // File counts are far below 2^52, so the rate is exact enough.
    #[allow(clippy::cast_precision_loss)]
    let rate = if total == 0 {
        0.0
    } else {
        planned.hits as f64 / total as f64
    };
    let (p, stamp) = (platform.clone(), planned.stamp);
    app.db
        .write(move |c| {
            if still_on(c, id, &p)? {
                rows::set_map_stamp(c, id, Some(&stamp))?;
                rows::set_binding(c, id, Some(&p), Some(rate))?;
            }
            Ok(())
        })
        .await?;
    if changed {
        tracing::info!(source = %id, platform = %platform.0, "source mapped again");
        publish_changed(app, &row);
    }
    Ok(changed)
}

fn still_on(conn: &Connection, id: SourceId, platform: &PlatformId) -> Result<bool> {
    Ok(rows::get(conn, id)?.is_some_and(|r| r.platform_id.as_ref() == Some(platform)))
}

/// Maps again, one at a time, the sources bound to `platforms`, or to any
/// platform when `None`, whose platform's roms changed since they were mapped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemapSources {
    /// The platforms whose roms changed; `None` for all.
    pub platforms: Option<Vec<PlatformId>>,
}

#[async_trait]
impl Job for RemapSources {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn payload(&self) -> Value {
        json!({ "platforms": self.platforms.as_ref().map(|p| p.iter().map(|x| x.0.clone()).collect::<Vec<_>>()) })
    }

    fn lane(&self) -> Lane {
        Lane::Background
    }

    async fn run(&self, ctx: &JobContext) -> Result<()> {
        let platforms = self.platforms.clone();
        let ids = ctx
            .app
            .db
            .read(move |c| match &platforms {
                Some(p) => rows::list_on_platforms(c, p),
                None => rows::list_mapped(c),
            })
            .await?;
        let mut changed = 0;
        for (done, id) in ids.iter().enumerate() {
            ctx.checkpoint().await?;
            if remap_one(&ctx.app, *id).await? {
                changed += 1;
            }
            ctx.progress(json!({ "done": done + 1, "total": ids.len(), "changed": changed }))
                .await?;
        }
        Ok(())
    }
}

/// Queues a re-map of every mapped source when one was bound before its
/// mapping was stamped; returns whether it did.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure, or a stopped scheduler.
pub async fn enqueue_if_unstamped(app: &Arc<AppState>) -> Result<bool> {
    if !app.db.read(rows::any_unstamped).await? {
        return Ok(false);
    }
    let job = RemapSources { platforms: None };
    super::Scheduler::enqueue(app, Arc::new(job)).await?;
    Ok(true)
}

impl RemapSources {
    /// The job a stored payload describes.
    ///
    /// ```
    /// use mistarr_server::jobs::remap::RemapSources;
    /// let job = RemapSources::from_payload(&serde_json::json!({ "platforms": ["nes"] }));
    /// assert_eq!(job.platforms.map(|p| p.len()), Some(1));
    /// ```
    #[must_use]
    pub fn from_payload(payload: &Value) -> Self {
        let platforms = payload.get("platforms").and_then(Value::as_array).map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(|p| PlatformId(p.to_owned()))
                .collect()
        });
        Self { platforms }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::app::testutil::state;
    use crate::db::sources::fixtures::seed_rom;
    use crate::db::sources::{NewSource, SourceState};
    use crate::jobs::Scheduler;

    fn file(index: u32, path: &str, size: u64) -> TorrentFile {
        TorrentFile {
            index,
            path: path.to_owned(),
            size,
        }
    }

    fn source(c: &Connection, hash: &str, files: &[TorrentFile]) -> SourceId {
        let id = rows::insert(
            c,
            &NewSource {
                infohash: hash,
                display_name: "Synthetic",
                origin_file: "s.torrent",
                state: SourceState::Bound,
                reason: None,
                added_at: 0,
            },
        )
        .expect("insert");
        rows::replace_files(c, id, files).expect("files");
        rows::set_binding(c, id, Some(&PlatformId("nes".into())), Some(0.0)).expect("bind");
        id
    }

    /// Whether a `source.changed` arrived since the last call.
    fn announced(rx: &mut tokio::sync::broadcast::Receiver<Arc<crate::events::Event>>) -> bool {
        let mut seen = false;
        while let Ok(ev) = rx.try_recv() {
            seen |= ev.kind == crate::events::EventKind::SourceChanged;
        }
        seen
    }

    #[test]
    fn fuzzy_extensions_are_those_of_cartridge_platforms() {
        assert_eq!(fuzzy_extensions(&PlatformId("snes".into())), ["sfc", "smc"]);
        assert!(fuzzy_extensions(&PlatformId("psx".into())).is_empty());
        assert!(fuzzy_extensions(&PlatformId("neogeo".into())).is_empty());
        assert!(fuzzy_extensions(&PlatformId("none".into())).is_empty());
    }

    #[tokio::test]
    async fn an_unstamped_source_is_mapped_again_at_start() {
        let (_dir, app) = state();
        assert!(!enqueue_if_unstamped(&app).await.expect("none"));
        app.db
            .write_blocking(|c| {
                source(c, &"2b".repeat(20), &[file(0, "a.nes", 16)]);
                Ok(())
            })
            .expect("db");
        assert!(enqueue_if_unstamped(&app).await.expect("queued"));
        let queued = app
            .db
            .read(|c| crate::db::jobs::count_kind(c, KIND))
            .await
            .expect("count");
        assert_eq!(queued, 1);
    }

    #[tokio::test]
    async fn a_source_is_mapped_again_only_when_its_roms_change() {
        let (_dir, app) = state();
        let nes = PlatformId("nes".into());
        let id = app
            .db
            .write_blocking(|c| {
                let files = [
                    file(0, "nova.nes", 16),
                    file(1, "Other Tale (USA).nes", 8),
                    file(2, "a.txt", 1),
                ];
                let id = source(c, &"2a".repeat(20), &files);
                assert_eq!(map_files(c, id, &PlatformId("nes".into()), &files)?, 0);
                Ok(id)
            })
            .expect("db");
        assert!(!remap_one(&app, id).await.expect("remap"), "no rom changed");
        app.db
            .write_blocking(|c| {
                seed_rom(c, "nes", "Nova Quest (World).nes", 16, "[]")?;
                seed_rom(c, "nes", "Other Tale (USA).nes", 8, "[]")?;
                Ok(())
            })
            .expect("seed");
        let mut events = app.events.subscribe(None).live;
        let job = RemapSources {
            platforms: Some(vec![nes.clone()]),
        };
        assert_eq!(RemapSources::from_payload(&job.payload()), job);
        Scheduler::run_inline(&app, Arc::new(job))
            .await
            .expect("run");
        assert!(announced(&mut events), "the changed source is announced");
        let row = app.db.read(move |c| rows::get(c, id)).await.expect("get");
        let row = row.expect("row");
        assert_eq!(row.matched_count, 2);
        assert!((row.bind_score.unwrap_or_default() - 1.0 / 3.0).abs() < 1e-9);
        let found = app
            .db
            .read(move |c| candidates::of_file(c, id, 0))
            .await
            .expect("of");
        assert_eq!(found.len(), 1);
        assert!(!remap_one(&app, id).await.expect("again"), "stamp is fresh");
        let all = RemapSources { platforms: None };
        Scheduler::run_inline(&app, Arc::new(all))
            .await
            .expect("run all");
        assert!(!announced(&mut events), "nothing changed");
    }
}
