import type { Watched } from './incoming.svelte';
import { showToast } from './toast.svelte';

/** A file the user uploaded in this session, followed until its import finishes. */
export interface Upload {
  kind: Watched;
  file: string;
  /** `null` until the server has recorded the import job; see docs/API.md "Upload answers". */
  jobId: number | null;
  /** What the file waited for when it was received. */
  reason: string | null;
  /** Set by a resync, after which its finishing event may have been missed. */
  stale?: boolean;
  /** Set once a toast has said how its import ended. */
  announced?: boolean;
}

let uploads = $state<Upload[]>([]);

/** What events said about a file before its upload's answer arrived, kept briefly. */
interface Early {
  at: number;
  jobId?: number;
  /** Present once its import ended: `null` when it loaded, else why it was rejected. */
  outcome?: string | null;
}

// Plain, not reactive: nothing renders from it.
let early: Record<string, Early | undefined> = {};
const EARLY_KEEP_MS = 30_000;

function earlyKey(kind: Watched, file: string): string {
  return `${kind}/${file}`;
}

// Notes an event for an upload whose answer has not arrived yet.
function noteEarly(kind: Watched, file: string, note: Partial<Early>): void {
  const now = Date.now();
  early = Object.fromEntries(Object.entries(early).filter(([, e]) => e && now - e.at <= EARLY_KEEP_MS));
  const key = earlyKey(kind, file);
  early[key] = { ...early[key], ...note, at: now };
}

export function getUploads(kind: Watched): Upload[] {
  return uploads.filter((u) => u.kind === kind);
}

/**
 * Follows an upload; a queued job or an outcome its events reported before the
 * server's answer arrived is applied to it at once.
 */
export function addUpload(upload: Upload): void {
  const key = earlyKey(upload.kind, upload.file);
  const seen = early[key];
  early[key] = undefined;
  const fresh = seen !== undefined && Date.now() - seen.at <= EARLY_KEEP_MS ? seen : undefined;
  const rest = uploads.filter((u) => !(u.kind === upload.kind && u.file === upload.file));
  uploads = [{ ...upload, jobId: upload.jobId ?? fresh?.jobId ?? null }, ...rest].slice(0, 10);
  if (fresh?.outcome !== undefined) {
    announceUpload(upload.kind, upload.file, fresh.outcome);
  }
}

/** Gives an upload received before its job was recorded the job the server queued for it. */
export function resolveUpload(kind: Watched, file: string, jobId: number): void {
  if (!uploads.some((u) => u.kind === kind && u.file === file)) {
    noteEarly(kind, file, { jobId });
    return;
  }
  uploads = uploads.map((u) => (u.kind === kind && u.file === file && u.jobId === null ? { ...u, jobId } : u));
}

/**
 * Says once how the import of uploaded file `file` ended: in `dats/` from `dat.loaded` or
 * `dat.rejected`, in `sources/` from its job's stored progress; `reason` is why it was rejected.
 */
export function announceUpload(kind: Watched, file: string, reason: string | null): void {
  const up = uploads.find((u) => u.kind === kind && u.file === file);
  if (!up) {
    noteEarly(kind, file, { outcome: reason });
    return;
  }
  if (up.announced) {
    return;
  }
  uploads = uploads.map((u) => (u.kind === kind && u.file === file ? { ...u, announced: true } : u));
  if (reason !== null) {
    showToast(`${file} was rejected: ${reason}`, 'error');
  } else {
    showToast(kind === 'dats' ? `${file} loaded.` : `${file} added as a source.`, 'success');
  }
}

export function markUploadsStale(): void {
  uploads = uploads.map((u) => ({ ...u, stale: true }));
}

export function dismissUpload(kind: Watched, file: string): void {
  uploads = uploads.filter((u) => !(u.kind === kind && u.file === file));
}
