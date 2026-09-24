import { api, errorMessage } from './api';
import { scheduleIncoming, type Watched } from './stores/incoming.svelte';
import { showToast } from './stores/toast.svelte';
import { addUpload } from './stores/uploads.svelte';

const isMock = import.meta.env.VITE_MOCK === '1';

/**
 * Uploads each file the input holds into `dats/` or `sources/` and follows its import
 * as a session upload; the incoming list shows the outcome. Clears the input after.
 */
export async function uploadFiles(which: Watched, input: HTMLInputElement | undefined): Promise<void> {
  const files = Array.from(input?.files ?? []);
  if (isMock) {
    return;
  }
  for (const file of files) {
    try {
      const up = which === 'dats' ? await api.uploadDat(file) : await api.uploadSource(file);
      addUpload({ kind: which, file: up.file, jobId: up.job_id });
    } catch (err) {
      showToast(`${file.name}: ${errorMessage(err)}`);
    }
  }
  scheduleIncoming(which);
  if (input) {
    input.value = '';
  }
}
