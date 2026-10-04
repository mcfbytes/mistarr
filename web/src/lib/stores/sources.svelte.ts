import { api } from '../api';
import { readAllPages } from '../paging';
import type { Source, SourceState } from '../types';

let sources = $state<Source[]>([]);
let reloadTimer: ReturnType<typeof setTimeout> | null = null;

export function getSources(): Source[] {
  return sources;
}

export async function loadSources(): Promise<void> {
  sources = await readAllPages((limit, offset) => api.sources(limit, offset), (s) => s.id);
}

export function findSource(id: number): Source | undefined {
  return sources.find((s) => s.id === id);
}

export function patchSource(id: number, patch: Partial<Source>): void {
  sources = sources.map((s) => (s.id === id ? { ...s, ...patch } : s));
}

// The event carries no reason or suggestion, so the list is re-read, once per burst.
export function applySourceChanged(sourceId: number, state: SourceState, platformId: string | null): void {
  patchSource(sourceId, { state, platform_id: platformId ?? null });
  if (reloadTimer) {
    return;
  }
  reloadTimer = setTimeout(() => {
    reloadTimer = null;
    void loadSources().catch(() => undefined);
  }, 500);
}
