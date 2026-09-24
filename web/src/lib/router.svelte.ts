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

if (typeof window !== 'undefined') {
  window.addEventListener('hashchange', () => {
    route = parseHash(currentHash());
  });
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
