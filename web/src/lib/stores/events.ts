import { EventSubscriber } from '../api';
import type { SseEvent } from '../types';
import { applyStatus, setConnected } from './status.svelte';
import { applySourceChanged, loadSources } from './sources.svelte';
import { applyDownloadChanged, loadDownloads, loadImports } from './downloads.svelte';
import { applyJobProgress, loadJobs } from './jobs.svelte';
import { applyDatLoaded, loadDats } from './dats.svelte';
import { loadPlatforms } from './platforms.svelte';
import { reloadTitles } from './titles.svelte';

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
    reloadTitles()
  ]);
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
      break;
    case 'download.changed':
      applyDownloadChanged(event.data.download_id, event.data.state, event.data.progress);
      break;
    case 'job.progress':
      applyJobProgress(event.data.id, event.data.kind, event.data.state, event.data.progress);
      break;
    case 'dat.loaded':
      applyDatLoaded();
      break;
    case 'dat.rejected':
      break;
    case 'import.done':
      void loadImports();
      void reloadTitles();
      break;
    case 'file.changed':
      void reloadTitles();
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
