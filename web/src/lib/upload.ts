import { api, errorMessage } from './api';
import { fixtureIncomingDats } from './fixtures';
import { patchIncoming, scheduleIncoming, type Watched } from './stores/incoming.svelte';
import { showToast } from './stores/toast.svelte';
import { addUpload } from './stores/uploads.svelte';
import { uploadNoun } from './status';
import type { IncomingFile } from './types';

const isMock = import.meta.env.VITE_MOCK === '1';

function sentence(text: string): string {
  return /[.!?]$/.test(text) ? text : `${text}.`;
}

/** The toast for a file the server has received, saying what happens to it next. */
export function receivedText(up: Pick<IncomingFile, 'file' | 'state' | 'reason'>): string {
  const next = up.state === 'importing' ? 'Importing now.' : sentence(up.reason ?? 'Queued.');
  return `${uploadNoun(up.file)} received: ${up.file}. ${next}`;
}

/** Follows a received file as a session upload and says so. */
export function received(which: Watched, up: IncomingFile): void {
  addUpload({ kind: which, file: up.file, jobId: up.job_id, reason: up.reason });
  showToast(receivedText(up), 'info');
  if (isMock) {
    patchIncoming(which, up.file, up);
  } else {
    scheduleIncoming(which);
  }
}

// Mock mode answers like a server whose writer is busy with the fixture's DAT import.
async function mockUpload(file: string): Promise<IncomingFile> {
  await new Promise((resolve) => setTimeout(resolve, 900));
  const importing = fixtureIncomingDats[0]?.file ?? 'a DAT';
  return {
    file,
    size: 1,
    state: 'waiting',
    reason: `Waiting for the DAT import of ${importing} to finish.`,
    job_id: null,
    progress: null,
    modified: 0
  };
}

/**
 * Uploads each file the input holds into `dats/` or `sources/`, says each was received
 * or why not, and follows it as a session upload. Clears the input after.
 */
export async function uploadFiles(which: Watched, input: HTMLInputElement | undefined): Promise<void> {
  const files = Array.from(input?.files ?? []);
  for (const file of files) {
    try {
      let up: IncomingFile;
      if (isMock) {
        up = await mockUpload(file.name);
      } else {
        up = which === 'dats' ? await api.uploadDat(file) : await api.uploadSource(file);
      }
      received(which, up);
    } catch (err) {
      showToast(`${file.name}: ${errorMessage(err)}`, 'error');
    }
  }
  if (input) {
    input.value = '';
  }
}

/** Sends a magnet link; true when the server took it. */
export async function addMagnet(uri: string): Promise<boolean> {
  try {
    const up = isMock ? await mockUpload('Example magnet.magnet') : await api.addMagnet(uri);
    received('sources', up);
    return true;
  } catch (err) {
    showToast(errorMessage(err), 'error');
    return false;
  }
}
