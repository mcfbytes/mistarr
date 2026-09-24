import { api } from '../api';
import { fixtureIncomingDats, fixtureIncomingSources } from '../fixtures';
import type { IncomingFile } from '../types';

const isMock = import.meta.env.VITE_MOCK === '1';

export type Watched = 'dats' | 'sources';

let lists = $state<Record<Watched, IncomingFile[]>>({ dats: [], sources: [] });
const timers: Partial<Record<Watched, ReturnType<typeof setTimeout>>> = {};

export function getIncoming(which: Watched): IncomingFile[] {
  return lists[which];
}

export async function loadIncoming(which: Watched): Promise<void> {
  if (isMock) {
    lists = {
      ...lists,
      [which]: which === 'dats' ? fixtureIncomingDats : fixtureIncomingSources
    };
    return;
  }
  const page = which === 'dats' ? await api.datsIncoming() : await api.sourcesIncoming();
  lists = { ...lists, [which]: page.items };
}

// Events arrive in bursts while a pack loads; one re-read per burst is enough.
export function scheduleIncoming(which: Watched): void {
  if (isMock || timers[which]) {
    return;
  }
  timers[which] = setTimeout(() => {
    delete timers[which];
    void loadIncoming(which).catch(() => undefined);
  }, 300);
}
