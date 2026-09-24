import { api } from '../api';
import { fixtureTitle, fixtureTitles } from '../fixtures';
import type { FileState, TitleDetail, TitleFilters, TitleGroup } from '../types';

const isMock = import.meta.env.VITE_MOCK === '1';
const PAGE_SIZE = 60;

let groups = $state<TitleGroup[]>([]);
let groupsTotal = $state(0);
let groupsPlatform = $state<string | null>(null);
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

export function getDetail(): TitleDetail | null {
  return detail;
}

export async function loadTitlesPage(
  platformId: string,
  filters: TitleFilters,
  page: number
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
  const offset = page * PAGE_SIZE;
  if (isMock) {
    const all = fixtureTitles(platformId, 240);
    const slice = all.slice(offset, offset + PAGE_SIZE);
    groups = page === 0 ? slice : [...groups, ...slice];
    groupsTotal = all.length;
    return;
  }
  try {
    const res = await api.titles(platformId, filters, PAGE_SIZE, offset, controller.signal);
    if (controller.signal.aborted) {
      return;
    }
    groups = page === 0 ? res.items : [...groups, ...res.items];
    groupsTotal = res.total;
  } catch (err) {
    if (!controller.signal.aborted) {
      throw err;
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
      await loadTitlesPage(platform, filters, p);
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
