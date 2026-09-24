import type { PathMapping } from './types';

/** Drops rows left blank and returns the rest, or a message naming the first bad row. */
export function cleanPathMap(map: PathMapping[]): { map: PathMapping[] } | { error: string } {
  const rows = map
    .map((m) => ({ remote: m.remote.trim(), local: m.local.trim() }))
    .filter((m) => m.remote !== '' || m.local !== '');
  const bad = rows.findIndex((m) => m.remote === '' || !m.local.startsWith('/'));
  if (bad >= 0) {
    return { error: `Mapping ${bad + 1} needs a remote path and an absolute local path.` };
  }
  return { map: rows };
}
