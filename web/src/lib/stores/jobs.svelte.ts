import { SvelteMap, SvelteSet } from 'svelte/reactivity';
import { api } from '../api';
import { fixtureJobs, fixtureRecentJobs, mockScenario } from '../fixtures';
import type { IncomingFile, Job, JobState } from '../types';
import { received } from '../upload';
import { findPlatform } from './platforms.svelte';
import { showToast } from './toast.svelte';
import { announceUpload, resolveUpload } from './uploads.svelte';

const isMock = import.meta.env.VITE_MOCK === '1';

/** What a finished job left behind, kept so an upload can show its outcome. */
export interface FinishedJob {
  kind: string;
  state: JobState;
  progress: Record<string, unknown> | null;
}

let jobs = $state<Job[]>([]);
let recent = $state<Job[]>([]);
// Insertion-ordered, so the oldest entry is evicted first.
const finished = new SvelteMap<number, FinishedJob>();
const FINISHED_KEEP = 50;
let reloadTimer: ReturnType<typeof setTimeout> | null = null;
let recentTimer: ReturnType<typeof setTimeout> | null = null;
// Pages showing the recent list; it is re-read only while one is open.
let recentWatchers = 0;
// Fetch tokens whose cancel was asked for and has not been refused.
const cancelling = new SvelteSet<number>();

/** Whether fetch `token`'s cancel is pending. */
export function isCancelling(token: number): boolean {
  return cancelling.has(token);
}

/** Marks fetch `token`'s cancel as pending, or no longer. */
export function markCancelling(token: number, pending: boolean): void {
  if (pending) {
    cancelling.add(token);
  } else {
    cancelling.delete(token);
  }
}

/** The error a fetch the user cancelled ends with. */
export const FETCH_CANCELLED = 'Cancelled.';
// URL fetches started in this tab, by token, with their job once known; never the URL.
// eslint-disable-next-line svelte/prefer-svelte-reactivity -- only event handlers read it, never markup
const pendingFetches = new Map<number, number | null>();
// Scans the user queued, by job id, until their outcome is shown; kept across pages.
// eslint-disable-next-line svelte/prefer-svelte-reactivity -- only event handlers read it, never markup
const pendingScans = new Map<number, string>();

export function getJobs(): Job[] {
  return jobs;
}

export function getFinishedJob(id: number): FinishedJob | undefined {
  return finished.get(id);
}

export async function loadJobs(): Promise<void> {
  if (isMock) {
    jobs = mockScenario() === 'busy' ? fixtureJobs : [];
    startMockProgress();
    return;
  }
  jobs = (await api.jobs()).items;
}

export function getRecentJobs(): Job[] {
  return recent;
}

export async function loadRecentJobs(): Promise<void> {
  if (isMock) {
    recent = mockScenario() === 'idle' ? [] : fixtureRecentJobs;
  } else {
    recent = (await api.recentJobs()).items;
  }
  for (const job of recent) {
    announce(job.id, job.state, job.progress);
  }
}

/** Keeps the recent list fresh while the caller is shown; returns the stop function. */
export function watchRecent(): () => void {
  recentWatchers += 1;
  void loadRecentJobs().catch(() => undefined);
  return () => {
    recentWatchers -= 1;
  };
}

/** Shows a toast with the outcome of scan `jobId` of `platformId` once it finishes. */
export function trackScan(jobId: number, platformId: string): void {
  pendingScans.set(jobId, platformId);
  const done = finished.get(jobId);
  if (done) {
    announce(jobId, done.state, done.progress);
  }
}

/** Follows URL fetch `token`, whose job is `jobId` once recorded, until a toast says how it ended. */
export function trackFetch(token: number, jobId: number | null): void {
  pendingFetches.set(token, jobId);
  const done = jobId === null ? undefined : finished.get(jobId);
  if (jobId !== null && done) {
    announceFetch(jobId, done.state, done.progress);
  }
}

// A URL fetch moved: its progress names its token, and once it ends a toast says how.
function announceFetch(id: number, state: JobState, progress: Record<string, unknown> | null): void {
  const token = typeof progress?.token === 'number' ? progress.token : null;
  if (token !== null && pendingFetches.get(token) === null) {
    pendingFetches.set(token, id);
  }
  if (state !== 'done' && state !== 'failed') {
    return;
  }
  const mine = [...pendingFetches].find(([t, j]) => j === id || t === token);
  if (!mine) {
    return;
  }
  pendingFetches.delete(mine[0]);
  if (state === 'done') {
    const target = progress?.target;
    const placed = progress?.placed;
    if ((target === 'dats' || target === 'sources') && placed && typeof placed === 'object') {
      received(target, placed as IncomingFile);
    }
    return;
  }
  const error = typeof progress?.error === 'string' ? progress.error : 'see Activity';
  if (error === FETCH_CANCELLED) {
    showToast('The fetch was cancelled.', 'info');
  } else {
    showToast(`The fetch failed: ${error}`, 'error');
  }
}

function platformName(id: string): string {
  return findPlatform(id)?.name ?? id;
}

function announce(id: number, state: JobState, progress: Record<string, unknown> | null): void {
  const platformId = pendingScans.get(id);
  if (platformId === undefined || (state !== 'done' && state !== 'failed')) {
    return;
  }
  pendingScans.delete(id);
  const text = jobOutcome({ kind: 'scan', state, progress, payload: { platform_id: platformId } }, platformName);
  showToast(text, state === 'done' ? 'success' : 'error');
}

// A source import named `file` ended; a DAT upload's outcome comes from `dat.loaded` or `dat.rejected`.
function announceSource(file: string, state: JobState, progress: Record<string, unknown> | null): void {
  if (state === 'failed') {
    const why = typeof progress?.error === 'string' ? progress.error : 'see Activity';
    announceUpload('sources', file, why);
  } else {
    announceUpload('sources', file, typeof progress?.rejected === 'string' ? progress.rejected : null);
  }
}

/** After a resync, re-reads the recent list when a page shows it or a scan awaits its outcome. */
export function resyncRecent(): Promise<void> {
  if (recentWatchers === 0 && pendingScans.size === 0) {
    return Promise.resolve();
  }
  return loadRecentJobs().catch(() => undefined);
}

// One re-read of the recent list per burst of finished jobs, while it is shown.
function scheduleRecent(): void {
  if (isMock || recentTimer || recentWatchers === 0) {
    return;
  }
  recentTimer = setTimeout(() => {
    recentTimer = null;
    void loadRecentJobs().catch(() => undefined);
  }, 500);
}

/** One line saying what a finished job did, such as "Scan of NES: 10 matched, 2 unmatched". */
export function jobOutcome(
  job: { kind: string; state: JobState; payload?: Record<string, unknown>; progress: Record<string, unknown> | null },
  platformName: (id: string) => string
): string {
  const p = job.progress ?? {};
  const pid = typeof job.payload?.platform_id === 'string' ? job.payload.platform_id : null;
  const where = pid ? platformName(pid) : 'every platform';
  const path = typeof job.payload?.path === 'string' ? job.payload.path : '';
  const file = path.split('/').pop() ?? '';
  if (job.kind === 'url_fetch') {
    const name = typeof p.file === 'string' ? ` of ${p.file}` : '';
    if (job.state === 'failed') {
      return typeof p.error === 'string' ? `URL fetch failed: ${p.error}` : 'URL fetch failed';
    }
    return typeof p.target === 'string' ? `URL fetch${name}: placed in ${p.target}/` : `URL fetch${name}: done`;
  }
  const source = typeof job.payload?.source_name === 'string' ? job.payload.source_name : '';
  const labels: Record<string, string> = {
    bind_source: `Binding of ${source}`.trim(),
    scan: `Scan of ${where}`,
    recompute_1g1r: `Matching for ${where}`,
    dat_import: `DAT ${file}`.trim(),
    source_import: `Source ${file}`.trim(),
    arcade_catalog: 'Arcade catalogue',
    import: 'Import',
    chd_tracks: 'CHD tracks'
  };
  const label = labels[job.kind] ?? job.kind;
  if (job.state === 'failed') {
    return typeof p.error === 'string' ? `${label} failed: ${p.error}` : `${label} failed`;
  }
  if (job.kind === 'scan' && typeof p.matched === 'number' && typeof p.unmatched === 'number') {
    const unidentified = typeof p.unidentified === 'number' && p.unidentified > 0 ? p.unidentified : 0;
    const tail = unidentified > 0 ? `, ${unidentified} not identified` : '';
    return `${label}: ${p.matched} matched, ${p.unmatched} unmatched${tail}`;
  }
  if (job.kind === 'chd_tracks' && typeof p.verified === 'number') {
    return `${label}: ${p.verified} verified, ${Number(p.unmatched ?? 0)} unmatched, ${Number(p.not_identified ?? 0)} not identified`;
  }
  if (job.kind === 'recompute_1g1r' && typeof p.matched === 'number') {
    return `${label}: ${p.matched} files newly matched`;
  }
  if (job.kind === 'bind_source' && typeof p.matched === 'number' && typeof p.total === 'number') {
    return `${label}: ${p.matched} of ${p.total} files matched`;
  }
  if (job.kind === 'dat_import' && typeof p.games === 'number') {
    return `${label}: ${p.games} games read`;
  }
  return `${label}: done`;
}

// After a resync the events that finished jobs may be lost; forget what is known.
export function resetFinished(): void {
  finished.clear();
}

// Lane and hold reason come only from /system/jobs, so a new or moved job re-reads it.
function scheduleReload(): void {
  if (isMock || reloadTimer) {
    return;
  }
  reloadTimer = setTimeout(() => {
    reloadTimer = null;
    void loadJobs().catch(() => undefined);
  }, 500);
}

/** Whether job `id` is known to be running, so its next progress needs no re-read. */
export function isRunning(id: number): boolean {
  return jobs.some((j) => j.id === id && j.state === 'running');
}

export function applyJobProgress(
  id: number,
  kind: string,
  state: JobState,
  progress: Record<string, unknown> | null,
  detail: string | null = null
): void {
  if (kind === 'url_fetch') {
    announceFetch(id, state, progress);
  }
  if (state === 'queued' && detail !== null && (kind === 'dat_import' || kind === 'source_import')) {
    resolveUpload(kind === 'dat_import' ? 'dats' : 'sources', detail, id);
  }
  if (state === 'done' || state === 'failed') {
    finished.delete(id);
    for (const old of finished.keys()) {
      if (finished.size < FINISHED_KEEP) {
        break;
      }
      finished.delete(old);
    }
    finished.set(id, { kind, state, progress });
    jobs = jobs.filter((j) => j.id !== id);
    announce(id, state, progress);
    if (kind === 'source_import' && detail !== null) {
      announceSource(detail, state, progress);
    }
    scheduleRecent();
    return;
  }
  const known = jobs.find((j) => j.id === id);
  if (known && known.state === state) {
    jobs = jobs.map((j) => (j.id === id ? { ...j, progress } : j));
  } else {
    scheduleReload();
  }
}

/** Mock mode: shows `job` running, then finishes it with `outcome` after `ms` and calls `done`. */
export function runMockJob(job: Job, outcome: Record<string, unknown>, ms: number, done: () => void): void {
  jobs = [...jobs, job];
  setTimeout(() => {
    applyJobProgress(job.id, job.kind, 'done', outcome);
    done();
  }, ms);
}

let mockTimer: ReturnType<typeof setInterval> | null = null;

// Mock mode moves the fixture DAT import through its phases so its bar is seen to move.
function startMockProgress(): void {
  if (mockTimer || jobs.length === 0) {
    return;
  }
  const total = 18_400_000;
  let read = 5_200_000;
  let games = 4_120;
  let tick = 0;
  mockTimer = setInterval(() => {
    const job = jobs.find((j) => j.kind === 'dat_import' && j.state === 'running');
    if (!job) {
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
    const bytes = phase === 'reading' ? { bytes_read: read, bytes_total: total } : {};
    const progress = { file: job.progress?.file, members: 1, done: 0, games, phase, ...bytes };
    applyJobProgress(job.id, job.kind, 'running', progress);
  }, 600);
}

let mockFetchId = 9000;
// eslint-disable-next-line svelte/prefer-svelte-reactivity -- only timers and handlers read it, never markup
const mockFetches = new Map<number, { id: number; timer: ReturnType<typeof setInterval> }>();

/** The refusal the server gives for anything but a DAT, DAT pack or torrent. */
export const NOT_ACCEPTED = "This isn't a DAT, DAT pack or torrent file.";

/** Mock mode: a fetch job that moves through its phases; a link to an HTML page is refused. */
export function startMockFetch(link: string, token: number): void {
  mockFetchId += 1;
  const id = mockFetchId;
  const segment = link.split(/[?#]/)[0]?.split('/').pop() ?? '';
  const refuse = /\.html?$/i.test(segment);
  // A link whose last segment starts with "wait" stays connecting until it is cancelled.
  const hold = /^wait/i.test(segment);
  const file = !segment ? 'download.dat' : /\.(dat|xml|zip|torrent)$/i.test(segment) ? segment : `${segment}.dat`;
  const total = 2_400_000;
  let got = 0;
  const now = Math.floor(Date.now() / 1000);
  jobs = [
    ...jobs,
    {
      id,
      kind: 'url_fetch',
      lane: 'fetch',
      payload: { fetch: token },
      state: 'running',
      progress: { token, phase: 'connecting', bytes_received: 0 },
      reason: null,
      created_at: now,
      updated_at: now
    }
  ];
  trackFetch(token, id);
  const timer = setInterval(() => {
    if (hold) {
      return;
    }
    got = Math.min(total, got + 300_000);
    if (refuse && got > 300_000) {
      stopMockFetch(token);
      applyJobProgress(id, 'url_fetch', 'failed', { error: NOT_ACCEPTED });
    } else if (got < total) {
      const known = refuse ? {} : { file };
      applyJobProgress(id, 'url_fetch', 'running', { token, phase: 'receiving', bytes_received: got, bytes_total: total, ...known });
    } else {
      stopMockFetch(token);
      const target = file.endsWith('.torrent') ? 'sources' : 'dats';
      const placed: IncomingFile = { file, size: total, state: 'waiting', reason: 'Queued.', job_id: null, progress: null, modified: now };
      applyJobProgress(id, 'url_fetch', 'done', { token, phase: 'placed', file, target, bytes_received: total, bytes_total: total, placed });
    }
  }, 600);
  mockFetches.set(token, { id, timer });
}

function stopMockFetch(token: number): number | null {
  const running = mockFetches.get(token);
  if (!running) {
    return null;
  }
  clearInterval(running.timer);
  mockFetches.delete(token);
  return running.id;
}

/** Mock mode: cancels fetch `token` as the server would. */
export function cancelMockFetch(token: number): void {
  const id = stopMockFetch(token);
  if (id !== null) {
    applyJobProgress(id, 'url_fetch', 'failed', { error: FETCH_CANCELLED });
  }
}
