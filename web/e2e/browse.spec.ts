import { expect, test, type Page } from '@playwright/test';

/** Sets the mock search latency per query before the app starts; see `mockDelayMs`. */
async function delays(page: Page, byQuery: Record<string, number>): Promise<void> {
  await page.addInitScript((value) => {
    localStorage.setItem('mistarr.mockDelayMs', value);
  }, JSON.stringify(byQuery));
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

  // Past the slow answer's due time the grid still shows the newer search.
  await page.waitForTimeout(2000);
  for (const name of await names(page).allTextContents()) {
    expect(name).toBe('Mock Manor (USA)');
  }
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

  await page.evaluate(() => localStorage.setItem('mistarr.mockDelayMs', '{"#1": -1}'));
  await page.reload();
  await expect(names(page).first()).toBeVisible();
  await page.mouse.wheel(0, 50_000);
  await expect(page.getByRole('alert')).toContainText('Titles could not be loaded');

  await page.evaluate(() => localStorage.removeItem('mistarr.mockDelayMs'));
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
  // A scan's file.changed events ask for reloads faster than this search answers.
  await page.evaluate(() => {
    const w = window as unknown as { mistarrReloadTitles: () => Promise<void> };
    const timer = setInterval(() => void w.mistarrReloadTitles(), 500);
    setTimeout(() => clearInterval(timer), 10_000);
  });
  await expect(names(page).first()).toHaveText('Mock Manor (USA)', { timeout: 5000 });
  await expect(page.getByRole('progressbar', { name: 'Loading titles' })).toHaveCount(0);
});

test('a background reload stops when the user searches', async ({ page }) => {
  await page.goto('/#/p/nes');
  await expect(names(page).first()).toBeVisible();
  await scrollForMore(page, 120);

  await page.evaluate(() => {
    localStorage.setItem('mistarr.mockDelayMs', '{"": 1000}');
    const w = window as unknown as { mistarrReloadTitles: () => Promise<void> };
    void w.mistarrReloadTitles();
  });
  await page.getByPlaceholder('Search').fill('Mock');
  await expect(names(page).first()).toHaveText('Mock Manor (USA)');

  // Past the reload's due time for every page it had, only the search's rows show.
  await page.waitForTimeout(2500);
  for (const name of await names(page).allTextContents()) {
    expect(name).toBe('Mock Manor (USA)');
  }
});
