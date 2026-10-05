import { api, type EventStream } from '../api';
import { coalesce, DELAY_MS } from '../coalesce';
import type { DownloadState, SourceState, SseEvent } from '../types';
import { applyStatus, loadStatus, loadWizard, setConnected } from './status.svelte';
import { sources } from './sources.svelte';
import { downloads, imports, watchingImports } from './downloads.svelte';
import { applyJobProgress, DAT_FILE, isRunning, jobs, reportEnd, resetFinished, resyncRecent } from './jobs.svelte';
import { dats } from './dats.svelte';
import { platforms } from './platforms.svelte';
import { applyFileChanged, reloadTitles } from './titles.svelte';
import { incoming } from './incoming.svelte';
import { markUploadsStale, resolveUpload } from './uploads.svelte';

let subscriber: EventStream | null = null;

// Re-fetches every hydrated store when a reconnect's replay may have gaps;
// allSettled so one failed store neither rejects resync nor hides the others.
async function resync(): Promise<void> {
  resetFinished();
  markUploadsStale();
  await Promise.allSettled([
    platforms.load(),
    dats.load(),
    sources.load(),
    downloads.load(),
    imports.load(),
    jobs.load(),
    resyncRecent(),
    loadWizard(),
    loadStatus(),
    reloadTitles(),
    incoming('dats').load(),
    incoming('sources').load()
  ]);
}

// Jobs whose end can move the platform counts and the browse table.
const MATCHING_KINDS = new Set(['scan', 'recompute_1g1r', 'arcade_catalog', 'chd_tracks']);
const reloadTitlesSoon = coalesce(() => void reloadTitles(), DELAY_MS.reload);
// At most one titles reload per window; a scan fires file.changed rapidly.
const reloadTitlesBurst = coalesce(() => void reloadTitles(), DELAY_MS.window, { leading: true });

// The imports log is re-read only while Activity shows it; the next mount or resync pages it in.
function reloadImportsSoon(): void {
  if (watchingImports()) {
    imports.reloadSoon();
  }
}

// The event carries no reason or suggestion, so the list is re-read, once per burst.
function sourceChanged(sourceId: number, state: SourceState, platformId: string | null): void {
  sources.patch(sourceId, { state, platform_id: platformId });
  sources.reloadSoon();
}

function downloadChanged(downloadId: number, state: DownloadState, progress: number): void {
  if (downloads.items.some((d) => d.id === downloadId)) {
    downloads.patch(downloadId, { state, progress });
  } else {
    void downloads.load();
  }
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
      sourceChanged(event.data.source_id, event.data.state, event.data.platform_id);
      incoming('sources').reloadSoon();
      // Only matters while the wizard is open; a miss shows up at the next resync.
      void loadWizard().catch(() => undefined);
      break;
    case 'download.changed':
      downloadChanged(event.data.download_id, event.data.state, event.data.progress);
      break;
    case 'job.progress': {
      // Live progress of a job already known to run moves no file between lists.
      const { id, kind, state, progress, detail = null } = event.data;
      const moved = !(state === 'running' && isRunning(id));
      if (detail !== null && (kind === 'dat_import' || kind === 'source_import')) {
        resolveUpload(kind === 'dat_import' ? 'dats' : 'sources', detail, id);
      }
      applyJobProgress(id, kind, state, progress, detail);
      if ((state === 'done' || state === 'failed') && MATCHING_KINDS.has(kind)) {
        // A miss leaves counts stale until the next resync or matching job.
        platforms.reloadSoon();
        reloadTitlesSoon();
      }
      if (moved && kind === 'dat_import') {
        incoming('dats').reloadSoon();
      } else if (moved && kind === 'source_import') {
        incoming('sources').reloadSoon();
      }
      break;
    }
    case 'dat.loaded':
      reportEnd({ id: null, kind: DAT_FILE, state: 'done', progress: null, detail: event.data.file });
      void dats.load();
      incoming('dats').reloadSoon();
      // Only matters while the wizard is open; a miss shows up at the next resync.
      void loadWizard().catch(() => undefined);
      break;
    case 'dat.rejected':
      reportEnd({
        id: null,
        kind: DAT_FILE,
        state: 'done',
        progress: { rejected: event.data.reason },
        detail: event.data.file
      });
      incoming('dats').reloadSoon();
      break;
    case 'import.done':
      reloadImportsSoon();
      reloadTitlesBurst();
      break;
    case 'file.changed':
      applyFileChanged(event.data.file_id, event.data.state);
      reloadTitlesBurst();
      break;
    default:
      break;
  }
}

export function startEvents(): void {
  if (subscriber) {
    return;
  }
  // Live progress is never replayed, so every (re)connection reads the open jobs again.
  subscriber = api.events(handle, (connected) => {
    setConnected(connected);
    if (connected) {
      // A miss leaves the running list stale until the next job.progress event or resync.
      void jobs.load();
    }
  });
  subscriber.start();
}

export function stopEvents(): void {
  subscriber?.stop();
  subscriber = null;
}
