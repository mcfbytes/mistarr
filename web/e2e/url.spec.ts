import { test, expect, type Page } from '@playwright/test';

const toasts = (page: Page) => page.locator('.toasts');

async function expectUrlField(page: Page): Promise<void> {
  const input = page.getByLabel('Add from a URL');
  await expect(input).toBeVisible();
  await expect(input).toHaveAttribute('autocomplete', 'off');
  await expect(input).toHaveAttribute('placeholder', 'https://example.invalid/…');
  await expect(page.locator('form.url')).toHaveAttribute('autocomplete', 'off');
  await expect(page.locator('datalist')).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Fetch' })).toBeVisible();
}

test('the URL field is on DATs, Sources and both wizard steps', async ({ page }) => {
  await page.goto('/#/dats');
  await expectUrlField(page);
  await page.goto('/#/sources');
  await expectUrlField(page);
  await page.goto('/#/wizard');
  await page.getByRole('button', { name: 'Next' }).click();
  await expectUrlField(page);
  await page.getByRole('button', { name: 'Next' }).click();
  await page.getByRole('button', { name: 'Next' }).click();
  await expectUrlField(page);
});

test('a fetch shows its progress in the activity panel and a toast when it lands', async ({ page }) => {
  await page.goto('/#/dats');
  await page.getByLabel('Add from a URL').fill('https://example.invalid/dats/Example.dat');
  await page.getByRole('button', { name: 'Fetch' }).click();
  await expect(toasts(page).getByText('Fetching the file. Its progress is under Background work.')).toBeVisible();
  await expect(page.getByLabel('Add from a URL')).toHaveValue('');
  await page.getByRole('button', { name: /^Background work:/ }).click();
  const running = page.getByRole('region', { name: 'Background work' }).getByRole('list', { name: 'Running' });
  const row = running.getByRole('listitem').filter({ hasText: 'URL fetch: Example.dat' });
  await expect(row).toBeVisible();
  await expect(row.getByRole('progressbar')).toBeVisible();
  await expect(row).toContainText(/Receiving · \d+% · [\d.]+ MiB of 2\.3 MiB/);
  await expect(row.getByRole('button', { name: 'Cancel URL fetch: Example.dat' })).toBeVisible();
  await expect(row.getByRole('link')).toHaveAttribute('href', '#/activity');
  await expect(toasts(page).getByText('DAT received: Example.dat. Queued.')).toBeVisible({ timeout: 10_000 });
  await expect(row).toHaveCount(0);
});

test('a page that is not a DAT or torrent is refused with a toast', async ({ page }) => {
  await page.goto('/#/sources');
  await page.getByLabel('Add from a URL').fill('https://example.invalid/index.html');
  await page.getByRole('button', { name: 'Fetch' }).click();
  await expect(
    toasts(page).getByText("The fetch failed: This isn't a DAT, DAT pack or torrent file.")
  ).toBeVisible({ timeout: 10_000 });
});

test('a bad link is refused at once and a magnet is placed', async ({ page }) => {
  await page.goto('/#/sources');
  const input = page.getByLabel('Add from a URL');
  await input.fill('ftp://example.invalid/a.dat');
  await page.getByRole('button', { name: 'Fetch' }).click();
  await expect(toasts(page).getByText('Only http, https and magnet links are accepted.')).toBeVisible();
  await expect(input).toHaveValue('ftp://example.invalid/a.dat');
  await input.fill('magnet:?xt=urn:btih:example');
  await page.getByRole('button', { name: 'Fetch' }).click();
  await expect(toasts(page).getByText(/^Magnet received: Example magnet\.magnet\./)).toBeVisible();
});

test('a running fetch can be cancelled from the panel', async ({ page }) => {
  await page.goto('/#/dats');
  await page.getByLabel('Add from a URL').fill('https://example.invalid/slow.zip');
  await page.getByRole('button', { name: 'Fetch' }).click();
  await page.getByRole('button', { name: /^Background work:/ }).click();
  const panel = page.getByRole('region', { name: 'Background work' });
  await panel.getByRole('button', { name: /^Cancel URL fetch/ }).click();
  await expect(toasts(page).getByText('The fetch was cancelled.')).toBeVisible();
});

test('no link is remembered after a reload', async ({ page }) => {
  const link = 'https://example.invalid/kept/Example.dat';
  await page.goto('/#/dats');
  await page.getByLabel('Add from a URL').fill(link);
  await page.getByRole('button', { name: 'Fetch' }).click();
  await expect(toasts(page).getByText('Fetching the file. Its progress is under Background work.')).toBeVisible();
  await page.getByLabel('Add from a URL').fill('https://example.invalid/typed-not-sent.dat');
  await page.reload();
  await expect(page.getByLabel('Add from a URL')).toHaveValue('');
  const stored = await page.evaluate(() =>
    [localStorage, sessionStorage]
      .flatMap((store) => Object.keys(store).map((key) => `${key}=${store.getItem(key) ?? ''}`))
      .join('\n')
  );
  expect(stored).not.toContain('example.invalid');
});

test('unnamed fetches are told apart without the URL and a cancel shows it is pending', async ({ page }) => {
  await page.goto('/#/dats');
  for (const link of ['https://example.invalid/wait-one', 'https://example.invalid/wait-two']) {
    await page.getByLabel('Add from a URL').fill(link);
    await page.getByRole('button', { name: 'Fetch' }).click();
    await expect(page.getByLabel('Add from a URL')).toHaveValue('');
  }
  await page.getByRole('button', { name: /^Background work:/ }).click();
  const panel = page.getByRole('region', { name: 'Background work' });
  const titles = panel.getByRole('link', { name: /^URL fetch: sent at / });
  await expect(titles).toHaveCount(2);
  const [first, second] = await titles.allTextContents();
  expect(first).not.toEqual(second);
  expect(`${first} ${second}`).not.toMatch(/example|wait/);
  const cancel = panel.getByRole('button', { name: `Cancel ${first}` });
  await cancel.click();
  const pending = panel.getByRole('button', { name: `Cancelling ${first}` });
  await expect(pending).toBeDisabled();
  await expect(pending).toHaveText('Cancelling…');
  await expect(toasts(page).getByText('The fetch was cancelled.')).toBeVisible();
});
