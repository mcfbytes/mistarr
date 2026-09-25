import { api } from '../api';
import { fixtureSources } from '../fixtures';
import type { Source, SourceState } from '../types';

const isMock = import.meta.env.VITE_MOCK === '1';

let sources = $state<Source[]>([]);
let reloadTimer: ReturnType<typeof setTimeout> | null = null;

export function getSources(): Source[] {
  return sources;
}

export async function loadSources(): Promise<void> {
  // Mock mode keeps its patched rows, so a change made on one page shows on the next.
  sources = isMock ? (sources.length > 0 ? sources : fixtureSources) : (await api.sources()).items;
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
  if (isMock || reloadTimer) {
    return;
  }
  reloadTimer = setTimeout(() => {
    reloadTimer = null;
    void loadSources().catch(() => undefined);
  }, 500);
}
