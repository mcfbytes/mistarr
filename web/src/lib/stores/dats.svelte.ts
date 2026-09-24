import { api } from '../api';
import { fixtureDats } from '../fixtures';
import type { DatVersion } from '../types';

const isMock = import.meta.env.VITE_MOCK === '1';

let dats = $state<DatVersion[]>([]);

export function getDats(): DatVersion[] {
  return dats;
}

export async function loadDats(): Promise<void> {
  dats = isMock ? fixtureDats : (await api.dats()).items;
}

export function applyDatLoaded(): void {
  void loadDats();
}
