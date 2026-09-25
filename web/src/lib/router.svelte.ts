export type RouteName =
  | 'wizard'
  | 'platforms'
  | 'browse'
  | 'title'
  | 'activity'
  | 'sources'
  | 'dats'
  | 'system'
  | 'notfound';

export interface Route {
  name: RouteName;
  params: Record<string, string>;
}

function parseHash(hash: string): Route {
  const path = hash.replace(/^#/, '') || '/';
  const segments = path.split('/').filter(Boolean);

  if (segments.length === 0) {
    return { name: 'platforms', params: {} };
  }
  if (segments[0] === 'wizard') {
    return { name: 'wizard', params: {} };
  }
  if (segments[0] === 'activity') {
    return { name: 'activity', params: {} };
  }
  if (segments[0] === 'sources') {
    return { name: 'sources', params: {} };
  }
  if (segments[0] === 'dats') {
    return { name: 'dats', params: {} };
  }
  if (segments[0] === 'system') {
    return { name: 'system', params: {} };
  }
  if (segments[0] === 'p' && segments[1]) {
    return { name: 'browse', params: { id: segments[1] } };
  }
  if (segments[0] === 't' && segments[1]) {
    return { name: 'title', params: { id: segments[1] } };
  }
  return { name: 'notfound', params: {} };
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

export function platformUrl(id: string): string {
  return `#/p/${id}`;
}

export function titleUrl(id: number): string {
  return `#/t/${id}`;
}
