import { test, expect } from '@playwright/test';
import { coalesce, debounce } from '../src/lib/coalesce';
import { PAGE_SIZE, readAllPages } from '../src/lib/paging';

const sleep = (ms: number): Promise<void> => new Promise((resolve) => setTimeout(resolve, ms));

test('coalesce runs a burst once, after the delay', async () => {
  let runs = 0;
  const soon = coalesce(() => (runs += 1), 40);
  soon();
  soon();
  soon();
  expect(runs).toBe(0);
  await sleep(120);
  expect(runs).toBe(1);
});

test('a leading coalesce runs at once and once more for calls inside its window', async () => {
  let runs = 0;
  const soon = coalesce(() => (runs += 1), 60, { leading: true });
  soon();
  expect(runs).toBe(1);
  soon();
  soon();
  expect(runs).toBe(1);
  await sleep(160);
  expect(runs).toBe(2);
});

test('cancel drops a pending run', async () => {
  let runs = 0;
  const soon = coalesce(() => (runs += 1), 40);
  soon();
  soon.cancel();
  await sleep(100);
  expect(runs).toBe(0);
});

test('debounce runs with the latest value once calls pause', async () => {
  const seen: string[] = [];
  const typed = debounce((value: string) => seen.push(value), 50);
  typed('a');
  await sleep(20);
  typed('ab');
  await sleep(20);
  typed('abc');
  await sleep(150);
  expect(seen).toEqual(['abc']);
  typed('x');
  typed.cancel();
  await sleep(100);
  expect(seen).toEqual(['abc']);
});

test('readAllPages reads every page once and reports the total', async () => {
  const rows = Array.from({ length: PAGE_SIZE + 5 }, (_, id) => ({ id }));
  const asked: number[] = [];
  const all = await readAllPages(
    (limit, offset) => {
      asked.push(offset);
      return Promise.resolve({ items: rows.slice(offset, offset + limit), total: rows.length });
    },
    (r) => r.id
  );
  expect(asked).toEqual([0, PAGE_SIZE]);
  expect(all.items).toHaveLength(rows.length);
  expect(all.total).toBe(rows.length);
});
