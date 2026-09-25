import type { DownloadState, Source, SourceFileKind, SourceFileUnmatched } from './types';

/** A byte count in the largest unit that keeps it above one, such as "1.4 GB". */
export function formatSize(bytes: number): string {
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let value = bytes;
  let unit = 0;
  while (value >= 1000 && unit < units.length - 1) {
    value /= 1000;
    unit += 1;
  }
  return unit === 0 ? `${value} B` : `${value.toFixed(value < 10 ? 1 : 0)} ${units[unit]}`;
}

/** The first and last eight characters of an infohash. */
export function shortHash(hash: string): string {
  return hash.length > 20 ? `${hash.slice(0, 8)}…${hash.slice(-8)}` : hash;
}

const KIND_TEXT: Record<SourceFileKind, string> = {
  rom: 'Game file',
  archive: 'Archive',
  disc: 'Disc image',
  extra: 'Extra'
};

/** How a file's kind reads in the file table. */
export function kindText(kind: SourceFileKind): string {
  return KIND_TEXT[kind];
}

/** The sentence for why a file matched no DAT entry. */
export function unmatchedText(why: SourceFileUnmatched): string {
  switch (why) {
    case 'unbound':
      return 'Not matched: the source is not bound to a platform.';
    case 'extra':
      return 'Not a game file.';
    default:
      return 'Not matched: no DAT entry has this name, or this base name and size.';
  }
}

const DOWNLOAD_TEXT: Record<DownloadState, string> = {
  wanted: 'Wanted',
  queued: 'Queued',
  transferring: 'Transferring',
  checking: 'Checking',
  importing: 'Importing',
  done: 'Done',
  bad: 'Bad file',
  failed: 'Failed',
  cancelled: 'Cancelled'
};

/** A file's transfer state with its share done while it moves, such as "Transferring · 45%". */
export function downloadText(d: { state: DownloadState; progress: number }): string {
  const moving = d.state === 'transferring' || d.state === 'checking';
  return moving ? `${DOWNLOAD_TEXT[d.state]} · ${Math.floor(d.progress * 100)}%` : DOWNLOAD_TEXT[d.state];
}

/** How the source came to its binding, in one sentence. */
export function bindingText(source: Source, platformName: (id: string) => string): string {
  const share = source.bind_score === null ? null : Math.round(source.bind_score * 100);
  const matches = share === null ? '' : ` ${share}% of its files match DAT entries.`;
  if (source.platform_id) {
    const name = platformName(source.platform_id);
    if (source.user_binding) {
      return `Bound to ${name} by you.${matches}`;
    }
    const hint =
      source.suggested_platform_id === source.platform_id ? ` Its names also point to ${name}.` : '';
    return `Bound to ${name} automatically.${matches}${hint}`;
  }
  if (source.user_binding) {
    return 'Marked by you as not a game set. It is not bound automatically.';
  }
  return 'Not bound to a platform.';
}

/** The share of the source's open downloads transferred, 0 to 1, or `null` with none open. */
export function transferShare(t: { size: number; done: number }): number | null {
  return t.size > 0 ? Math.min(1, t.done / t.size) : null;
}
