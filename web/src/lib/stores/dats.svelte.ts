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
  const rows: DatVersion[] = [];
  let count = 0;
  do {
    const page = await api.dats(PAGE, rows.length);
    rows.push(...page.items);
    count = page.total;
    if (page.items.length === 0) {
      break;
    }
  } while (rows.length < count);
  dats = rows;
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
