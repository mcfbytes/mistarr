import { api } from '../api';
import { mockExtraPlatforms, scenarioPlatforms } from '../fixtures';
import { readAllPages } from '../paging';
import type { Platform } from '../types';

const isMock = import.meta.env.VITE_MOCK === '1';

let platforms = $state<Platform[]>([]);
let loaded = $state(false);

export function getPlatforms(): Platform[] {
  return platforms;
}

export function platformsLoaded(): boolean {
  return loaded;
}

export async function loadPlatforms(): Promise<void> {
  platforms = isMock
    ? [...scenarioPlatforms(), ...mockExtraPlatforms()]
    : await readAllPages((limit, offset) => api.platforms(limit, offset), (p) => p.id);
  loaded = true;
}

export function findPlatform(id: string): Platform | undefined {
  return platforms.find((p) => p.id === id);
}

export function patchPlatform(id: string, patch: Partial<Platform>): void {
  platforms = platforms.map((p) => (p.id === id ? { ...p, ...patch } : p));
}
