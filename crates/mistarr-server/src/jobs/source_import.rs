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

use super::remap::{map_files, store_mapping};
use super::{wizard, Job, JobContext, Lane, Scheduler};
use crate::app::AppState;
use crate::db::candidates;
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

/// Largest `.torrent` or `.magnet` file read; a torrent listing 100 000 files is
/// under half of it, and the whole file is held while it is parsed.
pub const MAX_SOURCE_BYTES: u64 = 16 * 1024 * 1024;

/// Rejection reason for a file above [`MAX_SOURCE_BYTES`].
pub const TOO_LARGE: &str = "The file is larger than 16 MiB, the most a source may be.";

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

/// The bytes of the file at `path`, or `None` when it holds more than [`MAX_SOURCE_BYTES`];
/// never reads more than one byte past the limit, even from a file that grows meanwhile.
///
/// # Errors
///
/// The I/O error when the file cannot be opened or read.
pub async fn read_bounded(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    use std::io::Read as _;
    let path = path.to_path_buf();
    crate::threads::blocking(crate::threads::label::SOURCE_FILE, move || {
        let file = std::fs::File::open(&path)?;
        if file.metadata()?.len() > MAX_SOURCE_BYTES {
            return Ok(None);
        }
        let mut data = Vec::new();
        file.take(MAX_SOURCE_BYTES + 1).read_to_end(&mut data)?;
        Ok((data.len() as u64 <= MAX_SOURCE_BYTES).then_some(data))
    })
    .await
    .map_err(std::io::Error::other)?
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

/// Adds a resolving magnet to the client with nothing wanted and starts it so
/// the client fetches metadata; once the client lists the files, leaves them
/// unwanted, stops the torrent and binds the source. Leaves a reason while it waits.
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

    fn lane(&self) -> Lane {
        Lane::Background
    }

    async fn run(&self, ctx: &JobContext) -> Result<()> {
        let data = match read_bounded(&self.path).await {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        let origin = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let outcome = match (self.path.extension().and_then(|e| e.to_str()), data) {
            (_, None) => Err(TOO_LARGE.to_owned()),
            (Some("torrent"), Some(data)) => import_torrent(&ctx.app, &origin, data).await?,
            (Some("magnet"), Some(data)) => import_magnet(&ctx.app, &origin, &data).await?,
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
                if let Err(e) = wizard::on_change(&ctx.app).await {
                    tracing::warn!(error = %e, "cannot check wizard completion");
                }
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
    crate::threads::blocking(crate::threads::label::SOURCE_FILE, move || match reason {
        None => watch::mark_loaded(&path),
        Some(r) => watch::mark_rejected(&path, &r),
    })
    .await
    .map_err(|e| crate::Error::Task(e.to_string()))?
    .map_err(|e| crate::Error::Job(e.to_string()))
}

/// Parses and binds a `.torrent`, dropping its bytes once parsed. The inner error is a
/// rejection reason.
async fn import_torrent(
    app: &AppState,
    origin: &str,
    data: Vec<u8>,
) -> Result<Result<SourceRow, String>> {
    let meta = match torrent::parse_torrent(&data) {
        Ok(m) => m,
        Err(e) => return Ok(Err(format!("Not a valid .torrent file: {e}."))),
    };
    drop(data);
    let infohash = InfoHash::from_bytes(meta.infohash).to_string();
    let threshold = app.config().sources.bind_threshold;
    let origin = origin.to_owned();
    app.db
        .write_bulk(move |c| {
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
            suggest(&tx, id, &origin, &meta.name, &meta.files)?;
            bind_best(&tx, id, &meta.files, threshold)?;
            let row = rows::get(&tx, id)?;
            crate::db::commit(tx)?;
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
    let (binding, mapping) = binding::bind_and_map(files, &SqlDatIndex::new(conn), threshold);
    let (state, reason) = match binding {
        Binding::Bound(platform, rate) => {
            store_mapping(conn, id, &platform, files, mapping)?;
            rows::set_binding(conn, id, Some(&platform), Some(f64::from(rate)))?;
            (SourceState::Bound, None)
        }
        Binding::Unbound(_) => {
            candidates::clear(conn, id)?;
            rows::set_binding(conn, id, None, None)?;
            (
                SourceState::Unbound,
                Some(unbound_explained(conn, id, threshold)?),
            )
        }
    };
    keep_disabled(conn, id, state, reason.as_deref())
}

/// Guesses the source's platform from its names, with no DAT, and stores it;
/// see `mistarr_mister::platforms::guess_platform`.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn suggest(
    conn: &Connection,
    id: SourceId,
    origin_file: &str,
    info_name: &str,
    files: &[TorrentFile],
) -> Result<Option<PlatformId>> {
    let hints = binding::name_hints(origin_file, info_name, files);
    let dirs = hints.dirs.iter().map(|(d, n)| (d.as_str(), *n));
    let names = hints.names.iter().map(String::as_str);
    let guess = mistarr_mister::platforms::guess_platform(names, dirs)
        .map(mistarr_mister::platforms::Platform::platform_id);
    rows::set_suggestion(conn, id, guess.as_ref())?;
    Ok(guess)
}

/// The reason an unbound source shows, naming its suggested platform and
/// whether a DAT for that platform is loaded yet.
fn unbound_explained(conn: &Connection, id: SourceId, threshold: f32) -> Result<String> {
    let suggested = rows::get(conn, id)?.and_then(|r| r.suggested_platform_id);
    let Some(platform) = suggested else {
        return Ok(unbound_reason(threshold));
    };
    let name =
        mistarr_mister::platforms::by_id(&platform.0).map_or(platform.0.as_str(), |p| p.name);
    if rows::platform_has_dat(conn, &platform)? {
        Ok(format!(
            "{} Its names suggest {name}.",
            unbound_reason(threshold)
        ))
    } else {
        Ok(awaiting_dat_reason(name))
    }
}

/// The reason a source shows while the DAT of its suggested platform is missing.
///
/// ```
/// let r = mistarr_server::jobs::source_import::awaiting_dat_reason("Example System");
/// assert!(r.starts_with("Looks like Example System."));
/// ```
#[must_use]
pub fn awaiting_dat_reason(platform_name: &str) -> String {
    format!("Looks like {platform_name}. No DAT for it is loaded yet; it binds once one loads.")
}

/// Binds the unbound sources again after a DAT loaded titles for `platforms`,
/// skipping those the user unbound, and queues a [`RemapSources`] for the
/// sources already bound to one of `platforms`. A source binds to its suggested platform
/// when that reaches the threshold and no other platform scores higher, else
/// as [`bind_best`] decides. Publishes `source.changed` for sources whose
/// state or platform changed, and returns how many bound.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub async fn rebind_after_dat(app: &Arc<AppState>, platforms: &[PlatformId]) -> Result<usize> {
    let threshold = app.config().sources.bind_threshold;
    let platforms_queued = platforms.to_vec();
    let changed = app
        .db
        .write_bulk(move |c| {
            let tx = c.transaction()?;
            let mut out = Vec::new();
            for (id, suggested) in rows::list_unbound(&tx)? {
                let before = rows::get(&tx, id)?;
                rebind_one(&tx, id, suggested.as_ref(), threshold)?;
                let after = rows::get(&tx, id)?;
                if let (Some(b), Some(a)) = (before, after) {
                    if (b.state, &b.platform_id) != (a.state, &a.platform_id) {
                        out.push(a);
                    }
                }
            }
            crate::db::commit(tx)?;
            Ok(out)
        })
        .await?;
    if !platforms_queued.is_empty() {
        super::remap::enqueue(app, Some(platforms_queued)).await;
    }
    let mut bound = 0;
    for row in &changed {
        if row.state == SourceState::Bound {
            bound += 1;
            tracing::info!(source = %row.id, platform = ?row.platform_id, "source bound after a DAT loaded");
        }
        publish_changed(app, row);
    }
    Ok(bound)
}

/// Binds one unbound source to `suggested` when it reaches `threshold` and
/// scores at least as well as the best platform, else as [`bind_best`] does.
fn rebind_one(
    conn: &Connection,
    id: SourceId,
    suggested: Option<&PlatformId>,
    threshold: f32,
) -> Result<()> {
    let files = rows::torrent_files(conn, id)?;
    let Some(platform) = suggested else {
        return bind_best(conn, id, &files, threshold);
    };
    rows::refresh_match_keys(conn)?;
    let index = SqlDatIndex::new(conn);
    let hits = binding::match_files(&files, platform, &index)
        .iter()
        .filter(|(_, rom, _)| rom.is_some())
        .count();
    // File counts are far below 2^24, so the rate is exact enough.
    #[allow(clippy::cast_precision_loss)]
    let rate = if files.is_empty() {
        0.0
    } else {
        hits as f32 / files.len() as f32
    };
    let beaten = match binding::bind(&files, &index, threshold) {
        Binding::Bound(best, best_rate) => best != *platform && best_rate > rate,
        Binding::Unbound(_) => false,
    };
    if rate >= threshold && !beaten {
        bind_to(conn, id, Some(platform))
    } else {
        bind_best(conn, id, &files, threshold)
    }
}

/// Binds a source to `platform` chosen by the user, matching its files against
/// that platform only; `None` unbinds it and forgets every match, hash proofs
/// included. A proof survives only a rebind to the platform of its rom. Keeps `disabled`.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn bind_to(conn: &Connection, id: SourceId, platform: Option<&PlatformId>) -> Result<()> {
    let files = rows::torrent_files(conn, id)?;
    rows::replace_files(conn, id, &files)?;
    let Some(platform) = platform else {
        rows::clear_matches(conn, id)?;
        rows::set_binding(conn, id, None, None)?;
        return keep_disabled(conn, id, SourceState::Unbound, None);
    };
    let hits = map_files(conn, id, platform, &files)?;
    // File counts are far below 2^52, so the rate is exact enough.
    #[allow(clippy::cast_precision_loss)]
    let rate = if files.is_empty() {
        0.0
    } else {
        hits as f64 / files.len() as f64
    };
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
            crate::client::prepare_download_dir(&local).await;
            let seed = rows::seed_from_text(&row.seed_policy).unwrap_or(SeedPolicy::None);
            match client
                .add(TorrentSource::Magnet(uri), &dir, &[], seed)
                .await
            {
                Ok(added) => {
                    // A paused magnet never fetches metadata; with nothing
                    // wanted, starting it transfers only the file list.
                    if let Err(e) = client.start(&added).await {
                        return note(app, &row, &unanswered(&e)).await;
                    }
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
        // Stop first: once metadata is in, the client wants every file.
        if let Err(e) = client.stop(&torrent).await {
            tracing::warn!(source = %id, error = %e, "cannot stop the resolved magnet");
        }
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
            .write_bulk(move |c| {
                let tx = c.transaction()?;
                let still = rows::get(&tx, id)?.is_some_and(|r| r.state == SourceState::Resolving);
                if !still {
                    return Ok(None);
                }
                rows::replace_files(&tx, id, &files)?;
                if let Some(r) = rows::get(&tx, id)? {
                    suggest(&tx, id, &r.origin_file, &r.display_name, &files)?;
                }
                bind_best(&tx, id, &files, threshold)?;
                let row = rows::get(&tx, id)?;
                crate::db::commit(tx)?;
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
        let result = crate::threads::blocking(crate::threads::label::SOURCE_WATCH, move || {
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

/// Enqueues a [`ResolveMagnet`] for resolving sources: every
/// [`crate::app::Options::magnet_started_poll`] for those already started in
/// the client, so a set torrent is stopped soon after its metadata arrives, and
/// every [`crate::app::Options::magnet_poll`] for the rest.
pub async fn resolve_pending(app: Arc<AppState>) {
    let slow = app.options.magnet_poll;
    let mut tick = tokio::time::interval(app.options.magnet_started_poll.min(slow));
    let mut last_slow: Option<tokio::time::Instant> = None;
    loop {
        let now = tick.tick().await;
        let pending = match app.db.read(rows::list_resolving).await {
            Ok(ids) => ids,
            Err(e) => {
                tracing::warn!(error = %e, "cannot list resolving sources");
                continue;
            }
        };
        let slow_due = last_slow.is_none_or(|t| now.duration_since(t) >= slow);
        if slow_due {
            last_slow = Some(now);
        }
        for (source_id, started) in pending {
            if !started && !slow_due {
                continue;
            }
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

    fn candidate_count(c: &Connection, id: SourceId) -> i64 {
        c.query_row(
            "SELECT COUNT(*) FROM torrent_candidates WHERE source_id = ?1",
            [id.0],
            |r| r.get(0),
        )
        .expect("count")
    }

    #[test]
    fn a_lone_rom_gets_fuzzy_and_size_candidates_and_unbinding_drops_them() {
        let (_dir, app) = state();
        app.db
            .write_blocking(|c| {
                let a = seed_rom(c, "nes", "Nova Quest (World).nes", 16, &[])?;
                let b = seed_rom(c, "nes", "Nova Quest (World) (Alt).nes", 16, &[])?;
                let nes = PlatformId("nes".into());
                let files = [file(0, "nova.nes", 16), file(1, "nova.png", 16)];
                let id = source(c, &"0e".repeat(20), &files);
                assert_eq!(map_files(c, id, &nes, &files)?, 0);
                let found = candidates::of_file(c, id, 0)?;
                assert_eq!(found, [(a, "fuzzy".into()), (b, "fuzzy".into())]);
                assert_eq!(rows::get(c, id)?.expect("row").matched_count, 1);

                let lone = [file(0, "rom.nes", 16)];
                let other = source(c, &"0f".repeat(20), &lone);
                bind_to(c, other, Some(&nes))?;
                assert_eq!(candidates::of_file(c, other, 0)?.len(), 2);
                assert!(candidates::of_file(c, other, 0)?
                    .iter()
                    .all(|(_, k)| k == "size"));
                bind_to(c, other, None)?;
                assert_eq!(candidate_count(c, other), 0);
                Ok(())
            })
            .expect("db");
    }

    #[test]
    fn a_set_of_same_sized_files_gets_no_size_only_candidates() {
        let (_dir, app) = state();
        app.db
            .write_blocking(|c| {
                for i in 0..6 {
                    seed_rom(c, "nes", &format!("Title {i} (World).nes"), 40_976, &[])?;
                }
                let files: Vec<TorrentFile> = (0..2_000)
                    .map(|i| file(i, &format!("Set/track {i}.nes"), 40_976))
                    .collect();
                let id = source(c, &"1a".repeat(20), &files);
                bind_to(c, id, Some(&PlatformId("nes".into())))?;
                assert_eq!(candidate_count(c, id), 0);
                Ok(())
            })
            .expect("db");
    }

    #[tokio::test]
    async fn a_dat_load_maps_its_bound_sources_again() {
        let (_dir, app) = state();
        let id = app
            .db
            .write_blocking(|c| {
                let files = [file(0, "nova.nes", 16), file(1, "a.txt", 1)];
                let id = source(c, &"1b".repeat(20), &files);
                bind_to(c, id, Some(&PlatformId("nes".into())))?;
                assert_eq!(candidate_count(c, id), 0);
                seed_rom(c, "nes", "Nova Quest (World).nes", 16, &[])?;
                Ok(id)
            })
            .expect("db");
        let mut events = app.events.subscribe(None).live;
        let nes = [PlatformId("nes".into())];
        assert_eq!(rebind_after_dat(&app, &nes).await.expect("rebind"), 0);
        let queued = app
            .db
            .read(|c| crate::db::jobs::count_kind(c, super::super::remap::KIND))
            .await;
        assert_eq!(queued.expect("count"), 1);
        let job = crate::jobs::remap::RemapSources {
            platforms: Some(nes.to_vec()),
        };
        Scheduler::run_inline(&app, Arc::new(job))
            .await
            .expect("run");
        let mut seen = false;
        while let Ok(ev) = events.try_recv() {
            seen |= ev.kind == EventKind::SourceChanged;
        }
        assert!(seen, "the mapped source is announced");
        let n = app.db.read(move |c| Ok(candidate_count(c, id))).await;
        assert_eq!(n.expect("count"), 1);
    }

    #[test]
    fn best_binding_and_manual_binding() {
        let (_dir, app) = state();
        app.db
            .write_blocking(|c| {
                seed_rom(c, "nes", "Example Quest (USA).nes", 16, &[])?;
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
    async fn a_named_set_is_suggested_and_bound_once_its_dat_loads() {
        let (_dir, app) = state();
        let files = [
            file(0, "Sets/Nintendo - Game Boy/Example Quest (USA).zip", 16),
            file(1, "Sets/Nintendo - Game Boy/Other Tale (USA).zip", 8),
        ];
        let id = app
            .db
            .write_blocking(|c| {
                let id = source(c, &"0c".repeat(20), &files);
                let guess = suggest(c, id, "pack.torrent", "Example Pack", &files)?;
                assert_eq!(guess, Some(PlatformId("gb".into())));
                bind_best(c, id, &files, 0.6)?;
                let row = rows::get(c, id)?.expect("row");
                assert_eq!(row.reason, Some(awaiting_dat_reason("Game Boy")));
                seed_rom(c, "gb", "Example Quest (USA).gb", 16, &[])?;
                Ok(id)
            })
            .expect("db");
        let mut events = app.events.subscribe(None).live;
        assert_eq!(rebind_after_dat(&app, &[]).await.expect("rebind"), 0);
        assert!(
            events.try_recv().is_err(),
            "an unchanged source is not announced"
        );
        let row = app.db.read(move |c| rows::get(c, id)).await.expect("get");
        assert_eq!(
            row.expect("row").state,
            SourceState::Unbound,
            "half is below 60%"
        );

        app.db
            .write_blocking(|c| seed_rom(c, "gb", "Other Tale (USA).gb", 8, &[]))
            .expect("seed");
        let other = app
            .db
            .write_blocking(|c| {
                let other = source(c, &"0d".repeat(20), &files);
                suggest(c, other, "pack2.torrent", "Example Pack", &files)?;
                bind_to(c, other, None)?;
                rows::set_user_unbound(c, other, true)?;
                Ok(other)
            })
            .expect("db");
        assert_eq!(rebind_after_dat(&app, &[]).await.expect("rebind"), 1);
        assert!(events.try_recv().is_ok(), "the bound source is announced");
        assert!(events.try_recv().is_err(), "only once");
        let row = app.db.read(move |c| rows::get(c, id)).await.expect("get");
        let row = row.expect("row");
        assert_eq!(row.platform_id, Some(PlatformId("gb".into())));
        assert_eq!((row.state, row.matched_count), (SourceState::Bound, 2));
        let kept = app
            .db
            .read(move |c| rows::get(c, other))
            .await
            .expect("get");
        assert_eq!(
            kept.expect("row").state,
            SourceState::Unbound,
            "the user unbound it"
        );
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

    #[tokio::test]
    async fn oversized_files_are_rejected_unread() {
        let (_dir, app) = state();
        let sources = app.config().paths.sources();
        std::fs::create_dir_all(&sources).expect("mkdir");
        let big = sources.join("big.torrent");
        let f = std::fs::File::create(&big).expect("create");
        f.set_len(MAX_SOURCE_BYTES + 1).expect("grow");
        drop(f);
        let job = Arc::new(SourceImport { path: big.clone() });
        Scheduler::run_inline(&app, job).await.expect("run");
        assert!(!big.exists());
        let reason = std::fs::read_to_string(sources.join("rejected/big.torrent.reason.txt"))
            .expect("reason");
        assert!(reason.starts_with(TOO_LARGE), "{reason}");
    }

    #[tokio::test]
    async fn reads_are_bounded_by_the_source_limit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("a.torrent");
        std::fs::write(&path, b"d4:infode").expect("write");
        let read = read_bounded(&path).await.expect("read");
        assert_eq!(read.as_deref(), Some(&b"d4:infode"[..]));
        let f = std::fs::File::create(&path).expect("create");
        f.set_len(MAX_SOURCE_BYTES).expect("grow");
        drop(f);
        let read = read_bounded(&path).await.expect("read").expect("fits");
        assert_eq!(read.len() as u64, MAX_SOURCE_BYTES);
        let missing = read_bounded(&dir.path().join("gone")).await;
        assert!(missing.is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound));
    }
}
