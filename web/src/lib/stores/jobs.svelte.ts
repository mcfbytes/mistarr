import { api } from '../api';
import { fixtureJobs, fixtureRecentJobs, mockScenario } from '../fixtures';
import type { Job, JobState } from '../types';
import { findPlatform } from './platforms.svelte';
import { showToast } from './toast.svelte';
import { announceUpload, findUpload, resolveUpload } from './uploads.svelte';

const isMock = import.meta.env.VITE_MOCK === '1';

/** What a finished job left behind, kept so an upload can show its outcome. */
export interface FinishedJob {
  kind: string;
  state: JobState;
  progress: Record<string, unknown> | null;
}

let jobs = $state<Job[]>([]);
let recent = $state<Job[]>([]);
let finished = $state<Record<number, FinishedJob>>({});
const FINISHED_KEEP = 50;
let reloadTimer: ReturnType<typeof setTimeout> | null = null;
let recentTimer: ReturnType<typeof setTimeout> | null = null;
// Pages showing the recent list; it is re-read only while one is open.
let recentWatchers = 0;
// Scans the user queued, by job id, until their outcome is shown; kept across pages.
let pendingScans: Record<number, string> = {};

export function getJobs(): Job[] {
  return jobs;
}

export function getFinishedJob(id: number): FinishedJob | undefined {
  return finished[id];
}

export async function loadJobs(): Promise<void> {
  if (isMock) {
    jobs = mockScenario() === 'idle' ? [] : fixtureJobs;
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
  pendingScans = { ...pendingScans, [jobId]: platformId };
  const done = finished[jobId];
  if (done) {
    announce(jobId, done.state, done.progress);
  }
}

function platformName(id: string): string {
  return findPlatform(id)?.name ?? id;
}

function announce(id: number, state: JobState, progress: Record<string, unknown> | null): void {
  const platformId = pendingScans[id];
  if (platformId === undefined || (state !== 'done' && state !== 'failed')) {
    return;
  }
  pendingScans = Object.fromEntries(Object.entries(pendingScans).filter(([k]) => Number(k) !== id));
  const text = jobOutcome({ kind: 'scan', state, progress, payload: { platform_id: platformId } }, platformName);
  showToast(text, state === 'done' ? 'success' : 'error');
}

// A source upload's import ended; a DAT upload's outcome comes from `dat.loaded` or `dat.rejected`.
function announceSource(id: number, state: JobState, progress: Record<string, unknown> | null): void {
  const up = findUpload(id);
  if (up?.kind !== 'sources') {
    return;
  }
  if (state === 'failed') {
    const why = typeof progress?.error === 'string' ? progress.error : 'see Activity';
    announceUpload('sources', up.file, why);
  } else {
    announceUpload('sources', up.file, typeof progress?.rejected === 'string' ? progress.rejected : null);
  }
}

/** After a resync, re-reads the recent list when a page shows it or a scan awaits its outcome. */
export function resyncRecent(): Promise<void> {
  if (recentWatchers === 0 && Object.keys(pendingScans).length === 0) {
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
  const labels: Record<string, string> = {
    scan: `Scan of ${where}`,
    recompute_1g1r: `Matching for ${where}`,
    dat_import: `DAT ${file}`.trim(),
    source_import: `Source ${file}`.trim(),
    arcade_catalog: 'Arcade catalogue',
    import: 'Import'
  };
  const label = labels[job.kind] ?? job.kind;
  if (job.state === 'failed') {
    return typeof p.error === 'string' ? `${label} failed: ${p.error}` : `${label} failed`;
  }
  if (job.kind === 'scan' && typeof p.matched === 'number' && typeof p.unmatched === 'number') {
    return `${label}: ${p.matched} matched, ${p.unmatched} unmatched`;
  }
  if (job.kind === 'recompute_1g1r' && typeof p.matched === 'number') {
    return `${label}: ${p.matched} files newly matched`;
  }
  if (job.kind === 'dat_import' && typeof p.games === 'number') {
    return `${label}: ${p.games} games read`;
  }
  return `${label}: done`;
}

// After a resync the events that finished jobs may be lost; forget what is known.
export function resetFinished(): void {
  finished = {};
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
  if (state === 'queued' && detail !== null && (kind === 'dat_import' || kind === 'source_import')) {
    resolveUpload(kind === 'dat_import' ? 'dats' : 'sources', detail, id);
  }
  if (state === 'done' || state === 'failed') {
    const kept = Object.entries(finished).slice(-(FINISHED_KEEP - 1));
    finished = { ...Object.fromEntries(kept), [id]: { kind, state, progress } };
    jobs = jobs.filter((j) => j.id !== id);
    announce(id, state, progress);
    announceSource(id, state, progress);
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
