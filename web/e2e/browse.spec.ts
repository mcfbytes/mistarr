import { expect, test, type Page } from '@playwright/test';
import { MOCK_EVENT } from '../src/mock/events';
import { emitEvent, setMockKnob } from './helpers';

/** Sets the mock search latency per query; see `delayMs` in `src/mock/fixtures.ts`. */
async function delays(page: Page, byQuery: Record<string, number> | null): Promise<void> {
  await setMockKnob(page, 'delayMs', byQuery);
}

/** A file's state changed, as a scan reports it; the grid reloads its pages. */
async function fileChanged(page: Page): Promise<void> {
  await emitEvent(page, { name: 'file.changed', data: { file_id: 0, state: 'verified' } });
}

function names(page: Page) {
  return page.locator('.grid .name');
}

test('a slow search shows a busy bar and dims the old results until it lands', async ({ page }) => {
  await delays(page, { Sample: 1500 });
  await page.goto('/#/p/nes');
  await expect(names(page).first()).toBeVisible();
  const busy = page.getByRole('progressbar', { name: 'Loading titles' });
  await expect(busy).toHaveCount(0);

  await page.getByPlaceholder('Search').fill('Sample');

  await expect(busy).toBeVisible();
  await expect(page.locator('.grid')).toHaveAttribute('aria-busy', 'true');
  await expect(names(page).first()).toBeVisible();
  await expect(busy).toHaveCount(0, { timeout: 5000 });
  await expect(page.locator('.grid')).toHaveAttribute('aria-busy', 'false');
  await expect(names(page).first()).toHaveText('Sample Racer (USA)');
  for (const name of await names(page).allTextContents()) {
    expect(name).toContain('Sample');
  }
});

test('a newer search wins over a slower one still on its way', async ({ page }) => {
  await delays(page, { Example: 1500, Mock: 0 });
  await page.goto('/#/p/nes');
  await expect(names(page).first()).toBeVisible();
  const search = page.getByPlaceholder('Search');
  const busy = page.getByRole('progressbar', { name: 'Loading titles' });

  await search.fill('Example');
  await expect(busy).toBeVisible();
  await search.fill('Mock');
  await expect(names(page).first()).toHaveText('Mock Manor (USA)');
  await expect(busy).toHaveCount(0);

  // The newer search cut the slow one short, whose answer landed first and was dropped.
  await expect(names(page).filter({ hasNotText: 'Mock Manor (USA)' })).toHaveCount(0);
});

test('a failed search says so and offers Retry', async ({ page }) => {
  await delays(page, { Trial: -10 });
  await page.goto('/#/p/nes');
  await expect(names(page).first()).toBeVisible();

  await page.getByPlaceholder('Search').fill('Trial');

  const alert = page.getByRole('alert');
  await expect(alert).toContainText('Titles could not be loaded');
  await expect(alert.getByRole('button', { name: 'Retry' })).toBeVisible();
  await expect(page.getByText('No titles match.')).toHaveCount(0);
});

function ids(page: Page): Promise<string[]> {
  return page
    .locator('.grid a.poster')
    .evaluateAll((els) => els.map((e) => e.getAttribute('href') ?? ''));
}

async function scrollForMore(page: Page, count: number): Promise<void> {
  await expect(async () => {
    await page.mouse.wheel(0, 50_000);
    expect(await page.locator('.grid a.poster').count()).toBeGreaterThanOrEqual(count);
  }).toPass({ timeout: 5000 });
}

test('a failed next page is loaded again after Retry, never skipped', async ({ page }) => {
  await page.goto('/#/p/nes');
  await expect(names(page).first()).toBeVisible();
  await scrollForMore(page, 120);
  const expected = (await ids(page)).slice(0, 120);

  await delays(page, { '#1': -1 });
  await page.reload();
  await expect(names(page).first()).toBeVisible();
  await page.mouse.wheel(0, 50_000);
  await expect(page.getByRole('alert')).toContainText('Titles could not be loaded');

  await delays(page, null);
  await page.getByRole('button', { name: 'Retry' }).click();
  await expect(page.getByRole('alert')).toHaveCount(0);
  await scrollForMore(page, 120);
  expect((await ids(page)).slice(0, 120)).toEqual(expected);
});

test('a slow search lands while background reloads keep arriving', async ({ page }) => {
  await delays(page, { Mock: 1500 });
  await page.goto('/#/p/nes');
  await expect(names(page).first()).toBeVisible();

  await page.getByPlaceholder('Search').fill('Mock');
  // Resyncs ask for reloads faster than this search answers.
  await page.evaluate((name) => {
    const resync = () => window.dispatchEvent(new CustomEvent(name, { detail: { name: 'resync', data: {} } }));
    const timer = setInterval(resync, 500);
    setTimeout(() => clearInterval(timer), 10_000);
  }, MOCK_EVENT);
  await expect(names(page).first()).toHaveText('Mock Manor (USA)', { timeout: 5000 });
  await expect(page.getByRole('progressbar', { name: 'Loading titles' })).toHaveCount(0);
});

test('a reload asked for during a load runs once that load has landed', async ({ page }) => {
  await delays(page, { Mock: 1500 });
  await page.goto('/#/p/nes');
  await expect(names(page).first()).toBeVisible();

  await page.getByPlaceholder('Search').fill('Mock');
  await expect(page.getByRole('progressbar', { name: 'Loading titles' })).toBeVisible();
  // The search already read its delay; the reload's own request will fail, which shows.
  await delays(page, { Mock: -1 });
  await fileChanged(page);
  const alert = page.getByRole('alert');
  await expect(alert).toHaveCount(0);
  // Had the reload cut the search short, its rows would never land.
  await expect(names(page).first()).toHaveText('Mock Manor (USA)', { timeout: 5000 });
  await expect(alert).toContainText('Titles could not be loaded');
});

test('scrolling on after a reload loads the next page with no gap', async ({ page }) => {
  await page.goto('/#/p/nes');
  await expect(names(page).first()).toBeVisible();
  await scrollForMore(page, 180);
  const expected = await ids(page);

  await page.reload();
  await expect(names(page).first()).toBeVisible();
  await scrollForMore(page, 120);
  // Page 0 reloads at once and page 1 slowly, while the grid sits scrolled to its end.
  await delays(page, { '#1': 1500 });
  await fileChanged(page);
  await scrollForMore(page, 180);
  await delays(page, null);
  const got = await ids(page);
  expect(got.slice(0, 180)).toEqual(expected.slice(0, 180));
});

async function scrollToEnd(page: Page): Promise<string[]> {
  await expect(async () => {
    await page.mouse.wheel(0, 50_000);
    expect(await page.locator('.sentinel').count()).toBe(0);
  }).toPass({ timeout: 5000 });
  return ids(page);
}

function duplicates(list: string[]): string[] {
  return list.filter((id, i) => list.indexOf(id) !== i);
}

test('a reload after a row left the list never shows a group twice or skips one', async ({ page }) => {
  await page.goto('/#/p/nes');
  await expect(names(page).first()).toBeVisible();
  await scrollForMore(page, 120);
  const removed = Number((await ids(page))[4]?.split('/').pop());
  await setMockKnob(page, 'removedIds', [removed]);

  // The list as a fresh visit sees it, from a second tab sharing the storage.
  const other = await page.context().newPage();
  await other.goto('/#/p/nes');
  await expect(names(other).first()).toBeVisible();
  const fresh = await scrollToEnd(other);
  expect(fresh.some((id) => id.endsWith(`/${removed}`))).toBe(false);

  // Page 0 reloads at once and page 1 slowly; the grid is then the fresh list's start.
  await delays(page, { '#1': 1500 });
  await fileChanged(page);
  await expect(page.locator(`.grid a.poster[href$="/${removed}"]`)).toHaveCount(0);
  const during = await ids(page);
  expect(during.length).toBeGreaterThanOrEqual(119);
  expect(during).toEqual(fresh.slice(0, during.length));

  // Scrolling on cuts the reload short; what follows must still end at the fresh list.
  await delays(page, null);
  await expect(async () => {
    const shown = await scrollToEnd(page);
    expect(duplicates(shown)).toEqual([]);
    expect(shown).toEqual(fresh);
  }).toPass({ timeout: 10_000 });
});

test('a background reload stops when the user searches', async ({ page }) => {
  await page.goto('/#/p/nes');
  await expect(names(page).first()).toBeVisible();
  await scrollForMore(page, 120);

  await delays(page, { '': 1000 });
  await fileChanged(page);
  await page.getByPlaceholder('Search').fill('Mock');
  await expect(names(page).first()).toHaveText('Mock Manor (USA)');

  // The search cut the reload short, whose pages never land over its rows.
  await expect(page.getByRole('progressbar', { name: 'Loading titles' })).toHaveCount(0);
  await expect(names(page).filter({ hasNotText: 'Mock Manor (USA)' })).toHaveCount(0);
});
