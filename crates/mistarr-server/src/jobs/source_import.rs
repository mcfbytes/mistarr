//! Source import, binding and magnet resolving; the flow is `docs/ARCHITECTURE.md` "Source import".

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use mistarr_clients::{ClientError, ClientTorrentId, InfoHash, SeedPolicy, TorrentSource};
use mistarr_core::PlatformId;
use mistarr_sources::binding::{self, Binding};
use mistarr_sources::torrent::TorrentFile;
use mistarr_sources::watch::{self, Scanner};
use mistarr_sources::{magnet, torrent};
use rusqlite::Connection;
use serde::Serialize;
use serde_json::{json, Value};

use super::{Job, JobContext, Scheduler};
use crate::app::AppState;
use crate::db::sources::{self as rows, NewSource, SourceId, SourceRow, SourceState, SqlDatIndex};
use crate::error::Result;
use crate::events::EventKind;

/// The `jobs.kind` of [`SourceImport`].
pub const IMPORT_KIND: &str = "source_import";
/// The `jobs.kind` of [`ResolveMagnet`].
pub const RESOLVE_KIND: &str = "resolve_magnet";

/// Reason shown on a resolving source while no client is detected.
pub const NO_CLIENT: &str = "No download client found. The file list is read once one is detected.";
/// Reason shown while the client fetches a magnet's metadata.
pub const WAITING: &str = "Waiting for the download client to read the file list.";
/// Rejection reason for a second copy of a loaded source.
pub const DUPLICATE: &str = "A source with the same content is already loaded.";

/// The `source.changed` event body.
#[derive(Debug, Clone, Serialize)]
pub struct SourceChanged<'a> {
    /// The source.
    pub source_id: SourceId,
    /// Its state now.
    pub state: SourceState,
    /// Its platform, when bound.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform_id: Option<&'a PlatformId>,
}

/// Publishes `source.changed` for a source row.
pub fn publish_changed(app: &AppState, row: &SourceRow) {
    let body = SourceChanged {
        source_id: row.id,
        state: row.state,
        platform_id: row.platform_id.as_ref(),
    };
    app.events.publish(EventKind::SourceChanged, &body);
}

/// Reads one `.torrent` or `.magnet` file from `sources/`, records it and moves
/// it to `loaded/` or, with a reason file, to `rejected/`. A missing file is a no-op.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceImport {
    /// The file, directly inside `sources/`.
    pub path: PathBuf,
}

/// Adds a resolving magnet to the client if needed and binds it once the
/// client lists its files. Leaves a reason on the source while it waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolveMagnet {
    /// The resolving source.
    pub source_id: SourceId,
}

#[async_trait]
impl Job for SourceImport {
    fn kind(&self) -> &'static str {
        IMPORT_KIND
    }

    fn payload(&self) -> Value {
        json!({ "path": self.path.to_string_lossy() })
    }

    async fn run(&self, ctx: &JobContext) -> Result<()> {
        let data = match tokio::fs::read(&self.path).await {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        let origin = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let outcome = match self.path.extension().and_then(|e| e.to_str()) {
            Some("torrent") => import_torrent(&ctx.app, &origin, &data).await?,
            Some("magnet") => import_magnet(&ctx.app, &origin, &data).await?,
            _ => Err("Only .torrent and .magnet files are read.".to_owned()),
        };
        match outcome {
            Ok(row) => {
                if let Err(e) = move_blocking(&self.path, None).await {
                    tracing::warn!(file = %origin, error = %e, "cannot move source into loaded/");
                }
                ctx.progress(json!({ "file": origin, "source_id": row.id, "state": row.state }))
                    .await?;
                publish_changed(&ctx.app, &row);
                if row.state == SourceState::Resolving {
                    let job = Arc::new(ResolveMagnet { source_id: row.id });
                    Scheduler::enqueue(&ctx.app, job).await?;
                }
            }
            Err(reason) => {
                tracing::info!(file = %origin, reason, "source rejected");
                if let Err(e) = move_blocking(&self.path, Some(reason.clone())).await {
                    tracing::warn!(file = %origin, error = %e, "cannot move source into rejected/");
                }
                ctx.progress(json!({ "file": origin, "rejected": reason }))
                    .await?;
            }
        }
        Ok(())
    }
}

/// Moves into `loaded/`, or into `rejected/` with `reason`, off the async runtime.
async fn move_blocking(path: &Path, reason: Option<String>) -> Result<()> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || match reason {
        None => watch::mark_loaded(&path),
        Some(r) => watch::mark_rejected(&path, &r),
    })
    .await
    .map_err(|e| crate::Error::Task(e.to_string()))?
    .map_err(|e| crate::Error::Job(e.to_string()))
}

/// Parses and binds a `.torrent`. The inner error is a rejection reason.
async fn import_torrent(
    app: &AppState,
    origin: &str,
    data: &[u8],
) -> Result<Result<SourceRow, String>> {
    let meta = match torrent::parse_torrent(data) {
        Ok(m) => m,
        Err(e) => return Ok(Err(format!("Not a valid .torrent file: {e}."))),
    };
    let infohash = InfoHash::from_bytes(meta.infohash).to_string();
    let threshold = app.config().sources.bind_threshold;
    let origin = origin.to_owned();
    app.db
        .write(move |c| {
            let tx = c.transaction()?;
            let id = match rows::find_by_infohash(&tx, &infohash)? {
                Some(row) if row.state == SourceState::Resolving => row.id,
                Some(_) => return Ok(Err(DUPLICATE.to_owned())),
                None => rows::insert(
                    &tx,
                    &NewSource {
                        infohash: &infohash,
                        display_name: &meta.name,
                        origin_file: &origin,
                        state: SourceState::Unbound,
                        reason: None,
                        added_at: crate::unix_now(),
                    },
                )?,
            };
            rows::replace_files(&tx, id, &meta.files)?;
            bind_best(&tx, id, &meta.files, threshold)?;
            let row = rows::get(&tx, id)?;
            tx.commit()?;
            Ok(row.ok_or_else(|| "The source could not be read back.".to_owned()))
        })
        .await
}

/// Parses a `.magnet` and records it as resolving. The inner error is a rejection reason.
async fn import_magnet(
    app: &AppState,
    origin: &str,
    data: &[u8],
) -> Result<Result<SourceRow, String>> {
    let text = String::from_utf8_lossy(data);
    let uri = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let parsed = match magnet::parse_magnet(uri) {
        Ok(m) => m,
        Err(e) => return Ok(Err(format!("Not a valid .magnet file: {e}."))),
    };
    let infohash = InfoHash::from_bytes(parsed.infohash).to_string();
    let name = parsed.display_name.unwrap_or_else(|| {
        Path::new(origin)
            .file_stem()
            .map_or_else(|| infohash.clone(), |s| s.to_string_lossy().into_owned())
    });
    let origin = origin.to_owned();
    app.db
        .write(move |c| {
            if rows::find_by_infohash(c, &infohash)?.is_some() {
                return Ok(Err(DUPLICATE.to_owned()));
            }
            let id = rows::insert(
                c,
                &NewSource {
                    infohash: &infohash,
                    display_name: &name,
                    origin_file: &origin,
                    state: SourceState::Resolving,
                    reason: None,
                    added_at: crate::unix_now(),
                },
            )?;
            Ok(rows::get(c, id)?.ok_or_else(|| "The source could not be read back.".to_owned()))
        })
        .await
}

/// Binds a source whose `torrent_files` hold `files` to the best platform at or
/// above `threshold`, or leaves it unbound with a reason. Keeps `disabled`.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn bind_best(
    conn: &Connection,
    id: SourceId,
    files: &[TorrentFile],
    threshold: f32,
) -> Result<()> {
    rows::refresh_match_keys(conn)?;
    let index = SqlDatIndex::new(conn);
    let (state, reason) = match binding::bind(files, &index, threshold) {
        Binding::Bound(platform, rate) => {
            let matches = binding::match_files(files, &platform, &index);
            rows::set_matches(conn, id, &matches)?;
            rows::set_binding(conn, id, Some(&platform), Some(f64::from(rate)))?;
            (SourceState::Bound, None)
        }
        Binding::Unbound(_) => {
            rows::set_binding(conn, id, None, None)?;
            (SourceState::Unbound, Some(unbound_reason(threshold)))
        }
    };
    keep_disabled(conn, id, state, reason.as_deref())
}

/// Binds a source to `platform` chosen by the user, matching its files against
/// that platform only; `None` unbinds it. Keeps `disabled`.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn bind_to(conn: &Connection, id: SourceId, platform: Option<&PlatformId>) -> Result<()> {
    let files = rows::torrent_files(conn, id)?;
    rows::replace_files(conn, id, &files)?;
    let Some(platform) = platform else {
        rows::set_binding(conn, id, None, None)?;
        return keep_disabled(conn, id, SourceState::Unbound, None);
    };
    rows::refresh_match_keys(conn)?;
    let index = SqlDatIndex::new(conn);
    let matches = binding::match_files(&files, platform, &index);
    let hits = matches.iter().filter(|(_, rom, _)| rom.is_some()).count();
    // File counts are far below 2^52, so the rate is exact enough.
    #[allow(clippy::cast_precision_loss)]
    let rate = if files.is_empty() {
        0.0
    } else {
        hits as f64 / files.len() as f64
    };
    rows::set_matches(conn, id, &matches)?;
    rows::set_binding(conn, id, Some(platform), Some(rate))?;
    keep_disabled(conn, id, SourceState::Bound, None)
}

fn keep_disabled(
    conn: &Connection,
    id: SourceId,
    state: SourceState,
    reason: Option<&str>,
) -> Result<()> {
    let disabled = rows::get(conn, id)?.is_some_and(|r| r.state == SourceState::Disabled);
    if disabled {
        rows::set_reason(conn, id, reason)
    } else {
        rows::set_state(conn, id, state, reason)
    }
}

/// The reason an unbound source shows.
///
/// ```
/// let r = mistarr_server::jobs::source_import::unbound_reason(0.6);
/// assert!(r.contains("60%"));
/// ```
#[must_use]
pub fn unbound_reason(threshold: f32) -> String {
    let pct = (threshold * 100.0).round();
    format!("No platform matched {pct}% of the files. Pick a platform to bind it.")
}

#[async_trait]
impl Job for ResolveMagnet {
    fn kind(&self) -> &'static str {
        RESOLVE_KIND
    }

    fn payload(&self) -> Value {
        json!({ "source_id": self.source_id })
    }

    async fn run(&self, ctx: &JobContext) -> Result<()> {
        let app = &ctx.app;
        let id = self.source_id;
        let Some(row) = app.db.read(move |c| rows::get(c, id)).await? else {
            return Ok(());
        };
        if row.state != SourceState::Resolving {
            return Ok(());
        }
        let Some(client) = app.client() else {
            return note(app, &row, NO_CLIENT).await;
        };
        let torrent = if let Some(existing) = &row.client_id {
            ClientTorrentId::new(existing.as_str())
        } else {
            let uri = magnet_uri(app, &row).await;
            let config = app.config();
            let local = config.paths.staging().join(&row.infohash);
            let dir = crate::client::to_remote(&config.client.remote_path_map, &local);
            let seed = rows::seed_from_text(&row.seed_policy).unwrap_or(SeedPolicy::None);
            match client
                .add(TorrentSource::Magnet(uri), &dir, &[], seed)
                .await
            {
                Ok(added) => {
                    let stored = added.as_str().to_owned();
                    app.db
                        .write(move |c| rows::set_client_id(c, id, Some(&stored)))
                        .await?;
                    added
                }
                Err(e) => return note(app, &row, &unanswered(&e)).await,
            }
        };
        let listed = match client.files(&torrent).await {
            Ok(files) => files,
            Err(ClientError::MetadataPending) => return note(app, &row, WAITING).await,
            Err(ClientError::NotFound) => {
                app.db
                    .write(move |c| rows::set_client_id(c, id, None))
                    .await?;
                return note(app, &row, WAITING).await;
            }
            Err(e) => return note(app, &row, &unanswered(&e)).await,
        };
        if let Err(e) = client.set_wanted(&torrent, &[]).await {
            tracing::warn!(source = %id, error = %e, "cannot clear the magnet's file selection");
        }
        let files: Vec<TorrentFile> = listed
            .into_iter()
            .map(|f| TorrentFile {
                index: f.index,
                path: f.path,
                size: f.size,
            })
            .collect();
        let threshold = app.config().sources.bind_threshold;
        let bound = app
            .db
            .write(move |c| {
                let tx = c.transaction()?;
                let still = rows::get(&tx, id)?.is_some_and(|r| r.state == SourceState::Resolving);
                if !still {
                    return Ok(None);
                }
                rows::replace_files(&tx, id, &files)?;
                bind_best(&tx, id, &files, threshold)?;
                let row = rows::get(&tx, id)?;
                tx.commit()?;
                Ok(row)
            })
            .await?;
        if let Some(row) = bound {
            ctx.progress(json!({ "source_id": id, "state": row.state }))
                .await?;
            publish_changed(app, &row);
        }
        Ok(())
    }
}

fn unanswered(e: &ClientError) -> String {
    format!("The download client did not accept the source: {e}.")
}

/// Stores `reason` on a resolving source and announces it, unless it is already shown.
async fn note(app: &AppState, row: &SourceRow, reason: &str) -> Result<()> {
    if row.reason.as_deref() == Some(reason) {
        return Ok(());
    }
    let (id, text) = (row.id, reason.to_owned());
    app.db
        .write(move |c| rows::set_reason(c, id, Some(&text)))
        .await?;
    publish_changed(app, row);
    Ok(())
}

/// The magnet as the user dropped it, from `sources/loaded/`, or one built
/// from the infohash when that file is gone or now holds another source.
async fn magnet_uri(app: &AppState, row: &SourceRow) -> String {
    let file = app
        .config()
        .paths
        .sources()
        .join(watch::LOADED_DIR)
        .join(&row.origin_file);
    let text = tokio::fs::read_to_string(&file).await.unwrap_or_default();
    let dropped = text.lines().map(str::trim).find(|l| !l.is_empty());
    match dropped {
        Some(uri)
            if magnet::parse_magnet(uri)
                .is_ok_and(|m| InfoHash::from_bytes(m.infohash).to_string() == row.infohash) =>
        {
            uri.to_owned()
        }
        _ => format!("magnet:?xt=urn:btih:{}", row.infohash),
    }
}

/// Scans `sources/` every [`crate::app::Options::sources_poll`] and enqueues
/// one [`SourceImport`] per stable file; a file already queued is not queued twice.
pub async fn watch(app: Arc<AppState>) {
    let dir = app.config().paths.sources();
    let mut scanner = Scanner::with_min_age_secs(app.options.sources_min_age_secs);
    let mut tick = tokio::time::interval(app.options.sources_poll);
    loop {
        tick.tick().await;
        let d = dir.clone();
        let result = tokio::task::spawn_blocking(move || {
            let found = scanner.scan_once(&d);
            (scanner, found)
        })
        .await;
        let Ok((back, found)) = result else {
            tracing::warn!("sources scan stopped");
            return;
        };
        scanner = back;
        for incoming in found {
            let job = Arc::new(SourceImport {
                path: incoming.path,
            });
            if let Err(e) = Scheduler::enqueue(&app, job).await {
                tracing::warn!(error = %e, "cannot queue a source import");
            }
        }
    }
}

/// Enqueues a [`ResolveMagnet`] for every resolving source every
/// [`crate::app::Options::magnet_poll`], so a new client or new metadata is picked up.
pub async fn resolve_pending(app: Arc<AppState>) {
    let mut tick = tokio::time::interval(app.options.magnet_poll);
    loop {
        tick.tick().await;
        let pending = match app.db.read(rows::list_resolving).await {
            Ok(ids) => ids,
            Err(e) => {
                tracing::warn!(error = %e, "cannot list resolving sources");
                continue;
            }
        };
        for source_id in pending {
            if let Err(e) = Scheduler::enqueue(&app, Arc::new(ResolveMagnet { source_id })).await {
                tracing::warn!(error = %e, "cannot queue magnet resolving");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::state;
    use crate::db::sources::fixtures::seed_rom;

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
                state: SourceState::Unbound,
                reason: None,
                added_at: 0,
            },
        )
        .expect("insert");
        rows::replace_files(c, id, files).expect("files");
        id
    }

    #[test]
    fn best_binding_and_manual_binding() {
        let (_dir, app) = state();
        app.db
            .write_blocking(|c| {
                seed_rom(c, "nes", "Example Quest (USA).nes", 16, "[]")?;
                let files = [
                    file(0, "a/Example Quest (USA).nes", 16),
                    file(1, "b.txt", 1),
                ];
                let id = source(c, &"0a".repeat(20), &files);
                bind_best(c, id, &files, 0.6)?;
                let row = rows::get(c, id)?.expect("row");
                assert_eq!(row.state, SourceState::Unbound);
                assert_eq!(row.reason, Some(unbound_reason(0.6)));
                bind_best(c, id, &files, 0.5)?;
                let row = rows::get(c, id)?.expect("row");
                assert_eq!((row.state, row.matched_count), (SourceState::Bound, 1));
                assert_eq!(row.bind_score, Some(0.5));
                rows::set_state(c, id, SourceState::Disabled, None)?;
                bind_to(c, id, Some(&PlatformId("snes".into())))?;
                let row = rows::get(c, id)?.expect("row");
                assert_eq!(row.state, SourceState::Disabled);
                assert_eq!((row.matched_count, row.bind_score), (0, Some(0.0)));
                rows::set_state(c, id, SourceState::Unbound, None)?;
                bind_to(c, id, Some(&PlatformId("nes".into())))?;
                let row = rows::get(c, id)?.expect("row");
                assert_eq!((row.state, row.matched_count), (SourceState::Bound, 1));
                bind_to(c, id, None)?;
                let row = rows::get(c, id)?.expect("row");
                assert_eq!((row.state, row.platform_id), (SourceState::Unbound, None));
                Ok(())
            })
            .expect("db");
    }

    #[tokio::test]
    async fn resolving_without_a_client_says_so() {
        let (_dir, app) = state();
        let id = app
            .db
            .write_blocking(|c| {
                rows::insert(
                    c,
                    &NewSource {
                        infohash: &"0b".repeat(20),
                        display_name: "m",
                        origin_file: "m.magnet",
                        state: SourceState::Resolving,
                        reason: None,
                        added_at: 0,
                    },
                )
            })
            .expect("insert");
        assert!(app.client().is_none());
        let mut events = app.events.subscribe(None).live;
        Scheduler::run_inline(&app, Arc::new(ResolveMagnet { source_id: id }))
            .await
            .expect("run");
        let row = app.db.read(move |c| rows::get(c, id)).await.expect("get");
        let row = row.expect("row");
        assert_eq!(row.state, SourceState::Resolving);
        assert_eq!(row.reason.as_deref(), Some(NO_CLIENT));
        let ev = events.recv().await.expect("event");
        assert_eq!(ev.kind, EventKind::SourceChanged);
        assert!(ev.data.contains(r#""state":"resolving""#));
    }

    #[tokio::test]
    async fn import_of_a_missing_file_is_a_no_op() {
        let (dir, app) = state();
        let job = SourceImport {
            path: dir.path().join("gone.torrent"),
        };
        assert_eq!(job.payload()["path"], json!(job.path.to_string_lossy()));
        Scheduler::run_inline(&app, Arc::new(job))
            .await
            .expect("run");
        let (items, _) = app.db.read(|c| rows::list(c, 10, 0)).await.expect("list");
        assert!(items.is_empty());
    }

    #[tokio::test]
    async fn rejected_files_get_a_reason() {
        let (_dir, app) = state();
        let sources = app.config().paths.sources();
        std::fs::create_dir_all(&sources).expect("mkdir");
        let bad = sources.join("broken.magnet");
        std::fs::write(&bad, "not a magnet").expect("write");
        let job = Arc::new(SourceImport { path: bad.clone() });
        Scheduler::run_inline(&app, job).await.expect("run");
        assert!(!bad.exists());
        let reason = std::fs::read_to_string(sources.join("rejected/broken.magnet.reason.txt"))
            .expect("reason");
        assert!(reason.starts_with("Not a valid .magnet file"), "{reason}");
    }
}
