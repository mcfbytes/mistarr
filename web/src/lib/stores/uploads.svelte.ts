import { DAT_FILE, followJob, jobIdFor, type JobEnd } from './jobs.svelte';
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
}

let uploads = $state<Upload[]>([]);
// Stops following each upload's ending; keyed by kind and file.
// eslint-disable-next-line svelte/prefer-svelte-reactivity -- only event handlers read it, never markup
const following = new Map<string, () => void>();

function followKey(kind: Watched, file: string): string {
  return `${kind}/${file}`;
}

/** Why an import ended in rejection: `null` when it loaded. */
function rejection(end: JobEnd): string | null {
  if (end.state === 'failed') {
    return typeof end.progress?.error === 'string' ? end.progress.error : 'see Activity';
  }
  return typeof end.progress?.rejected === 'string' ? end.progress.rejected : null;
}

export function getUploads(kind: Watched): Upload[] {
  return uploads.filter((u) => u.kind === kind);
}

/**
 * Follows an upload and says once how its import ended: a DAT's from `dat.loaded` or
 * `dat.rejected`, a source's from its job. An ending heard since `since`, when the request
 * began, counts even if it came before the answer; an earlier attempt's does not.
 */
export function addUpload(upload: Upload, since: number): void {
  const { kind, file } = upload;
  const key = followKey(kind, file);
  following.get(key)?.();
  const rest = uploads.filter((u) => !(u.kind === kind && u.file === file));
  const jobId = upload.jobId ?? jobIdFor(kind === 'dats' ? 'dat_import' : 'source_import', file, since);
  const next = [{ ...upload, jobId }, ...rest];
  for (const gone of next.slice(10)) {
    stopFollowing(gone.kind, gone.file);
  }
  uploads = next.slice(0, 10);
  // followJob may call onEnd before it returns, which the compiler cannot see.
  let over = false as boolean;
  const stop = followJob(
    (end) => end.detail === file && end.kind === (kind === 'dats' ? DAT_FILE : 'source_import'),
    (end) => {
      over = true;
      following.delete(key);
      const why = rejection(end);
      if (why !== null) {
        showToast(`${file} was rejected: ${why}`, 'error');
      } else {
        showToast(kind === 'dats' ? `${file} loaded.` : `${file} added as a source.`, 'success');
      }
    },
    since
  );
  if (!over) {
    following.set(key, stop);
  }
}

function stopFollowing(kind: Watched, file: string): void {
  const key = followKey(kind, file);
  following.get(key)?.();
  following.delete(key);
}

/** Gives an upload received before its job was recorded the job the server queued for it. */
export function resolveUpload(kind: Watched, file: string, jobId: number): void {
  uploads = uploads.map((u) => (u.kind === kind && u.file === file && u.jobId === null ? { ...u, jobId } : u));
}

export function markUploadsStale(): void {
  uploads = uploads.map((u) => ({ ...u, stale: true }));
}

export function dismissUpload(kind: Watched, file: string): void {
  stopFollowing(kind, file);
  uploads = uploads.filter((u) => !(u.kind === kind && u.file === file));
}
