import { api } from '../api';
import type { Source } from '../types';
import { ListStore } from './list.svelte';

/** Every source the server knows. */
export const sources = new ListStore<Source>(
  (limit, offset) => api.sources(limit, offset),
  (s) => s.id
);

export function findSource(id: number): Source | undefined {
  return sources.items.find((s) => s.id === id);
}
