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
