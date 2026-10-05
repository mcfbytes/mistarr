import { api } from '../api';
import { jobDetail } from '../status';
import type { Job, JobKind, JobState } from '../types';
import { ListStore } from './list.svelte';

/** How a job or a DAT file ended, kept so a later follower can still hear of it. */
export interface JobEnd {
  /** `null` for an ending that is no job's, such as a DAT file's. */
  id: number | null;
  kind: string;
  state: Extract<JobState, 'done' | 'failed'>;
  progress: Record<string, unknown> | null;
  /** The file a job is about, when its event named one. */
  detail: string | null;
  at: number;
}

/** The kind an ending is reported under when `dat.loaded` or `dat.rejected` names a DAT file. */
export const DAT_FILE = 'dat_file';

const ENDED_KEEP = 50;
const ENDED_HEARD_MS = 30_000;

/** The open jobs, from `/system/jobs`. */
export const jobs = new ListStore<Job>(
  (limit, offset) => api.jobs(limit, offset),
  (j) => j.id
);

/** The jobs that ended lately; each end is also told to the followers waiting for it. */
export const recent = new ListStore<Job>(
  (limit, offset) => api.recentJobs(limit, offset),
  (j) => j.id,
  {
    onLoaded: (items) => {
      for (const j of items) {
        if (j.state === 'done' || j.state === 'failed') {
          settle({ id: j.id, kind: j.kind, state: j.state, progress: j.progress, detail: null, at: Date.now() });
        }
      }
    }
  }
);

// Oldest first, so the oldest entry is evicted first.
let ended = $state.raw<JobEnd[]>([]);

interface Follower {
  match: (end: JobEnd) => boolean;
  onEnd: (end: JobEnd) => void;
}

// eslint-disable-next-line svelte/prefer-svelte-reactivity -- only event handlers read it, never markup
const followers = new Set<Follower>();
// Fetch tokens to job ids, from live progress; dropped when the job ends.
// eslint-disable-next-line svelte/prefer-svelte-reactivity -- only event handlers read it, never markup
const fetchJobs = new Map<number, number>();
// Pages showing the recent list; it is re-read only while one is open.
let recentWatchers = 0;

/** What job `id` left behind when it ended, if it has since the last resync. */
export function getFinishedJob(id: number): JobEnd | undefined {
  return ended.find((e) => e.id === id);
}

/** The job queued for `file` of `kind`: an open one, else the newest that ended at or after `since`. */
export function jobIdFor(kind: JobKind, file: string, since: number): number | null {
  const open = jobs.items.find((j) => j.kind === kind && jobDetail(j.payload) === file);
  const late = [...ended].reverse().find((e) => e.at >= since && e.kind === kind && e.detail === file);
  return open?.id ?? late?.id ?? null;
}

/** The job of URL fetch `token`, once a progress event has named both. */
export function fetchJobId(token: number): number | null {
  return fetchJobs.get(token) ?? null;
}

/**
 * Calls `onEnd` once, with the first ending `match` accepts, among those heard since
 * `since` (default: the last half minute) or still to come; returns the function that stops following.
 */
export function followJob(
  match: (end: JobEnd) => boolean,
  onEnd: (end: JobEnd) => void,
  since = Date.now() - ENDED_HEARD_MS
): () => void {
  const heard = ended.find((e) => e.at >= since && match(e));
  if (heard) {
    onEnd(heard);
    return () => undefined;
  }
  const follower = { match, onEnd };
  followers.add(follower);
  return () => followers.delete(follower);
}

function settle(end: JobEnd): void {
  for (const f of [...followers]) {
    if (f.match(end)) {
      followers.delete(f);
      f.onEnd(end);
    }
  }
}

/** Records an ending and tells the followers waiting for it. */
export function reportEnd(end: Omit<JobEnd, 'at'>): void {
  const entry = { ...end, at: Date.now() };
  ended = [...ended.filter((e) => e.id === null || e.id !== end.id), entry].slice(-ENDED_KEEP);
  settle(entry);
}

/** Keeps the recent list fresh while the caller is shown; returns the stop function. */
export function watchRecent(): () => void {
  recentWatchers += 1;
  void recent.load();
  return () => {
    recentWatchers -= 1;
  };
}

/** After a resync, re-reads the recent list when a page shows it or a follower awaits an ending. */
export function resyncRecent(): Promise<void> {
  return recentWatchers === 0 && followers.size === 0 ? Promise.resolve() : recent.load();
}

// After a resync the events that finished jobs may be lost; forget what is known.
export function resetFinished(): void {
  ended = [];
  fetchJobs.clear();
}

/** Whether job `id` is known to be running, so its next progress needs no re-read. */
export function isRunning(id: number): boolean {
  return jobs.items.some((j) => j.id === id && j.state === 'running');
}

export function applyJobProgress(
  id: number,
  kind: string,
  state: JobState,
  progress: Record<string, unknown> | null,
  detail: string | null = null
): void {
  if (kind === 'url_fetch' && typeof progress?.token === 'number') {
    fetchJobs.set(progress.token, id);
  }
  if (state === 'done' || state === 'failed') {
    jobs.patch(id, null);
    reportEnd({ id, kind, state, progress, detail });
    for (const [token, job] of fetchJobs) {
      if (job === id) {
        fetchJobs.delete(token);
      }
    }
    if (recentWatchers > 0) {
      recent.reloadSoon();
    }
    return;
  }
  // Lane and hold reason come only from /system/jobs, so a new or moved job re-reads it.
  if (jobs.items.some((j) => j.id === id && j.state === state)) {
    jobs.patch(id, { progress });
  } else {
    jobs.reloadSoon();
  }
}
