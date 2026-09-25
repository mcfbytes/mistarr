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
  transfer: 'Transfer',
  remap_sources: 'Source matching',
  detect_client: 'Client check',
  resolve_magnet: 'Magnet lookup',
  deselect: 'Transfer stop'
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

/** The page that owns a job's result. */
export function jobHref(job: Pick<Job, 'kind' | 'payload'>): string {
  switch (job.kind) {
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
  matching: 'Matching files on the card'
};

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
  } else if (kind === 'scan') {
    const done = num(p.done);
    const total = num(p.total);
    if (done !== null && total !== null && total > 0) {
      fraction = Math.min(1, done / total);
      parts.push(`${done.toLocaleString()} of ${total.toLocaleString()} files`);
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
