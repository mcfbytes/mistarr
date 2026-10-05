import { api } from '../api';
import type { Platform } from '../types';
import { ListStore } from './list.svelte';

/** Every platform, with its counts. */
export const platforms = new ListStore<Platform>(
  (limit, offset) => api.platforms(limit, offset),
  (p) => p.id
);

export function findPlatform(id: string): Platform | undefined {
  return platforms.items.find((p) => p.id === id);
}
