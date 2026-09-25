import type { DownloadState, IncomingFile, Job, SourceState } from './types';

/** The fixed status vocabulary every job, file and transfer is shown in; see docs/UI.md. */
export type WorkStatus = 'queued' | 'running' | 'waiting' | 'paused' | 'done' | 'failed';

export const STATUS_LABEL: Record<WorkStatus, string> = {
  queued: 'Queued',
  running: 'Running',
  waiting: 'Waiting',
  paused: 'Paused',
  done: 'Done',
  failed: 'Failed'
};

/** A status plus the word shown for it where the thing has its own name for the state. */
export interface Shown {
  status: WorkStatus;
  label: string;
}

function shown(status: WorkStatus, label?: string): Shown {
  return { status, label: label ?? STATUS_LABEL[status] };
}

/** Kinds of housekeeping work the activity indicator leaves to the Activity page. */
export const QUIET_KINDS = new Set(['detect_client', 'resolve_magnet', 'deselect', 'remap_sources']);

/**
 * A job's status: a queued job held by the gate is paused, one blocked by other work
 * (its reason starts "Waiting") is waiting.
 */
export function jobStatus(job: Pick<Job, 'state' | 'reason'>): Shown {
  if (job.state === 'queued') {
    if (job.reason?.startsWith('Paused')) {
      return shown('paused');
    }
    return shown(job.reason?.startsWith('Waiting') ? 'waiting' : 'queued');
  }
  return shown(job.state);
}

export function incomingStatus(f: Pick<IncomingFile, 'state' | 'reason' | 'job_id'>): Shown {
  if (f.state === 'importing') {
    return shown('running', 'Importing');
  }
  if (f.state === 'rejected') {
    return shown('failed', 'Rejected');
  }
  if (f.reason?.startsWith('Paused')) {
    return shown('paused');
  }
  return f.job_id !== null && f.reason?.startsWith('Queued') ? shown('queued') : shown('waiting');
}

export function sourceStatus(state: SourceState): Shown {
  const map: Record<SourceState, Shown> = {
    resolving: shown('running', 'Resolving'),
    unbound: shown('waiting', 'Unbound'),
    bound: shown('done', 'Bound'),
    disabled: shown('paused', 'Disabled')
  };
  return map[state];
}

export function downloadStatus(state: DownloadState): Shown {
  const map: Record<DownloadState, Shown> = {
    wanted: shown('waiting', 'Wanted'),
    queued: shown('queued'),
    transferring: shown('running', 'Transferring'),
    checking: shown('running', 'Checking'),
    importing: shown('running', 'Importing'),
    done: shown('done'),
    bad: shown('failed', 'Bad file'),
    failed: shown('failed'),
    cancelled: shown('paused', 'Cancelled')
  };
  return map[state];
}

const KIND_LABEL: Record<string, string> = {
  dat_import: 'DAT import',
  source_import: 'Source import',
  scan: 'Scan',
  recompute_1g1r: 'Matching',
  arcade_catalog: 'Arcade catalogue',
  import: 'Import',
  chd_tracks: 'CHD tracks',
  transfer: 'Transfer',
  remap_sources: 'Source matching',
  detect_client: 'Client check',
  resolve_magnet: 'Magnet lookup',
  deselect: 'Transfer stop',
  url_fetch: 'URL fetch'
};

export function kindLabel(kind: string): string {
  const label = KIND_LABEL[kind] ?? kind.replace(/_/g, ' ');
  return label.charAt(0).toUpperCase() + label.slice(1);
}

/** The file name or platform a job is about, from its payload. */
export function jobDetail(payload: Record<string, unknown>): string | null {
  if (typeof payload.path === 'string') {
    return payload.path.split('/').pop() ?? null;
  }
  return typeof payload.platform_id === 'string' ? payload.platform_id : null;
}

type FetchJob = Pick<Job, 'id' | 'kind' | 'progress' | 'created_at'>;

/**
 * What a URL fetch is about, never any part of the URL: its file once known, else when it
 * was sent, numbered by job when several unnamed fetches in `all` were sent that second.
 */
export function fetchSubject(job: FetchJob, all: readonly FetchJob[] = []): string {
  if (typeof job.progress?.file === 'string') {
    return job.progress.file;
  }
  const time = new Date(job.created_at * 1000).toLocaleTimeString(undefined, {
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit'
  });
  const same = all
    .filter((j) => j.kind === 'url_fetch' && typeof j.progress?.file !== 'string' && j.created_at === job.created_at)
    .map((j) => j.id)
    .sort((a, b) => a - b);
  const n = same.indexOf(job.id);
  return same.length > 1 && n >= 0 ? `sent at ${time} (${n + 1})` : `sent at ${time}`;
}

/** The page that owns a job's result. */
export function jobHref(job: Pick<Job, 'kind' | 'payload'> & { progress?: Job['progress'] }): string {
  switch (job.kind) {
    case 'url_fetch':
      return job.progress?.target === 'sources' ? '#/sources' : job.progress?.target === 'dats' ? '#/dats' : '#/activity';
    case 'dat_import':
      return '#/dats';
    case 'source_import':
    case 'resolve_magnet':
    case 'remap_sources':
      return '#/sources';
    case 'scan':
    case 'recompute_1g1r':
      return typeof job.payload.platform_id === 'string' ? `#/p/${job.payload.platform_id}` : '#/';
    default:
      return '#/activity';
  }
}

/** How far a job has come: `fraction` from 0 to 1, or `null` when unknown, and a line of text. */
export interface ProgressView {
  fraction: number | null;
  text: string;
}

const PHASE_TEXT: Record<string, string> = {
  indexing: 'Reading the clone list',
  reading: 'Reading games',
  storing: 'Saving titles',
  picking: 'Choosing preferred versions',
  refreshing: 'Refreshing title groups',
  matching: 'Matching files on the card',
  'copying the database to memory': 'Copying the database to memory',
  'writing the database to the card': 'Writing the database to the card',
  importing: 'Importing',
  'importing in place': 'Importing in place',
  connecting: 'Connecting',
  receiving: 'Receiving',
  checking: 'Checking the file',
  placing: 'Writing to the card'
};

/** Bytes as "4.2 MiB" or "812 KiB", the units the size caps are given in. */
export function bytesText(n: number): string {
  if (n >= 1024 * 1024) {
    return `${(n / (1024 * 1024)).toFixed(1)} MiB`;
  }
  return `${Math.max(0, Math.round(n / 1024)).toLocaleString()} KiB`;
}

function num(v: unknown): number | null {
  return typeof v === 'number' && Number.isFinite(v) ? v : null;
}

/** Reads a running job's progress; `null` when it says nothing yet. */
export function describeProgress(kind: string, p: Record<string, unknown> | null): ProgressView | null {
  if (!p) {
    return null;
  }
  const phase = typeof p.phase === 'string' ? p.phase : null;
  const parts: string[] = [];
  let fraction: number | null = null;
  if (kind === 'dat_import') {
    const members = num(p.members);
    const done = num(p.done) ?? 0;
    const read = num(p.bytes_read);
    const total = num(p.bytes_total);
    if (read !== null && total !== null && total > 0) {
      fraction = Math.min(1, read / total);
    }
    if (members !== null && members > 1) {
      parts.push(`DAT ${Math.min(done + 1, members)} of ${members}`);
      if (fraction !== null) {
        fraction = (done + fraction) / members;
      }
    }
    const games = num(p.games);
    if (games !== null) {
      parts.push(`${games.toLocaleString()} games`);
    }
    // Why the import runs on the card rather than on a copy in memory.
    if (typeof p.reason === 'string' && p.reason) {
      parts.push(p.reason);
    }
  } else if (kind === 'scan') {
    const done = num(p.done);
    const total = num(p.total);
    if (done !== null && total !== null && total > 0) {
      fraction = Math.min(1, done / total);
      parts.push(`${done.toLocaleString()} of ${total.toLocaleString()} files`);
    }
  } else if (kind === 'chd_tracks') {
    const read = num(p.bytes_done);
    const total = num(p.bytes_total);
    if (read !== null && total !== null && total > 0) {
      fraction = Math.min(1, read / total);
    }
    if (typeof p.file === 'string') {
      parts.push(p.file);
    }
    const done = num(p.done);
    const images = num(p.total);
    if (done !== null && images !== null && images > 1) {
      parts.push(`image ${Math.min(done + 1, images)} of ${images}`);
    }
  } else if (kind === 'url_fetch') {
    const got = num(p.bytes_received);
    const total = num(p.bytes_total);
    if (phase === 'receiving' && got !== null) {
      if (total !== null && total > 0) {
        fraction = Math.min(1, got / total);
        parts.push(`${bytesText(got)} of ${bytesText(total)}`);
      } else {
        parts.push(`${bytesText(got)} received`);
      }
    }
  } else if (kind === 'recompute_1g1r') {
    const checked = num(p.checked);
    if (checked !== null) {
      parts.push(`${checked.toLocaleString()} files checked`);
    }
  }
  const head = phase ? (PHASE_TEXT[phase] ?? phase) : null;
  const pct = fraction === null ? null : `${Math.floor(fraction * 100)}%`;
  const text = [head, pct, ...parts].filter((s): s is string => s !== null).join(' · ');
  // A phase without bytes has no share done and shows as indeterminate.
  return !text && fraction === null ? null : { fraction, text };
}

/** Which uploads get which noun in the toast after they are received. */
export function uploadNoun(file: string): string {
  if (file.endsWith('.magnet')) {
    return 'Magnet';
  }
  return file.endsWith('.torrent') ? 'Torrent' : 'DAT';
}
