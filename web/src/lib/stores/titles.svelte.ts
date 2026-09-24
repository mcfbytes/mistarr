import { api } from '../api';
import { fixtureTitle, fixtureTitles } from '../fixtures';
import type { TitleDetail, TitleFilters, TitleGroup } from '../types';

const isMock = import.meta.env.VITE_MOCK === '1';
const PAGE_SIZE = 60;

let groups = $state<TitleGroup[]>([]);
let groupsTotal = $state(0);
let groupsPlatform = $state<string | null>(null);
let detail = $state<TitleDetail | null>(null);

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
  const res = await api.titles(platformId, filters, PAGE_SIZE, offset);
  groups = page === 0 ? res.items : [...groups, ...res.items];
  groupsTotal = res.total;
}

export async function loadTitleDetail(id: number): Promise<void> {
  detail = isMock ? fixtureTitle(id) : await api.title(id);
}

export function patchGroup(parentId: number, patch: Partial<TitleGroup>): void {
  groups = groups.map((g) => (g.parent_id === parentId ? { ...g, ...patch } : g));
}
