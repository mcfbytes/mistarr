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
/** Counts the loads a user asked for, so a background reload stops once one starts. */
let userLoads = 0;
/** True while a page load is on its way. */
let loadInFlight = false;
/** True while `reloadTitles` runs. */
let reloading = false;
/** A reload asked for while a load was on its way, run once that load settles. */
let reloadQueued = false;
let detailToken = 0;
/** The background detail refresh on its way; a newer one cancels it. */
let detailController: AbortController | null = null;

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
  page: number,
  signal: AbortSignal
): Promise<{ items: TitleGroup[]; total: number }> {
  const ms = mockDelayMs(filters.q ?? '', page);
  await pause(Math.abs(ms), signal);
  if (ms < 0) {
    throw new Error('Mock search failed.');
  }
  const all = fixtureTitles(platformId, 240, filters);
  return { items: all, total: all.length };
}

/**
 * The page to load after the rows already in the grid: the count of full pages
 * held, so a load-more never leaves a gap whatever reloads did meanwhile.
 */
export function nextPage(): number {
  return Math.floor(groups.length / PAGE_SIZE);
}

export function getDetail(): TitleDetail | null {
  return detail;
}

/**
 * Loads one page of groups, aborting the request before it so only the newest
 * answer lands, and resolves to whether this page landed. A `quiet` load
 * refreshes in place without the loading state.
 */
export async function loadTitlesPage(
  platformId: string,
  filters: TitleFilters,
  page: number,
  quiet = false
): Promise<boolean> {
  groupsController?.abort();
  const controller = new AbortController();
  groupsController = controller;
  lastFilters = filters;
  loadInFlight = true;
  if (!quiet) {
    userLoads += 1;
  }

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
      ? await mockTitles(platformId, filters, page, controller.signal).then((all) => ({
          items: all.items.slice(offset, offset + PAGE_SIZE),
          total: all.total
        }))
      : await api.titles(platformId, filters, PAGE_SIZE, offset, controller.signal);
    if (controller.signal.aborted) {
      return false;
    }
    groups = placed(groups, res.items, offset, quiet);
    groupsTotal = res.total;
    groupsError = null;
    lastPage = page;
    return true;
  } catch (err) {
    if (!controller.signal.aborted) {
      groupsError = errorMessage(err);
    }
    return false;
  } finally {
    if (groupsController === controller) {
      groupsLoading = false;
      loadInFlight = false;
      runQueuedReload();
    }
  }
}

/**
 * `rows` with `items` at `offset`. A background reload writes its page over the
 * same slot and keeps the pages after it, unless its page came back short, so the
 * grid never shrinks to one page while the rest reload; any other load ends there.
 */
function placed(rows: TitleGroup[], items: TitleGroup[], offset: number, quiet: boolean): TitleGroup[] {
  const kept = quiet && items.length === PAGE_SIZE ? rows.slice(offset + PAGE_SIZE) : [];
  return [...rows.slice(0, offset), ...items, ...kept];
}

/** Starts the reload asked for while a load was on its way, once nothing is loading. */
function runQueuedReload(): void {
  if (reloadQueued && !loadInFlight && !reloading) {
    reloadQueued = false;
    void reloadTitles();
  }
}

/**
 * Re-fetches every loaded page in place. It stops as soon as a page fails or a
 * load the user asked for starts, so it never lands results for stale filters.
 * Asked for while a load is on its way, it runs once that load settles instead
 * of aborting it, so reloads arriving faster than a page answers never starve it.
 */
export async function reloadTitles(): Promise<void> {
  if (loadInFlight || reloading) {
    reloadQueued = true;
    return;
  }
  reloading = true;
  let complete = true;
  try {
    // Snapshot before reloading: page 0 would otherwise reset lastPage first.
    const platform = groupsPlatform;
    const filters = lastFilters;
    const pages = lastPage;
    const loads = userLoads;
    if (platform) {
      for (let p = 0; p <= pages; p += 1) {
        if (userLoads !== loads || !(await loadTitlesPage(platform, filters, p, true))) {
          complete = false;
          break;
        }
        // A short page is the new end of the list; nothing after it is left to reload.
        if (groups.length < (p + 1) * PAGE_SIZE) {
          break;
        }
      }
    }
  } finally {
    reloading = false;
    runQueuedReload();
  }
  if (complete) {
    await refreshDetail();
  }
}

if (isMock && typeof window !== 'undefined') {
  // Lets the mock e2e suite stand in for the server's file.changed events.
  (window as unknown as { mistarrReloadTitles: () => Promise<void> }).mistarrReloadTitles = reloadTitles;
}

// Re-fetches the open title without clearing it first, so the page only
// swaps once the new detail arrives instead of flashing blank.
async function refreshDetail(): Promise<void> {
  if (detailId === null) {
    return;
  }
  detailController?.abort();
  const controller = new AbortController();
  detailController = controller;
  const id = detailId;
  const token = detailToken;
  try {
    const next = isMock ? fixtureTitle(id) : await api.title(id, controller.signal);
    if (token === detailToken && detailId === id && !controller.signal.aborted) {
      detail = next;
    }
  } catch {
    // A failed or cancelled refresh keeps the detail shown; the next one tries again.
  } finally {
    if (detailController === controller) {
      detailController = null;
    }
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
