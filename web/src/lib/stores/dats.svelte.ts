import { api } from '../api';
import { fixtureDats } from '../fixtures';
import type { DatVersion } from '../types';

const isMock = import.meta.env.VITE_MOCK === '1';
// The server's largest page; the list is read page by page until `total` rows arrived.
const PAGE = 1000;

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
  if (isMock) {
    dats = fixtureDats.map((d) => ({ ...d }));
    total = dats.length;
    return;
  }
  // Keyed by id, so a row that shifts pages while a load lands in between appears once.
  const byId: Record<number, DatVersion> = {};
  let offset = 0;
  let count: number;
  for (;;) {
    const page = await api.dats(PAGE, offset);
    count = page.total;
    for (const d of page.items) {
      byId[d.id] = d;
    }
    offset += page.items.length;
    if (page.items.length === 0 || offset >= count) {
      break;
    }
  }
  dats = Object.values(byId).sort((a, b) => b.loaded_at - a.loaded_at || b.id - a.id);
  total = count;
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
