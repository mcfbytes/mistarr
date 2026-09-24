import { api } from '../api';
import { fixtureJobs } from '../fixtures';
import type { Job, JobState } from '../types';

const isMock = import.meta.env.VITE_MOCK === '1';

/** What a finished job left behind, kept so an upload can show its outcome. */
export interface FinishedJob {
  kind: string;
  state: JobState;
  progress: Record<string, unknown> | null;
}

let jobs = $state<Job[]>([]);
let finished = $state<Record<number, FinishedJob>>({});
let reloadTimer: ReturnType<typeof setTimeout> | null = null;

export function getJobs(): Job[] {
  return jobs;
}

export function getFinishedJob(id: number): FinishedJob | undefined {
  return finished[id];
}

export async function loadJobs(): Promise<void> {
  jobs = isMock ? fixtureJobs : (await api.jobs()).items;
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
    finished = { ...finished, [id]: { kind, state, progress } };
    jobs = jobs.filter((j) => j.id !== id);
    return;
  }
  const known = jobs.find((j) => j.id === id);
  if (known && known.state === state) {
    jobs = jobs.map((j) => (j.id === id ? { ...j, progress } : j));
  } else {
    scheduleReload();
  }
}
