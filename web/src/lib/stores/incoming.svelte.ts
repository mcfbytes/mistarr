import { api } from '../api';
import { readAllPages } from '../paging';
import type { IncomingFile } from '../types';

export type Watched = 'dats' | 'sources';

let lists = $state<Record<Watched, IncomingFile[]>>({ dats: [], sources: [] });
const timers: Record<Watched, ReturnType<typeof setTimeout> | undefined> = { dats: undefined, sources: undefined };

export function getIncoming(which: Watched): IncomingFile[] {
  return lists[which];
}

export async function loadIncoming(which: Watched): Promise<void> {
  const items =
    which === 'dats'
      ? await readAllPages((limit, offset) => api.datsIncoming(limit, offset), (f) => f.file)
      : await readAllPages((limit, offset) => api.sourcesIncoming(limit, offset), (f) => f.file);
  lists = { ...lists, [which]: items };
}

/** Replaces or removes one listed file until the next read of the list. */
export function patchIncoming(which: Watched, file: string, next: IncomingFile | null): void {
  const rest = lists[which].filter((f) => f.file !== file);
  lists = { ...lists, [which]: next ? [next, ...rest] : rest };
}

// Events arrive in bursts while a pack loads; one re-read per burst is enough.
export function scheduleIncoming(which: Watched): void {
  if (timers[which]) {
    return;
  }
  timers[which] = setTimeout(() => {
    timers[which] = undefined;
    void loadIncoming(which).catch(() => undefined);
  }, 300);
}
