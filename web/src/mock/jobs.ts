import { FETCH_CANCELLED } from '../lib/fetch';
import type { IncomingFile, Job, JobState } from '../lib/types';
import { emit, streamOpen } from './events';
import { mock, nextJobId, nowSecs } from './state';

/** The refusal the server gives for anything but a DAT, DAT pack or torrent. */
export const NOT_ACCEPTED = "This isn't a DAT, DAT pack or torrent file.";

function announce(job: Pick<Job, 'id' | 'kind'>, state: JobState, progress: Record<string, unknown> | null): void {
  emit({ name: 'job.progress', data: { id: job.id, kind: job.kind, state, progress, detail: null } });
}

/** Lists `job` as open and says so. */
export function openJob(job: Job): void {
  mock().jobs.push(job);
  announce(job, job.state, job.progress);
}

/** Sets open job `id`'s progress and says so. */
function update(id: number, progress: Record<string, unknown>): void {
  const s = mock();
  const job = s.jobs.find((j) => j.id === id);
  if (job) {
    job.progress = progress;
    job.updated_at = nowSecs();
    announce(job, job.state, progress);
  }
}

/** Moves open job `id` to the recent list, ended in `state` with `progress`, and says so. */
function finish(id: number, state: 'done' | 'failed', progress: Record<string, unknown>): void {
  const s = mock();
  const job = s.jobs.find((j) => j.id === id);
  if (!job) {
    return;
  }
  s.jobs = s.jobs.filter((j) => j !== job);
  s.recent.unshift({ ...job, state, progress, updated_at: nowSecs() });
  announce(job, state, progress);
}

/** Runs `job` for `ms`, then makes its change with `apply` and finishes it with `outcome`. */
export function runJob(job: Job, outcome: Record<string, unknown>, ms: number, apply: () => void): void {
  openJob(job);
  setTimeout(() => {
    apply();
    finish(job.id, 'done', outcome);
  }, ms);
}

let ticker: ReturnType<typeof setInterval> | null = null;

/** Moves the running fixture DAT import through its phases while a stream is open, so its bar is seen to move. */
export function startProgress(): void {
  if (ticker) {
    return;
  }
  const total = 18_400_000;
  let read = 5_200_000;
  let games = 4_120;
  let tick = 0;
  ticker = setInterval(() => {
    const job = mock().jobs.find((j) => j.kind === 'dat_import' && j.state === 'running');
    if (!job || !streamOpen()) {
      return;
    }
    tick += 1;
    let phase = 'reading';
    if (read < total) {
      read = Math.min(total, read + 460_000);
      games += 104;
    } else {
      phase = tick % 12 < 6 ? 'storing' : 'refreshing';
      if (tick % 12 === 11) {
        read = 1_000_000;
        games = 900;
      }
    }
    const bytes = phase === 'reading' ? { bytes: read, bytes_total: total } : {};
    update(job.id, { file: job.progress?.file, members: 1, done: 0, games, phase, ...bytes });
  }, 600);
}

let lastToken = 0;
const fetches = new Map<number, { id: number; timer: ReturnType<typeof setInterval> }>();

/**
 * Starts a fetch job for `link` as `POST /fetch` does, returning its token and job id.
 * A link to an HTML page is refused partway; one whose last segment starts with
 * "wait" stays connecting until it is cancelled.
 */
export function startFetch(link: string): { token: number; id: number } {
  lastToken += 1;
  const token = lastToken;
  const id = nextJobId();
  const segment = link.split(/[?#]/)[0]?.split('/').pop() ?? '';
  const refuse = /\.html?$/i.test(segment);
  const hold = /^wait/i.test(segment);
  const file = !segment ? 'download.dat' : /\.(dat|xml|zip|torrent)$/i.test(segment) ? segment : `${segment}.dat`;
  const total = 2_400_000;
  let got = 0;
  const now = nowSecs();
  openJob({
    id,
    kind: 'url_fetch',
    lane: 'fetch',
    payload: { fetch: token },
    state: 'running',
    progress: { token, phase: 'connecting', bytes: 0 },
    reason: null,
    created_at: now,
    updated_at: now
  });
  const timer = setInterval(() => {
    if (hold) {
      return;
    }
    got = Math.min(total, got + 300_000);
    if (refuse && got > 300_000) {
      stopFetch(token);
      finish(id, 'failed', { error: NOT_ACCEPTED });
    } else if (got < total) {
      const known = refuse ? {} : { file };
      update(id, { token, phase: 'receiving', bytes: got, bytes_total: total, ...known });
    } else {
      stopFetch(token);
      const target = file.endsWith('.torrent') ? 'sources' : 'dats';
      const placed: IncomingFile = { file, size: total, state: 'waiting', reason: 'Queued.', job_id: null, progress: null, modified: now };
      mock().incoming[target].unshift(placed);
      finish(id, 'done', { token, phase: 'placed', file, target, bytes: total, bytes_total: total, placed });
    }
  }, 600);
  fetches.set(token, { id, timer });
  return { token, id };
}

function stopFetch(token: number): number | null {
  const running = fetches.get(token);
  if (!running) {
    return null;
  }
  clearInterval(running.timer);
  fetches.delete(token);
  return running.id;
}

/** Cancels fetch `token` as `DELETE /fetch/{token}` does. */
export function cancelFetch(token: number): void {
  const id = stopFetch(token);
  if (id !== null) {
    finish(id, 'failed', { error: FETCH_CANCELLED });
  }
}

/** Kinds whose payload names a platform, so runs of different platforms stay apart. */
const PER_PLATFORM = new Set(['scan', 'recompute_1g1r']);

/** Whether `next` continues the run whose newest row is `last`. */
function sameRun(last: Job, next: Job): boolean {
  return (
    last.kind === next.kind &&
    last.state === next.state &&
    (!PER_PLATFORM.has(next.kind) || last.payload.platform_id === next.payload.platform_id)
  );
}

/** `recent`, newest first, runs of one kind and outcome folded as `/system/jobs/recent` folds them. */
export function foldRecent(recent: readonly Job[]): Job[] {
  const folded: Job[] = [];
  for (const row of recent) {
    const last = folded[folded.length - 1];
    if (last && sameRun(last, row)) {
      last.count = (last.count ?? 1) + 1;
      last.first_updated_at = row.updated_at;
    } else {
      folded.push({ ...row, count: 1, first_updated_at: row.updated_at });
    }
  }
  return folded;
}
