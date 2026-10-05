import { errorMessage } from '../api';
import { coalesce, DELAY_MS, type Coalesced } from '../coalesce';
import { readAllPages } from '../paging';
import type { Paged } from '../types';

/** How a `ListStore` orders, refreshes and reacts to what it reads. */
export interface ListOptions<T> {
  sort?: (a: T, b: T) => number;
  /** The window `reloadSoon` merges calls in; one re-read per window, at its end unless `leading`. */
  reload?: { ms: number; leading?: boolean };
  onLoaded?: (items: readonly T[]) => void;
}

/**
 * Every row of a paginated endpoint, kept in memory. `error` holds why the first read
 * failed and clears when one succeeds; a failed re-read keeps the rows already held.
 */
export class ListStore<T> {
  items = $state.raw<T[]>([]);
  total = $state(0);
  loaded = $state(false);
  error = $state<string | null>(null);
  private readonly fetchPage: (limit: number, offset: number) => Promise<Paged<T>>;
  private readonly keyOf: (item: T) => string | number;
  private readonly options: ListOptions<T>;
  private readonly soon: Coalesced;
  private reading: Promise<void> | null = null;
  private again: Promise<void> | null = null;

  constructor(
    fetchPage: (limit: number, offset: number) => Promise<Paged<T>>,
    keyOf: (item: T) => string | number,
    options: ListOptions<T> = {}
  ) {
    this.fetchPage = fetchPage;
    this.keyOf = keyOf;
    this.options = options;
    const { ms, leading } = options.reload ?? { ms: DELAY_MS.reload };
    this.soon = coalesce(() => void this.load(), ms, { leading: leading ?? false });
  }

  /**
   * Reads every page; never throws, a failure lands in `error` while nothing has loaded.
   * It resolves with rows read after the call: a read already under way is followed by one more.
   */
  load(): Promise<void> {
    if (!this.reading) {
      this.reading = this.read().finally(() => {
        this.reading = null;
      });
      return this.reading;
    }
    this.again ??= this.reading.then(() => {
      this.again = null;
      return this.load();
    });
    return this.again;
  }

  /** Reads the list once, if it has not been read. */
  ensure(): Promise<void> {
    return this.loaded ? Promise.resolve() : this.load();
  }

  /** Merges `change` into the row with `key`, or drops the row when `change` is null. */
  patch(key: string | number, change: Partial<T> | null): void {
    this.items =
      change === null
        ? this.items.filter((i) => this.keyOf(i) !== key)
        : this.items.map((i) => (this.keyOf(i) === key ? { ...i, ...change } : i));
  }

  /** Re-reads the list once for a burst of calls. */
  reloadSoon(): void {
    this.soon();
  }

  private async read(): Promise<void> {
    try {
      const page = await readAllPages(this.fetchPage, this.keyOf);
      this.items = this.options.sort ? page.items.sort(this.options.sort) : page.items;
      this.total = page.total;
      this.loaded = true;
      this.error = null;
      this.options.onLoaded?.(this.items);
    } catch (err) {
      if (!this.loaded) {
        this.error = errorMessage(err);
      }
    }
  }
}
