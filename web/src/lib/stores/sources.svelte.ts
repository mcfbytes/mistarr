import { api } from '../api';
import { fixtureSources } from '../fixtures';
import type { Source, SourceState } from '../types';

const isMock = import.meta.env.VITE_MOCK === '1';

let sources = $state<Source[]>([]);

export function getSources(): Source[] {
  return sources;
}

export async function loadSources(): Promise<void> {
  sources = isMock ? fixtureSources : (await api.sources()).items;
}

export function patchSource(id: number, patch: Partial<Source>): void {
  sources = sources.map((s) => (s.id === id ? { ...s, ...patch } : s));
}

// The event carries no reason or suggestion, so the list is re-read after patching.
export function applySourceChanged(sourceId: number, state: SourceState, platformId: string | null): void {
  patchSource(sourceId, { state, platform_id: platformId ?? null });
  void loadSources().catch(() => undefined);
}
