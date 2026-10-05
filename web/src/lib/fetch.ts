import { SvelteSet } from 'svelte/reactivity';
import { attempt } from './actions';
import { api } from './api';
import { followJob, type JobEnd } from './stores/jobs.svelte';
import { showToast } from './stores/toast.svelte';
import { received } from './upload';
import type { IncomingFile, Job } from './types';

/** Said once a fetch is queued; how it ends comes in a later toast. */
export const FETCH_QUEUED = 'Fetching the file. Its progress is under Background work.';

/** The error a fetch the user cancelled ends with. */
export const FETCH_CANCELLED = 'Cancelled.';

// Fetch tokens whose cancel was asked for and has not been refused.
const cancelling = new SvelteSet<number>();

/** Whether fetch `token`'s cancel is pending. */
export function isCancelling(token: number): boolean {
  return cancelling.has(token);
}

// Says how fetch job `end` ended: a placed file is followed as an upload, a failure is toasted.
function announceFetch(end: JobEnd): void {
  const progress = end.progress;
  if (end.state === 'done') {
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

// Follows URL fetch `token`, whose job is `jobId` once recorded; never the URL.
function followFetch(token: number, jobId: number | null): void {
  followJob((end) => end.kind === 'url_fetch' && (end.id === jobId || end.progress?.token === token), announceFetch);
}

/**
 * Sends a link the user typed: a magnet is placed at once, an http(s) URL is fetched once
 * in the background. The link is kept nowhere; true when the server took it.
 */
export async function addFromUrl(link: string): Promise<boolean> {
  const started = await attempt(() => api.fetchUrl(link));
  if (!started) {
    return false;
  }
  if (started.target && started.file) {
    received(started.target, started.file);
    return true;
  }
  if (started.token !== null) {
    followFetch(started.token, started.job_id);
  }
  showToast(FETCH_QUEUED, 'info');
  return true;
}

/** The token a fetch job is cancelled by, from its payload. */
export function fetchToken(job: Pick<Job, 'kind' | 'payload'>): number | null {
  return job.kind === 'url_fetch' && typeof job.payload.fetch === 'number' ? job.payload.fetch : null;
}

/** Asks the server to stop a queued or running fetch; its toast follows when it ends. */
export async function cancelFetch(job: Pick<Job, 'kind' | 'payload'>): Promise<void> {
  const token = fetchToken(job);
  if (token === null) {
    return;
  }
  cancelling.add(token);
  if ((await attempt(() => api.cancelFetch(token).then(() => true))) !== true) {
    cancelling.delete(token);
  }
}
