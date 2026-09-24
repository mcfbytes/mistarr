import type { Watched } from './incoming.svelte';

/** A file the user uploaded in this session, followed until its import finishes. */
export interface Upload {
  kind: Watched;
  file: string;
  jobId: number;
  /** Set by a resync, after which its finishing event may have been missed. */
  stale?: boolean;
}

let uploads = $state<Upload[]>([]);

export function getUploads(kind: Watched): Upload[] {
  return uploads.filter((u) => u.kind === kind);
}

export function addUpload(upload: Upload): void {
  uploads = [upload, ...uploads.filter((u) => u.jobId !== upload.jobId)].slice(0, 10);
}

export function markUploadsStale(): void {
  uploads = uploads.map((u) => ({ ...u, stale: true }));
}

export function dismissUpload(jobId: number): void {
  uploads = uploads.filter((u) => u.jobId !== jobId);
}
