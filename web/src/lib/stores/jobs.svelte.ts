import { api } from '../api';
import { fixtureJobs } from '../fixtures';
import type { Job, JobKind } from '../types';

const isMock = import.meta.env.VITE_MOCK === '1';

let jobs = $state<Job[]>([]);

export function getJobs(): Job[] {
  return jobs;
}

export async function loadJobs(): Promise<void> {
  jobs = isMock ? fixtureJobs : (await api.jobs()).items;
}

export function applyJobProgress(id: number, kind: JobKind, progress: Record<string, unknown>): void {
  const exists = jobs.some((j) => j.id === id);
  if (exists) {
    jobs = jobs.map((j) => (j.id === id ? { ...j, progress, state: 'running' } : j));
  } else {
    jobs = [...jobs, { id, kind, state: 'running', progress, created_at: Date.now() / 1000, updated_at: Date.now() / 1000 }];
  }
}
