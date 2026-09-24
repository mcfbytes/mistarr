import { api, errorMessage } from '../api';
import { fixtureTitle, fixtureTitles, mockDelayMs } from '../fixtures';
import type { FileState, TitleDetail, TitleFilters, TitleGroup } from '../types';

const isMock = import.meta.env.VITE_MOCK === '1';
const PAGE_SIZE = 60;

let groups = $state<TitleGroup[]>([]);
let groupsTotal = $state(0);
let groupsPlatform = $state<string | null>(null);
let groupsLoading = $state(false);
let groupsError = $state<string | null>(null);
let lastFilters: TitleFilters = {};
let lastPage = 0;
let detail = $state<TitleDetail | null>(null);
let detailId: number | null = null;
let groupsController: AbortController | null = null;
let detailToken = 0;

export function getGroups(): TitleGroup[] {
  return groups;
}

export function getGroupsTotal(): number {
  return groupsTotal;
}

/** True while a page the user asked for is on its way. */
export function isGroupsLoading(): boolean {
  return groupsLoading;
}

/** Why the latest page failed to load, or null. */
export function getGroupsError(): string | null {
  return groupsError;
}

/** Waits `ms`, or less when `signal` aborts first. */
function pause(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    const timer = setTimeout(resolve, ms);
    signal.addEventListener('abort', () => {
      clearTimeout(timer);
      resolve();
    });
  });
}

async function mockTitles(
  platformId: string,
  filters: TitleFilters,
  signal: AbortSignal
): Promise<{ items: TitleGroup[]; total: number }> {
  const ms = mockDelayMs(filters.q ?? '');
  await pause(Math.abs(ms), signal);
  if (ms < 0) {
    throw new Error('Mock search failed.');
  }
  const all = fixtureTitles(platformId, 240, filters);
  return { items: all, total: all.length };
}

export function getDetail(): TitleDetail | null {
  return detail;
}

/**
 * Loads one page of groups, aborting the request before it so only the newest
 * answer lands. A `quiet` load refreshes in place without the loading state.
 */
export async function loadTitlesPage(
  platformId: string,
  filters: TitleFilters,
  page: number,
  quiet = false
): Promise<void> {
  groupsController?.abort();
  const controller = new AbortController();
  groupsController = controller;
  lastFilters = filters;
  lastPage = page;

  if (groupsPlatform !== platformId && page === 0) {
    groups = [];
  }
  groupsPlatform = platformId;
  if (!quiet) {
    groupsLoading = true;
  }
  const offset = page * PAGE_SIZE;
  try {
    const res = isMock
      ? await mockTitles(platformId, filters, controller.signal).then((all) => ({
          items: all.items.slice(offset, offset + PAGE_SIZE),
          total: all.total
        }))
      : await api.titles(platformId, filters, PAGE_SIZE, offset, controller.signal);
    if (controller.signal.aborted) {
      return;
    }
    groups = page === 0 ? res.items : [...groups, ...res.items];
    groupsTotal = res.total;
    groupsError = null;
  } catch (err) {
    if (!controller.signal.aborted) {
      groupsError = errorMessage(err);
    }
  } finally {
    if (groupsController === controller) {
      groupsLoading = false;
    }
  }
}

export async function reloadTitles(): Promise<void> {
  // Snapshot before reloading: page 0 would otherwise reset lastPage first.
  const platform = groupsPlatform;
  const filters = lastFilters;
  const pages = lastPage;
  if (platform) {
    for (let p = 0; p <= pages; p += 1) {
      await loadTitlesPage(platform, filters, p, true);
    }
  }
  await refreshDetail();
}

// Re-fetches the open title without clearing it first, so the page only
// swaps once the new detail arrives instead of flashing blank.
async function refreshDetail(): Promise<void> {
  if (detailId === null) {
    return;
  }
  const id = detailId;
  const token = detailToken;
  const next = isMock ? fixtureTitle(id) : await api.title(id);
  if (token === detailToken && detailId === id) {
    detail = next;
  }
}

export function applyFileChanged(fileId: number, state: FileState): void {
  if (!detail) {
    return;
  }
  detail = {
    ...detail,
    variants: detail.variants.map((v) => ({
      ...v,
      roms: v.roms.map((r) => (r.file_id === fileId ? { ...r, file_state: state } : r))
    }))
  };
}

export async function loadTitleDetail(id: number): Promise<void> {
  const token = ++detailToken;
  detailId = id;
  detail = null;
  const next = isMock ? fixtureTitle(id) : await api.title(id);
  if (token === detailToken) {
    detail = next;
  }
}

export function clearDetail(): void {
  detail = null;
  detailId = null;
  detailToken += 1;
}

export function patchGroup(parentId: number, patch: Partial<TitleGroup>): void {
  groups = groups.map((g) => (g.parent_id === parentId ? { ...g, ...patch } : g));
}

export function setDetail(next: TitleDetail): void {
  detail = next;
}
