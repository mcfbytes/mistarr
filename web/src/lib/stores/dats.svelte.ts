import { api } from '../api';
import { readAllPages } from '../paging';
import type { DatVersion } from '../types';

let dats = $state<DatVersion[]>([]);
let total = $state(0);

export function getDats(): DatVersion[] {
  return dats;
}

/** Every stored version, as the server counts them. */
export function getDatTotal(): number {
  return total;
}

export async function loadDats(): Promise<void> {
  const all = await readAllPages((limit, offset) => api.dats(limit, offset), (d) => d.id);
  dats = all.sort((a, b) => b.loaded_at - a.loaded_at || b.id - a.id);
  total = dats.length;
}

/** Marks a version removed in place, as the server reports it after `DELETE /dats/{id}`. */
export function markDatRemoved(id: number): void {
  dats = dats.map((d) =>
    d.id === id ? { ...d, retired: true, reason: 'Removed; its games are no longer listed' } : d
  );
}

export function applyDatLoaded(): void {
  void loadDats();
}
