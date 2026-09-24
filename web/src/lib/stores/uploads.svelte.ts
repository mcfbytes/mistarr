import type { Watched } from './incoming.svelte';

/** A file the user uploaded in this session, followed until its import finishes. */
export interface Upload {
  kind: Watched;
  file: string;
  jobId: number;
}

let uploads = $state<Upload[]>([]);

export function getUploads(kind: Watched): Upload[] {
  return uploads.filter((u) => u.kind === kind);
}

export function addUpload(upload: Upload): void {
  uploads = [upload, ...uploads.filter((u) => u.jobId !== upload.jobId)].slice(0, 10);
}

export function dismissUpload(jobId: number): void {
  uploads = uploads.filter((u) => u.jobId !== jobId);
}
