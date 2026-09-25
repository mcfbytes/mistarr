import { api } from '../api';
import { fixtureJobs, fixtureRecentJobs } from '../fixtures';
import type { Job, JobState } from '../types';

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

export function getJobs(): Job[] {
  return jobs;
}

export function getFinishedJob(id: number): FinishedJob | undefined {
  return finished[id];
}

export async function loadJobs(): Promise<void> {
  jobs = isMock ? fixtureJobs : (await api.jobs()).items;
}

export function getRecentJobs(): Job[] {
  return recent;
}

export async function loadRecentJobs(): Promise<void> {
  recent = isMock ? fixtureRecentJobs : (await api.recentJobs()).items;
}

// One re-read of the recent list per burst of finished jobs.
function scheduleRecent(): void {
  if (isMock || recentTimer) {
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
  const labels: Record<string, string> = {
    scan: `Scan of ${where}`,
    recompute_1g1r: `Matching for ${where}`,
    dat_import: `DAT ${path.split('/').pop() ?? ''}`.trim(),
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
    void loadJobs();
  }, 500);
}

export function applyJobProgress(
  id: number,
  kind: string,
  state: JobState,
  progress: Record<string, unknown> | null
): void {
  if (state === 'done' || state === 'failed') {
    const kept = Object.entries(finished).slice(-(FINISHED_KEEP - 1));
    finished = { ...Object.fromEntries(kept), [id]: { kind, state, progress } };
    jobs = jobs.filter((j) => j.id !== id);
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
