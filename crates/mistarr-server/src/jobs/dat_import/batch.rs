//! Several queued DAT imports applied on one copy of the database in RAM; the rules
//! are `docs/ARCHITECTURE.md` "DAT import in RAM".

use std::path::PathBuf;
use std::time::Instant;

use mistarr_sources::intake::{self, LOADED_DIR};
use serde_json::Value;

use super::stream::{disk_full, import_from};
use super::{
    file_name, follow_up_load, list_members, loaded_platforms, log_report, members_size,
    recompute_platforms, room, settle, store, DatImport, Member, Meter, Outcome, RamWatch, Request,
};
use crate::db::dat_stage;
use crate::db::ids::JobId;
use crate::db::jobs::{self as rows, JobState};
use crate::db::ram::{self, Ram};
use crate::db::Db;
use crate::error::{Error, Result};
use crate::jobs::progress::{Progress, Reporter};
use crate::jobs::{conclude, context, JobContext, JobKind, Lane};

/// One file of a batch, listed and planned before the copy is made.
#[derive(Clone)]
pub(super) struct Item {
    pub(super) job: DatImport,
    pub(super) members: Vec<Member>,
    /// Where the file goes in `dats/loaded/`; its name is the stored source file.
    pub(super) target: PathBuf,
    /// Uncompressed bytes of the members.
    pub(super) size: u64,
}

impl Item {
    fn file(&self) -> String {
        file_name(&self.target)
    }
}

/// How one file of a batch ended on the copy.
#[derive(Debug)]
pub(super) enum End {
    /// Every member was read; their outcomes.
    Imported(Vec<Outcome>),
    /// The file's own job stopped or failed; nothing of it is in the copy.
    Failed(Error),
}

/// One file as the work on the copy sees it.
pub(super) struct Entry {
    pub(super) id: JobId,
    pub(super) file: String,
    pub(super) path: PathBuf,
    pub(super) members: Vec<Member>,
    pub(super) req: Request,
}

/// Whether `e` ends only the file that raised it: not the memory, the lane, the server
/// or the copy, which end the whole batch.
fn local(e: &Error) -> bool {
    !(matches!(
        e,
        Error::Cancelled | Error::Paused | Error::NoRoom(_) | Error::Reopen(_)
    ) || disk_full(e))
}

/// Imports the members of each of `entries` in turn on the copy `db`, each file failing
/// alone, then recomputes once each platform that any loaded into; `stored` is told of
/// each member done. Returns how each file ended and whether the copy must be written
/// back. Stops with the error that ends the whole batch.
pub(super) fn import_batch(
    db: &Db,
    entries: &[Entry],
    stored: &mut dyn FnMut(&Db, &Entry, usize, u64) -> Result<()>,
) -> Result<(Vec<End>, bool)> {
    let mut ends = Vec::with_capacity(entries.len());
    for (i, entry) in entries.iter().enumerate() {
        let end = match import_entry(db, entry, i == 0, stored) {
            Ok(outcomes) => End::Imported(outcomes),
            Err(e) if local(&e) => {
                if let Err(c) = db.write_blocking(|c| dat_stage::clear(c)) {
                    tracing::warn!(error = %c, "cannot empty the DAT stage");
                }
                End::Failed(e)
            }
            Err(e) => return Err(e),
        };
        ends.push(end);
    }
    let platforms = loaded_platforms(ends.iter().flat_map(|e| match e {
        End::Imported(o) => o.as_slice(),
        End::Failed(_) => &[],
    }));
    if let Some(first) = entries.first() {
        let stop = || halt(&first.req);
        recompute_platforms(
            db,
            &platforms,
            &first.req.prefs,
            &stop,
            first.req.meter.as_ref(),
        )?;
    }
    let any = ends.iter().any(
        |e| matches!(e, End::Imported(o) if o.iter().any(|o| matches!(o, Outcome::Loaded(_)))),
    );
    Ok((ends, any))
}

fn import_entry(
    db: &Db,
    entry: &Entry,
    first: bool,
    stored: &mut dyn FnMut(&Db, &Entry, usize, u64) -> Result<()>,
) -> Result<Vec<Outcome>> {
    let req = &entry.req;
    let mut outcomes = Vec::with_capacity(entry.members.len());
    let mut games = 0;
    for (done, &member) in entry.members.iter().enumerate() {
        req.stop.check()?;
        // The first member of the batch follows the check that allowed the copy.
        if !(first && done == 0) {
            room(req)?;
        }
        if let Some(m) = &req.meter {
            m.at_member(done, games);
        }
        let outcome = import_from(db, &entry.path, member, req)?;
        if let Outcome::Loaded(l) = &outcome {
            games += l.games;
        }
        outcomes.push(outcome);
        stored(db, entry, done + 1, games)?;
    }
    Ok(outcomes)
}

/// [`Request::stop`]'s check, except that the cancel of the file's own job does not stop
/// the work shared by the batch.
pub(super) fn halt(req: &Request) -> Result<()> {
    match req.stop.stopped() {
        Ok(()) | Err(Error::CancelledByUser) => {}
        Err(e) => return Err(e),
    }
    if req.stop.held().is_some() {
        return Err(Error::Paused);
    }
    room(req)
}

/// A job of the batch other than the one running, for the phases of the copy.
struct Tracked {
    reporter: Reporter,
    file: String,
    members: usize,
}

/// The lead job's [`RamWatch`], and the phases of the copy and the write-back reported
/// on every other job of the batch too.
struct BatchWatch {
    lead: RamWatch,
    others: Vec<Tracked>,
}

impl ram::Watch for BatchWatch {
    fn phase(&mut self, phase: ram::Phase) {
        ram::Watch::phase(&mut self.lead, phase);
        if phase == ram::Phase::Importing {
            return;
        }
        for o in &self.others {
            o.reporter.report(phase.label(), || {
                Progress::phase(phase.label())
                    .with("file", &o.file)
                    .with("members", o.members)
            });
        }
    }

    fn between(&mut self, phase: ram::Phase) -> Result<()> {
        ram::Watch::between(&mut self.lead, phase)
    }
}

/// How a batch ended for the job that leads it.
pub(super) enum Ran {
    /// The batch ran; the lead job's own result.
    Ended(Result<()>),
    /// The copy could not hold the batch; the followers are queued again.
    Alone,
}

impl DatImport {
    /// Takes the queued DAT imports that can share this one's copy: the readable ones, in
    /// queue order, as many as [`ram::fit_now`] lets the sum of their sizes. Each is marked
    /// running and skipped by its lane. Sets `lead.size`.
    pub(super) async fn take_batch(
        &self,
        ctx: &JobContext,
        lead: &mut Item,
    ) -> Result<Vec<(Item, JobContext)>> {
        let app = &ctx.app;
        let queued = app
            .db
            .read(|c| rows::queued_of_kind(c, JobKind::DatImport))
            .await?;
        let candidates: Vec<(JobId, DatImport)> = queued
            .into_iter()
            .filter(|r| r.id != ctx.id && !app.scheduler.cancelled(r.id))
            .filter_map(|r| {
                let job = DatImport::from_payload(&r.payload).filter(|j| !j.binds())?;
                Some((r.id, job))
            })
            .collect();
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        let config = app.config();
        let floor = config.memory.import_floor_mib.saturating_mul(1024 * 1024);
        let (dir, db) = (
            config.memory.import_dir.clone(),
            app.db.path().to_path_buf(),
        );
        let (path, members) = (self.path.clone(), lead.members.clone());
        let (size, listed) = crate::threads::run(crate::threads::label::DAT_IMPORT, move || {
            let listed: Vec<(JobId, Item)> = candidates.into_iter().filter_map(prepare).collect();
            let mut sizes = vec![members_size(&path, &members)];
            sizes.extend(listed.iter().map(|(_, i)| i.size));
            let fit = ram::fit_now(&dir, &db, floor, &sizes).unwrap_or(1);
            (
                sizes[0],
                listed
                    .into_iter()
                    .take(fit.saturating_sub(1))
                    .collect::<Vec<_>>(),
            )
        })
        .await?;
        lead.size = size;
        let mut taken = Vec::with_capacity(listed.len());
        for (id, item) in listed {
            app.scheduler.take(id);
            let now = crate::unix_now();
            match app.db.write(move |c| rows::claim_queued(c, id, now)).await {
                Ok(true) => {}
                Ok(false) => {
                    app.scheduler.untake(id);
                    continue;
                }
                Err(e) => {
                    app.scheduler.untake(id);
                    tracing::warn!(job = %id, error = %e, "cannot take a queued DAT import");
                    continue;
                }
            }
            let cancel = app.scheduler.open(id);
            let follower = context(app, id, &item.job, Lane::Background, cancel);
            taken.push((item, follower));
        }
        Ok(taken)
    }
}

/// The file of a queued DAT import, listed and planned, or `None` when it is gone or not
/// a DAT, which its own turn rejects.
fn prepare((id, job): (JobId, DatImport)) -> Option<(JobId, Item)> {
    if !job.path.is_file() {
        return None;
    }
    let members = list_members(&job.path).ok()?;
    let target = intake::plan(&job.path, LOADED_DIR).ok()?;
    let size = members_size(&job.path, &members);
    Some((
        id,
        Item {
            job,
            members,
            target,
            size,
        },
    ))
}

/// Runs the lead's file and its followers' on one copy in RAM, settles every file and
/// ends the followers' jobs. `Alone` when the copy was not used; the followers are then
/// queued again.
pub(super) async fn run(
    ctx: &JobContext,
    lead: Item,
    followers: Vec<(Item, JobContext)>,
) -> Result<Ran> {
    let mut items = vec![lead];
    let mut contexts = Vec::with_capacity(followers.len());
    for (item, follower) in followers {
        items.push(item);
        contexts.push(follower);
    }
    let all: Vec<&JobContext> = std::iter::once(ctx).chain(contexts.iter()).collect();
    let ends = match attempt(&items, &all).await {
        Ok(Some(ends)) => ends,
        outcome => {
            release(&contexts).await;
            return outcome.map(|_| Ran::Alone);
        }
    };
    let mut loaded = Vec::new();
    let mut results = Vec::with_capacity(ends.len());
    for ((item, job), end) in items.iter().zip(&all).zip(ends) {
        let file = file_name(&item.job.path);
        results.push(match end {
            End::Imported(outcomes) => settle(
                &job.app,
                &item.job.path,
                &file,
                item.target.clone(),
                outcomes,
            )
            .await
            .map(|l| loaded.extend(l)),
            End::Failed(e) => Err(e),
        });
    }
    if !loaded.is_empty() {
        follow_up_load(&ctx.app, &loaded, true).await;
    }
    let mut results = results.into_iter();
    let lead_result = results.next().unwrap_or(Ok(()));
    for (follower, result) in contexts.iter().zip(results) {
        if let Err(e) = conclude(&ctx.app, follower, result).await {
            tracing::warn!(job = %follower.id, error = %e, "cannot record job state");
        }
    }
    Ok(Ran::Ended(lead_result))
}

/// One try per pause on the copy: `None` when the copy could not be used.
async fn attempt(items: &[Item], all: &[&JobContext]) -> Result<Option<Vec<End>>> {
    loop {
        match in_ram(items, all).await {
            Ok(Ram::Done(ends, report)) => {
                log_report(&items[0].file(), &report);
                return Ok(Some(ends));
            }
            Ok(Ram::Fallback(why)) => {
                tracing::info!(
                    files = items.len(),
                    reason = why.summary,
                    detail = why.detail,
                    "DAT batch not imported in RAM"
                );
                return Ok(None);
            }
            Err(Error::Paused) => {
                mark(&all[1..], JobState::Paused).await;
                let waited = all[0].checkpoint().await;
                mark(&all[1..], JobState::Running).await;
                waited?;
            }
            Err(e) => return Err(e),
        }
    }
}

async fn mark(jobs: &[&JobContext], state: JobState) {
    for job in jobs {
        let (id, now) = (job.id, crate::unix_now());
        let set = job
            .app
            .db
            .write(move |c| rows::set_state(c, id, state, now));
        if let Err(e) = set.await {
            tracing::warn!(job = %id, error = %e, "cannot record job state");
        }
    }
}

/// Puts the followers back in the queue, to run on their lane's next turns.
async fn release(followers: &[JobContext]) {
    for job in followers {
        let (id, now) = (job.id, crate::unix_now());
        let set = job
            .app
            .db
            .write(move |c| rows::requeue(c, id, Lane::Background, now));
        if let Err(e) = set.await {
            tracing::warn!(job = %id, error = %e, "cannot queue a DAT import again");
        }
        job.app.live.clear(id);
        job.app.scheduler.untake(id);
        job.publish(JobState::Queued, &Value::Null);
    }
}

/// Holds the writer and imports every file on a copy in RAM through [`ram::run`].
async fn in_ram(items: &[Item], all: &[&JobContext]) -> Result<Ram<Vec<End>>> {
    let lead = all[0];
    let config = lead.app.config();
    let floor = config.memory.import_floor_mib.saturating_mul(1024 * 1024);
    let plan = ram::Plan {
        dir: config.memory.import_dir.clone(),
        floor,
        job: Some(lead.id),
        input: items.iter().map(|i| i.size).sum(),
    };
    let mut entries = Vec::with_capacity(items.len());
    let mut others = Vec::new();
    for (index, (item, job)) in items.iter().zip(all).enumerate() {
        let file = item.file();
        let meter = Meter::new(job.reporter(), &file, item.members.len(), None);
        let req = item.job.request(job, &file, Some(floor), Some(meter));
        if index > 0 {
            others.push(Tracked {
                reporter: job.reporter(),
                file: file.clone(),
                members: item.members.len(),
            });
        }
        entries.push(Entry {
            id: job.id,
            file,
            path: item.job.path.clone(),
            members: item.members.clone(),
            req,
        });
    }
    let mut watch = BatchWatch {
        lead: RamWatch {
            reporter: lead.reporter(),
            id: lead.id,
            file: entries[0].file.clone(),
            members: entries[0].members.len(),
            stop: lead.stop.clone(),
            chunk_started: Instant::now(),
        },
        others,
    };
    lead.app
        .db
        .hold_writer(crate::threads::label::DAT_IMPORT, move |held| {
            ram::run(held, &plan, &mut watch, |db| {
                import_batch(db, &entries, &mut |db, e, done, games| {
                    store(db, e.id, &e.file, e.members.len(), done, games)
                })
            })
        })
        .await
}
