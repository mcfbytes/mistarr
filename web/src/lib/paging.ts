import type { Paged } from './types';

/** The server's largest page size for a single request (see docs/API.md). */
export const PAGE_SIZE = 1000;

/**
 * Reads every page of a paginated endpoint via `fetchPage(limit, offset)`,
 * keyed by `keyOf` so a row that shifts pages while the read is in flight
 * appears once. Stops once a short page lands or `total` rows arrived.
 */
export async function readAllPages<T>(
  fetchPage: (limit: number, offset: number) => Promise<Paged<T>>,
  keyOf: (item: T) => string | number
): Promise<T[]> {
  const byKey = new Map<string | number, T>();
  let offset = 0;
  for (;;) {
    const page = await fetchPage(PAGE_SIZE, offset);
    for (const item of page.items) {
      byKey.set(keyOf(item), item);
    }
    offset += page.items.length;
    if (page.items.length === 0 || offset >= page.total) {
      break;
    }
  }
  return [...byKey.values()];
}
