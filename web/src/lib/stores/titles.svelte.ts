import { api } from '../api';
import { fixtureTitle, fixtureTitles } from '../fixtures';
import type { TitleDetail, TitleFilters, TitleGroup } from '../types';

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
  if (groupsPlatform) {
    await loadTitlesPage(groupsPlatform, lastFilters, 0);
    for (let p = 1; p <= lastPage; p += 1) {
      await loadTitlesPage(groupsPlatform, lastFilters, p);
    }
  }
  if (detailId !== null) {
    await loadTitleDetail(detailId);
  }
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
