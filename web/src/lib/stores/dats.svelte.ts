import { api } from '../api';
import type { DatVersion } from '../types';
import { ListStore } from './list.svelte';

/** Every stored version, newest first. */
export const dats = new ListStore<DatVersion>(
  (limit, offset) => api.dats(limit, offset),
  (d) => d.id,
  { sort: (a, b) => b.loaded_at - a.loaded_at || b.id - a.id }
);

/** Marks a version removed in place, as the server reports it after `DELETE /dats/{id}`. */
export function markDatRemoved(id: number): void {
  dats.patch(id, { retired: true, reason: 'Removed; its games are no longer listed' });
}
