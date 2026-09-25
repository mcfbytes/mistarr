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

export function getUploads(kind: Watched): Upload[] {
  return uploads.filter((u) => u.kind === kind);
}

export function addUpload(upload: Upload): void {
  const rest = uploads.filter((u) => !(u.kind === upload.kind && u.file === upload.file));
  uploads = [upload, ...rest].slice(0, 10);
}

/** The upload whose import job is `jobId`. */
export function findUpload(jobId: number): Upload | undefined {
  return uploads.find((u) => u.jobId === jobId);
}

/** Gives an upload received before its job was recorded the job the server queued for it. */
export function resolveUpload(kind: Watched, file: string, jobId: number): void {
  uploads = uploads.map((u) => (u.kind === kind && u.file === file && u.jobId === null ? { ...u, jobId } : u));
}

/**
 * Says once how the import of uploaded file `file` ended: in `dats/` from `dat.loaded` or
 * `dat.rejected`, in `sources/` from its job's stored progress; `reason` is why it was rejected.
 */
export function announceUpload(kind: Watched, file: string, reason: string | null): void {
  const up = uploads.find((u) => u.kind === kind && u.file === file && !u.announced);
  if (!up) {
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
