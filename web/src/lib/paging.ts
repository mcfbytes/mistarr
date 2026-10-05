import type { Paged } from './types';

/** The server's largest page size for a single request (see docs/API.md). */
export const PAGE_SIZE = 1000;

/**
 * Reads every page of a paginated endpoint via `fetchPage(limit, offset)`,
 * keyed by `keyOf` so a row that shifts pages while the read is in flight
 * appears once. Stops once a short page lands or `total` rows arrived; `total`
 * is the count the last page reported.
 */
export async function readAllPages<T>(
  fetchPage: (limit: number, offset: number) => Promise<Paged<T>>,
  keyOf: (item: T) => string | number
): Promise<Paged<T>> {
  const byKey = new Map<string | number, T>();
  let offset = 0;
  let page: Paged<T>;
  do {
    page = await fetchPage(PAGE_SIZE, offset);
    for (const item of page.items) {
      byKey.set(keyOf(item), item);
    }
    offset += page.items.length;
  } while (page.items.length > 0 && offset < page.total);
  return { items: [...byKey.values()], total: page.total };
}
