import { api } from '../api';
import { DELAY_MS } from '../coalesce';
import type { IncomingFile } from '../types';
import { ListStore } from './list.svelte';

export type Watched = 'dats' | 'sources';

// Events arrive in bursts while a pack loads; one re-read per burst is enough.
const reload = { ms: DELAY_MS.incoming };

const lists: Record<Watched, ListStore<IncomingFile>> = {
  dats: new ListStore((limit, offset) => api.datsIncoming(limit, offset), (f) => f.file, { reload }),
  sources: new ListStore((limit, offset) => api.sourcesIncoming(limit, offset), (f) => f.file, { reload })
};

/** The files waiting in `dats/` or `sources/`. */
export function incoming(which: Watched): ListStore<IncomingFile> {
  return lists[which];
}

/** Replaces or removes one listed file until the next read of the list. */
export function patchIncoming(which: Watched, file: string, next: IncomingFile | null): void {
  const store = lists[which];
  const rest = store.items.filter((f) => f.file !== file);
  store.items = next ? [next, ...rest] : rest;
}
