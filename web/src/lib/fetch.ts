import { api, errorMessage } from './api';
import { markCancelling, trackFetch } from './stores/jobs.svelte';
import { showToast } from './stores/toast.svelte';
import { received } from './upload';
import type { Job } from './types';

/** Said once a fetch is queued; how it ends comes in a later toast. */
export const FETCH_QUEUED = 'Fetching the file. Its progress is under Background work.';

/**
 * Sends a link the user typed: a magnet is placed at once, an http(s) URL is fetched once
 * in the background. The link is kept nowhere; true when the server took it.
 */
export async function addFromUrl(link: string): Promise<boolean> {
  try {
    const started = await api.fetchUrl(link);
    if (started.target && started.file) {
      received(started.target, started.file);
      return true;
    }
    if (started.token !== null) {
      trackFetch(started.token, started.job_id);
    }
    showToast(FETCH_QUEUED, 'info');
    return true;
  } catch (err) {
    showToast(errorMessage(err), 'error');
    return false;
  }
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
  markCancelling(token, true);
  try {
    await api.cancelFetch(token);
  } catch (err) {
    markCancelling(token, false);
    showToast(errorMessage(err), 'error');
  }
}
