//! Binding one source as the user chose, or handing it back to automatic binding;
//! see `docs/ARCHITECTURE.md` "Source import" step 4.

use async_trait::async_trait;
use rusqlite::Connection;
use serde_json::{json, Value};

use super::remap::key_new_roms;
use super::source_import::{bind_to, publish_changed, rebind_one};
use super::{Job, JobContext, Lane};
use crate::db::sources::{self as rows, SourceId};
use crate::error::Result;

/// The `jobs.kind` of [`BindSource`].
pub const KIND: &str = "bind_source";

/// The reason a source the user marked as not a game set shows.
pub const IGNORED: &str = "Marked as not a game set. It is not bound automatically.";

pub use crate::db::sources::BindChoice as Choice;

/// Applies the binding the user last asked for on one source, as
/// [`rows::request_binding`] recorded it, through the same binding and matching
/// the automatic classifier uses, on the background lane. A job that finds no
/// request waiting does nothing, so requests sharing one queued job, or a job
/// queued behind one that already applied the latest request, are harmless.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindSource {
    /// The source.
    pub source_id: SourceId,
    /// Its display name, for the activity list.
    pub source_name: String,
}

impl BindSource {
    /// The job a stored payload describes, `None` when it names no source.
    ///
    /// ```
    /// use mistarr_server::jobs::bind_source::BindSource;
    /// let job = BindSource::from_payload(&serde_json::json!({ "source_id": 3 }));
    /// assert_eq!(job.map(|j| j.source_id.0), Some(3));
    /// ```
    #[must_use]
    pub fn from_payload(payload: &Value) -> Option<Self> {
        let source_id = SourceId(payload.get("source_id")?.as_i64()?);
        let source_name = payload
            .get("source_name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        Some(Self {
            source_id,
            source_name,
        })
    }
}

/// Applies `choice` to source `id` in the caller's transaction and records whether
/// the user chose the binding. Keeps `disabled`.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn apply(conn: &Connection, id: SourceId, choice: &Choice, threshold: f32) -> Result<()> {
    match choice {
        Choice::Platform(p) => {
            rows::set_user_binding(conn, id, true)?;
            bind_to(conn, id, Some(p))
        }
        Choice::Ignore => {
            rows::set_user_binding(conn, id, true)?;
            bind_to(conn, id, None)?;
            rows::set_reason(conn, id, Some(IGNORED))
        }
        Choice::Automatic => {
            rows::set_user_binding(conn, id, false)?;
            let files = rows::torrent_files(conn, id)?;
            rows::replace_files(conn, id, &files)?;
            crate::db::candidates::clear(conn, id)?;
            let suggested = rows::get(conn, id)?.and_then(|r| r.suggested_platform_id);
            rebind_one(conn, id, suggested.as_ref(), threshold)?;
            if rows::get(conn, id)?.is_some_and(|r| r.platform_id.is_none()) {
                rows::clear_matches(conn, id)?;
            }
            Ok(())
        }
    }
}

#[async_trait]
impl Job for BindSource {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn payload(&self) -> Value {
        json!({ "source_id": self.source_id, "source_name": self.source_name })
    }

    fn lane(&self) -> Lane {
        Lane::Background
    }

    async fn run(&self, ctx: &JobContext) -> Result<()> {
        let id = self.source_id;
        ctx.progress(json!({ "phase": "binding", "source_id": id }))
            .await?;
        key_new_roms(&ctx.app.db).await?;
        let threshold = ctx.app.config().sources.bind_threshold;
        let row = ctx
            .app
            .db
            .write_bulk(move |c| {
                let tx = c.transaction()?;
                let Some(choice) = rows::take_binding(&tx, id)? else {
                    return Ok(None);
                };
                if rows::get(&tx, id)?.is_some_and(|r| r.file_count > 0) {
                    apply(&tx, id, &choice, threshold)?;
                }
                let row = rows::get(&tx, id)?;
                crate::db::commit(tx)?;
                Ok(row)
            })
            .await?;
        let Some(row) = row else {
            return Ok(());
        };
        ctx.progress(json!({
            "source_id": id,
            "platform_id": row.platform_id,
            "matched": row.matched_count,
            "total": row.file_count,
        }))
        .await?;
        publish_changed(&ctx.app, &row);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use mistarr_core::PlatformId;
    use mistarr_sources::torrent::TorrentFile;

    use super::*;
    use crate::app::testutil::state;
    use crate::db::sources::fixtures::seed_rom;
    use crate::db::sources::{NewSource, SourceState};
    use crate::jobs::source_import::{bind_best, rebind_after_dat};
    use crate::jobs::Scheduler;

    fn file(index: u32, path: &str, size: u64) -> TorrentFile {
        TorrentFile {
            index,
            path: path.to_owned(),
            size,
        }
    }

    fn nes() -> PlatformId {
        PlatformId("nes".into())
    }

    /// A source of four files, one matching `nes` and two `snes`, bound to `snes` at half.
    fn source(c: &Connection) -> Result<SourceId> {
        seed_rom(c, "nes", "Example Quest (USA).nes", 16, &[])?;
        seed_rom(c, "snes", "Other Tale (USA).sfc", 32, &[])?;
        seed_rom(c, "snes", "Third Tale (USA).sfc", 64, &[])?;
        let id = rows::insert(
            c,
            &NewSource {
                infohash: &"5a".repeat(20),
                display_name: "Synthetic Set",
                origin_file: "set.torrent",
                state: SourceState::Unbound,
                reason: None,
                added_at: 0,
            },
        )?;
        let files = [
            file(0, "Example Quest (USA).nes", 16),
            file(1, "Other Tale (USA).sfc", 32),
            file(2, "Third Tale (USA).sfc", 64),
            file(3, "notes.txt", 1),
        ];
        rows::replace_files(c, id, &files)?;
        bind_best(c, id, &files, 0.5)?;
        Ok(id)
    }

    /// Records `choice` as a request would and runs one job for the source.
    async fn run(app: &Arc<crate::app::AppState>, id: SourceId, choice: Choice) {
        request(app, id, choice);
        run_job(app, id).await;
    }

    fn request(app: &crate::app::AppState, id: SourceId, choice: Choice) {
        app.db
            .write_blocking(move |c| rows::request_binding(c, id, &choice))
            .expect("request");
    }

    async fn run_job(app: &Arc<crate::app::AppState>, id: SourceId) {
        let job = BindSource {
            source_id: id,
            source_name: "Synthetic Set".into(),
        };
        Scheduler::run_inline(app, Arc::new(job))
            .await
            .expect("run");
    }

    async fn row(app: &crate::app::AppState, id: SourceId) -> rows::SourceRow {
        app.db
            .read(move |c| rows::get(c, id))
            .await
            .expect("get")
            .expect("row")
    }

    #[test]
    fn payloads_round_trip() {
        let job = BindSource {
            source_id: SourceId(4),
            source_name: "Synthetic Set".into(),
        };
        assert_eq!(BindSource::from_payload(&job.payload()), Some(job));
        assert_eq!(BindSource::from_payload(&json!({ "name": 1 })), None);
    }

    #[tokio::test]
    async fn the_last_request_wins_whichever_job_runs_it() {
        let (_dir, app) = state();
        let id = app.db.write_blocking(|c| source(c)).expect("seed");
        // Pick, reset and pick while the lane is busy: two jobs run, after all three.
        request(&app, id, Choice::Platform(nes()));
        request(&app, id, Choice::Automatic);
        let waiting = row(&app, id).await;
        assert_eq!(waiting.pending_binding, Some(Choice::Automatic));
        assert!(!waiting.user_binding);
        request(&app, id, Choice::Platform(nes()));
        run_job(&app, id).await;
        run_job(&app, id).await;
        let done = row(&app, id).await;
        assert_eq!(
            (
                done.platform_id.clone(),
                done.user_binding,
                done.pending_binding
            ),
            (Some(nes()), true, None)
        );
        assert_eq!(done.matched_count, 1);
    }

    #[tokio::test]
    async fn a_user_binding_is_set_survives_automatic_runs_and_resets() {
        let (_dir, app) = state();
        let id = app.db.write_blocking(|c| source(c)).expect("seed");
        let before = row(&app, id).await;
        assert_eq!(
            (before.platform_id.clone(), before.user_binding),
            (Some(PlatformId("snes".into())), false)
        );

        run(&app, id, Choice::Platform(nes())).await;
        let set = row(&app, id).await;
        assert_eq!(
            (set.platform_id.clone(), set.user_binding),
            (Some(nes()), true)
        );
        assert_eq!((set.state, set.matched_count), (SourceState::Bound, 1));

        app.db
            .write_blocking(|c| {
                seed_rom(c, "snes", "Example Quest (USA).sfc", 16, &[])?;
                Ok(())
            })
            .expect("seed");
        rebind_after_dat(&app, &[PlatformId("snes".into()), nes()])
            .await
            .expect("rebind");
        crate::jobs::remap::remap_one(&app, id)
            .await
            .expect("remap");
        let kept = row(&app, id).await;
        assert_eq!(
            (kept.platform_id.clone(), kept.user_binding),
            (Some(nes()), true)
        );

        run(&app, id, Choice::Ignore).await;
        let ignored = row(&app, id).await;
        assert_eq!(
            (ignored.state, ignored.platform_id.clone()),
            (SourceState::Unbound, None)
        );
        assert_eq!(
            (ignored.matched_count, ignored.reason.as_deref()),
            (0, Some(IGNORED))
        );
        rebind_after_dat(&app, &[]).await.expect("rebind");
        assert_eq!(
            row(&app, id).await.platform_id,
            None,
            "an ignored source stays unbound"
        );

        run(&app, id, Choice::Automatic).await;
        let reset = row(&app, id).await;
        assert_eq!(
            (reset.platform_id, reset.user_binding, reset.state),
            (Some(PlatformId("snes".into())), false, SourceState::Bound)
        );
        assert_eq!(reset.matched_count, 3);
    }
}
