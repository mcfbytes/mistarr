import { test, expect, type Page } from '@playwright/test';

const torrent = { name: 'Example bundle four.torrent', mimeType: 'application/x-bittorrent', buffer: Buffer.from('d1:ae') };

function indicator(page: Page) {
  return page.getByRole('button', { name: /^Background work:/ });
}

test('a torrent upload shows it is sending, then a toast saying it was received', async ({ page }) => {
  await page.goto('/#/sources');
  const input = page.getByLabel('Add a .torrent file');
  await input.setInputFiles(torrent);
  await expect(page.getByText('Uploading Example bundle four.torrent…')).toBeVisible();
  await expect(input).toBeDisabled();
  const toast = page.getByText(
    /^Torrent received: Example bundle four\.torrent\. Waiting for the DAT import of Example Console \(20260101\)\.zip to finish\.$/
  );
  await expect(toast).toBeVisible();
  await expect(input).toBeEnabled();
  const row = page
    .getByRole('list', { name: 'Files in sources' })
    .getByRole('listitem')
    .filter({ hasText: 'Example bundle four.torrent' });
  await expect(row.locator('[data-status="waiting"]')).toHaveText('Waiting');
  await expect(row).toContainText('Waiting for the DAT import of Example Console (20260101).zip to finish.');
});

test('the magnet Add button shows its pending state', async ({ page }) => {
  await page.goto('/#/sources');
  await page.getByLabel('Or a magnet link').fill('magnet:?xt=urn:btih:example');
  const add = page.getByRole('button', { name: 'Add', exact: true });
  await add.click();
  const pending = page.getByRole('button', { name: 'Adding…' });
  await expect(pending).toBeDisabled();
  await expect(page.getByText(/^Magnet received: Example magnet\.magnet\./)).toBeVisible();
  await expect(add).toBeEnabled();
  await expect(page.getByLabel('Or a magnet link')).toHaveValue('');
});

test('the nav indicator counts the work and its panel lists it', async ({ page }) => {
  await page.goto('/#/');
  const button = indicator(page);
  await expect(button).toHaveAccessibleName('Background work: 1 running, 2 waiting');
  await expect(button).toHaveAttribute('aria-expanded', 'false');
  await button.click();
  const panel = page.getByRole('region', { name: 'Background work' });
  await expect(panel).toBeVisible();
  await expect(button).toHaveAttribute('aria-expanded', 'true');
  const running = panel.getByRole('list', { name: 'Running' });
  await expect(running.getByRole('link', { name: 'DAT import: Example Console (20260101).zip' })).toHaveAttribute(
    'href',
    '#/dats'
  );
  await expect(running.getByRole('progressbar')).toBeVisible();
  const waiting = panel.getByRole('list', { name: 'Waiting' });
  const source = waiting.getByRole('listitem').filter({ hasText: 'Source import: Example bundle two.torrent' });
  await expect(source.locator('[data-status="waiting"]')).toBeVisible();
  await expect(source).toContainText('Waiting for the DAT import of Example Console (20260101).zip to finish.');
  const scan = waiting.getByRole('listitem').filter({ hasText: 'Scan: Nintendo Entertainment System' });
  await expect(scan.locator('[data-status="paused"]')).toBeVisible();
  await expect(scan).toContainText('Paused while FCEUmm is running');
  const finished = panel.getByRole('list', { name: 'Finished' });
  await expect(finished.locator('[data-status="failed"]')).toBeVisible();
  await expect(finished).toContainText('DAT Example Handheld (20260101).xml failed');
  await page.mouse.click(5, 400);
  await expect(panel).toBeHidden();
});

test('the indicator is quiet with no work', async ({ page }) => {
  await page.goto('/?mock=idle#/');
  await expect(indicator(page)).toHaveAccessibleName('Background work: nothing running');
  await indicator(page).click();
  await expect(page.getByText('Nothing is running or waiting.')).toBeVisible();
});

test('the activity panel works from the keyboard', async ({ page }) => {
  await page.goto('/#/sources');
  const button = indicator(page);
  await button.focus();
  await page.keyboard.press('Enter');
  const panel = page.getByRole('region', { name: 'Background work' });
  await expect(panel).toBeFocused();
  await page.keyboard.press('Tab');
  await expect(panel.getByRole('link', { name: 'DAT import: Example Console (20260101).zip' })).toBeFocused();
  await page.keyboard.press('Escape');
  await expect(panel).toBeHidden();
  await expect(button).toBeFocused();
  await page.keyboard.press('Space');
  await expect(panel).toBeFocused();
  await page.keyboard.press('Shift+Tab');
  await expect(button).toBeFocused();
  await page.keyboard.press('Enter');
  await expect(panel).toBeHidden();
});

test('a DAT import shows a bar that moves', async ({ page }) => {
  await page.goto('/#/dats');
  const bar = page.getByRole('progressbar', { name: 'Example Console (20260101).zip import progress' });
  await expect(bar).toHaveAttribute('aria-valuetext', /^Reading games · \d+% · [\d,]+ games$/);
  const first = Number(await bar.getAttribute('aria-valuenow'));
  await expect.poll(async () => Number(await bar.getAttribute('aria-valuenow'))).toBeGreaterThan(first);
});

test('status pills show on Sources and Activity', async ({ page }) => {
  await page.goto('/#/sources');
  const table = page.getByRole('table');
  await expect(table.locator('[data-status]').first()).toBeVisible();
  const states = await table.locator('[data-status]').allTextContents();
  expect(states.length).toBeGreaterThan(0);
  for (const s of states) {
    expect(['Resolving', 'Unbound', 'Bound', 'Disabled']).toContain(s.trim());
  }

  await page.goto('/#/activity');
  const jobs = page.locator('.job');
  await expect(jobs.filter({ hasText: 'DAT import' }).locator('[data-status="running"]')).toHaveText('Running');
  await expect(jobs.filter({ hasText: 'Source import' }).locator('[data-status="waiting"]')).toHaveText('Waiting');
  await expect(jobs.filter({ hasText: 'Scan' }).locator('[data-status="paused"]')).toHaveText('Paused');
  await expect(page.locator('[data-status="failed"]').first()).toBeVisible();
  await expect(page.locator('[data-status="done"]').first()).toBeVisible();
});
