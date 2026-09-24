import { EventSubscriber } from '../api';
import type { SseEvent } from '../types';
import { applyStatus, loadStatus, loadWizard, setConnected } from './status.svelte';
import { applySourceChanged, loadSources } from './sources.svelte';
import { applyDownloadChanged, loadDownloads, loadImports } from './downloads.svelte';
import { applyJobProgress, loadJobs } from './jobs.svelte';
import { applyDatLoaded, loadDats } from './dats.svelte';
import { loadPlatforms } from './platforms.svelte';
import { applyFileChanged, reloadTitles } from './titles.svelte';
import { loadIncoming, scheduleIncoming } from './incoming.svelte';

let subscriber: EventSubscriber | null = null;

// Re-fetches every hydrated store; the server asks for this when a
// reconnect's replay may have gaps.
async function resync(): Promise<void> {
  await Promise.all([
    loadPlatforms(),
    loadDats(),
    loadSources(),
    loadDownloads(),
    loadImports(),
    loadJobs(),
    loadWizard(),
    loadStatus(),
    reloadTitles(),
    loadIncoming('dats'),
    loadIncoming('sources')
  ]);
}

const FILE_CHANGED_DEBOUNCE_MS = 2000;
let fileChangeTimer: ReturnType<typeof setTimeout> | null = null;
let fileChangePending = false;

// At most one titles reload per window; a scan fires file.changed rapidly.
function scheduleReloadTitles(): void {
  if (fileChangeTimer) {
    fileChangePending = true;
    return;
  }
  fileChangePending = false;
  void reloadTitles();
  fileChangeTimer = setTimeout(() => {
    fileChangeTimer = null;
    if (fileChangePending) {
      scheduleReloadTitles();
    }
  }, FILE_CHANGED_DEBOUNCE_MS);
}

function handle(event: SseEvent): void {
  switch (event.name) {
    case 'resync':
      void resync();
      break;
    case 'status':
      applyStatus(event.data);
      break;
    case 'source.changed':
      applySourceChanged(event.data.source_id, event.data.state, event.data.platform_id);
      scheduleIncoming('sources');
      void loadWizard();
      break;
    case 'download.changed':
      applyDownloadChanged(event.data.download_id, event.data.state, event.data.progress);
      break;
    case 'job.progress':
      applyJobProgress(event.data.id, event.data.kind, event.data.state, event.data.progress);
      if (event.data.kind === 'dat_import') {
        scheduleIncoming('dats');
      } else if (event.data.kind === 'source_import') {
        scheduleIncoming('sources');
      }
      break;
    case 'dat.loaded':
      applyDatLoaded();
      scheduleIncoming('dats');
      void loadWizard();
      break;
    case 'dat.rejected':
      scheduleIncoming('dats');
      break;
    case 'import.done':
      void loadImports();
      scheduleReloadTitles();
      break;
    case 'file.changed':
      applyFileChanged(event.data.file_id, event.data.state);
      scheduleReloadTitles();
      break;
    default:
      break;
  }
}

export function startEvents(): void {
  if (subscriber || import.meta.env.VITE_MOCK === '1') {
    return;
  }
  subscriber = new EventSubscriber(handle, setConnected);
  subscriber.start();
}

export function stopEvents(): void {
  subscriber?.stop();
  subscriber = null;
}
