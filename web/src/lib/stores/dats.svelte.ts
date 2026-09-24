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

export function applyDatLoaded(datVersionId: number | undefined, file: string): void {
  if (datVersionId === undefined) {
    return;
  }
  const exists = dats.some((d) => d.id === datVersionId);
  if (!exists) {
    dats = [
      ...dats,
      {
        id: datVersionId,
        platform_id: null,
        dat_name: file,
        version: '',
        source_file: file,
        loaded_at: Date.now() / 1000,
        superseded_by: null,
        game_count: 0
      }
    ];
  }
}
