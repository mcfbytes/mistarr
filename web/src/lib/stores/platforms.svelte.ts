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

/** A platform's name, or its id while the platforms are not loaded. */
export function platformName(id: string): string {
  return findPlatform(id)?.name ?? id;
}

// Scan jobs whose outcome is awaited; it outlives the page, so a repeat answer for one job adds no second toast.
// eslint-disable-next-line svelte/prefer-svelte-reactivity -- only event handlers read it, never markup
export const followedScans = new Set<number>();
