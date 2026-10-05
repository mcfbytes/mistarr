import { attempt } from './actions';
import { api } from './api';
import { incoming, type Watched } from './stores/incoming.svelte';
import { showToast } from './stores/toast.svelte';
import { addUpload } from './stores/uploads.svelte';
import { uploadNoun } from './status';
import type { IncomingFile } from './types';

function sentence(text: string): string {
  return /[.!?]$/.test(text) ? text : `${text}.`;
}

/** The toast for a file the server has received, saying what happens to it next. */
export function receivedText(up: Pick<IncomingFile, 'file' | 'state' | 'reason'>): string {
  const next = up.state === 'importing' ? 'Importing now.' : sentence(up.reason ?? 'Queued.');
  return `${uploadNoun(up.file)} received: ${up.file}. ${next}`;
}

/** Follows a received file as a session upload and says so; `since` is when its request began. */
export function received(which: Watched, up: IncomingFile, since: number): void {
  addUpload({ kind: which, file: up.file, jobId: up.job_id, reason: up.reason }, since);
  showToast(receivedText(up), 'info');
  incoming(which).reloadSoon();
}

/**
 * Uploads each file the input holds into `dats/` or `sources/`, says each was received
 * or why not, and follows it as a session upload. Clears the input after.
 */
export async function uploadFiles(which: Watched, input: HTMLInputElement | undefined): Promise<void> {
  const files = Array.from(input?.files ?? []);
  for (const file of files) {
    const since = Date.now();
    const up = await attempt(() => (which === 'dats' ? api.uploadDat(file) : api.uploadSource(file)), `${file.name}: `);
    if (up) {
      received(which, up, since);
    }
  }
  if (input) {
    input.value = '';
  }
}

/** Sends a magnet link; true when the server took it. */
export async function addMagnet(uri: string): Promise<boolean> {
  const since = Date.now();
  const up = await attempt(() => api.addMagnet(uri));
  if (up) {
    received('sources', up, since);
  }
  return up !== undefined;
}
