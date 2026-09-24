import { EventSubscriber } from '../api';
import type { SseEvent } from '../types';
import { applyStatus, setConnected } from './status.svelte';
import { applySourceChanged } from './sources.svelte';
import { applyDownloadChanged } from './downloads.svelte';
import { applyJobProgress } from './jobs.svelte';
import { applyDatLoaded } from './dats.svelte';

let subscriber: EventSubscriber | null = null;

function handle(event: SseEvent): void {
  switch (event.name) {
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
      applyJobProgress(event.data.id, event.data.kind, event.data.progress);
      break;
    case 'dat.loaded':
      applyDatLoaded(event.data.dat_version_id, event.data.file);
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
