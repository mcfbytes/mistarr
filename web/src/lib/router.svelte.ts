export type RouteName =
  | 'wizard'
  | 'platforms'
  | 'browse'
  | 'title'
  | 'activity'
  | 'sources'
  | 'source'
  | 'dats'
  | 'system'
  | 'notfound';

export interface Route {
  name: RouteName;
  params: Record<string, string>;
}

/** Path heads and the screens they open; a `param` row needs a second segment and is tried first. */
const ROUTES: { head: string; param: boolean; name: RouteName }[] = [
  { head: 'wizard', param: false, name: 'wizard' },
  { head: 'activity', param: false, name: 'activity' },
  { head: 'sources', param: true, name: 'source' },
  { head: 'sources', param: false, name: 'sources' },
  { head: 'dats', param: false, name: 'dats' },
  { head: 'system', param: false, name: 'system' },
  { head: 'p', param: true, name: 'browse' },
  { head: 't', param: true, name: 'title' }
];

function parseHash(hash: string): Route {
  const segments = hash.replace(/^#/, '').split('/').filter(Boolean);
  const [head, id] = segments;
  if (head === undefined) {
    return { name: 'platforms', params: {} };
  }
  const row = ROUTES.find((r) => r.head === head && (!r.param || id));
  return { name: row?.name ?? 'notfound', params: row?.param && id ? { id } : {} };
}

function currentHash(): string {
  return typeof window === 'undefined' ? '#/' : window.location.hash || '#/';
}

let route = $state<Route>(parseHash(currentHash()));
let leaveGuard: (() => boolean) | null = null;
let restoring = false;
let shownAt = 0;

/** When the current history entry was first shown, from its state; NaN for a new entry. */
function entryTime(): number {
  const state: unknown = window.history.state;
  if (state && typeof state === 'object' && 'mistarrShownAt' in state && typeof state.mistarrShownAt === 'number') {
    return state.mistarrShownAt;
  }
  return NaN;
}

/** Stamps the current entry, so a later return to it reads as Back or Forward. */
function stampEntry(): void {
  let at = entryTime();
  if (Number.isNaN(at)) {
    at = performance.timeOrigin + performance.now();
    const state: unknown = window.history.state;
    const base = state && typeof state === 'object' ? state : {};
    window.history.replaceState({ ...base, mistarrShownAt: at }, '');
  }
  shownAt = at;
}

function onHashChange(): void {
  const next = parseHash(currentHash());
  if (restoring) {
    restoring = false;
    return;
  }
  if (leaveGuard && next.name !== route.name && !leaveGuard()) {
    // Step back to the entry that was showing; its hashchange is then ignored.
    restoring = true;
    if (entryTime() < shownAt) {
      window.history.forward();
    } else {
      window.history.back();
    }
    return;
  }
  stampEntry();
  route = next;
}

if (typeof window !== 'undefined') {
  stampEntry();
  window.addEventListener('hashchange', onHashChange);
}

/**
 * Asks `guard` before any navigation to another screen, by link, Back, Forward or an edited
 * address; a false answer stays on the current screen. `null` removes it.
 */
export function setLeaveGuard(guard: (() => boolean) | null): void {
  leaveGuard = guard;
}

export function getRoute(): Route {
  return route;
}

export function navigate(path: string): void {
  if (typeof window === 'undefined') {
    return;
  }
  window.location.hash = path.startsWith('#') ? path : `#${path}`;
}

/** The address of the platform list. */
export const HOME_URL = '#/';

/** The address of a screen that takes no parameter. */
export function pageUrl(page: 'wizard' | 'activity' | 'sources' | 'dats' | 'system'): string {
  return `#/${page}`;
}

export function platformUrl(id: string): string {
  return `#/p/${id}`;
}

export function titleUrl(id: number): string {
  return `#/t/${id}`;
}

export function sourceUrl(id: number): string {
  return `#/sources/${id}`;
}
