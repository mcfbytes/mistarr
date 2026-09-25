import { test, expect, type Page } from '@playwright/test';

test.use({ permissions: ['clipboard-read', 'clipboard-write'] });

async function clipboard(page: Page): Promise<string> {
  return page.evaluate(() => navigator.clipboard.readText());
}

test('the status tiles show the board, client, scheduler, memory, storage and uptime', async ({ page }) => {
  await page.goto('/#/system');
  const tiles = page.getByRole('list', { name: 'Status' }).getByRole('listitem');
  await expect(tiles).toHaveCount(6);
  await expect(tiles.filter({ hasText: 'MiSTer' })).toContainText('FCEUmm');
  await expect(tiles.filter({ hasText: 'MiSTer' })).toContainText('Launching: Ready');
  const client = tiles.filter({ hasText: 'Download client' });
  await expect(client).toContainText('rtorrent');
  await expect(client.locator('[data-status="done"]')).toHaveText('Reachable');
  await expect(client).toContainText('scgi://127.0.0.1:5000');
  const scheduler = tiles.filter({ hasText: 'Scheduler' });
  await expect(scheduler.locator('[data-status="paused"]')).toHaveText('Held for the core');
  await expect(scheduler).toContainText('1 job waiting');
  await expect(scheduler.getByRole('button', { name: 'Run now' })).toBeVisible();
  await expect(tiles.filter({ hasText: 'Memory' })).toContainText('41 MB used by mistarr');
  await expect(page.getByRole('meter', { name: 'Board memory in use' })).toHaveAttribute(
    'aria-valuetext',
    '214 MB available of 507 MB'
  );
  await expect(page.getByRole('meter', { name: 'Storage in use' })).toHaveAttribute('aria-valuenow', '61');
  await expect(tiles.filter({ hasText: 'Uptime' })).toContainText('1 h 15 min');
});

test('at the menu with nothing held, the board and scheduler say so', async ({ page }) => {
  await page.goto('/?mock=showcase#/system');
  const tiles = page.getByRole('list', { name: 'Status' }).getByRole('listitem');
  await expect(tiles.filter({ hasText: 'MiSTer' })).toContainText('At the menu');
  const scheduler = tiles.filter({ hasText: 'Scheduler' });
  await expect(scheduler.locator('[data-status="running"]')).toHaveText('Running');
  await expect(scheduler).toContainText('Nothing waiting');
  await expect(scheduler.getByRole('button', { name: 'Pause' })).toBeVisible();
});

test('the version copies, and a release links its notes', async ({ page }) => {
  await page.goto('/#/system');
  const about = page.getByRole('region', { name: 'About' });
  await expect(about.getByText('0.3.0-dev+1a2b3c4')).toBeVisible();
  await expect(about.getByText('Development build')).toBeVisible();
  await expect(about.getByRole('link')).toHaveCount(0);
  await about.getByRole('button', { name: 'Copy version' }).click();
  expect(await clipboard(page)).toBe('0.3.0-dev+1a2b3c4');
  await expect(page.locator('.toasts').getByText('Version 0.3.0-dev+1a2b3c4 copied.')).toBeVisible();

  await page.goto('/?mock=showcase#/system');
  const release = page.getByRole('region', { name: 'About' });
  await expect(release.getByText('Release', { exact: true })).toBeVisible();
  await expect(release.getByRole('link', { name: 'v0.3.0 on GitHub' })).toHaveAttribute(
    'href',
    'https://github.com/mcfbytes/mistarr/releases/tag/v0.3.0'
  );
});

test('Copy diagnostics gives plain text with no paths, addresses or file names', async ({ page }) => {
  await page.goto('/#/system');
  await page.getByRole('button', { name: 'Copy diagnostics' }).click();
  await expect(page.locator('.toasts').getByText('Diagnostics copied.')).toBeVisible();
  const text = await clipboard(page);
  expect(text.split('\n')[0]).toBe('mistarr 0.3.0-dev+1a2b3c4 (development build)');
  for (const line of ['client: rtorrent version unknown, reachable', 'corename: FCEUmm', 'scheduler: paused (core), 1 job waiting']) {
    expect(text).toContain(line);
  }
  expect(text).toMatch(/^memory: mistarr 39 MiB, available 204 MiB of 484 MiB$/m);
  for (const forbidden of ['/media/fat', 'scgi://', '127.0.0.1', 'dats', '.chd', '.torrent']) {
    expect(text, forbidden).not.toContain(forbidden);
  }
  expect(text).not.toMatch(/\bnes\b/);
  expect(text).not.toMatch(/[0-9a-f]{32,}/i);
});

test('the save bar appears on a change and Discard reverts it', async ({ page }) => {
  await page.goto('/#/system');
  const bar = page.getByRole('region', { name: 'Unsaved changes' });
  await expect(bar).toHaveCount(0);

  const menu = page.getByRole('group', { name: 'At the menu' });
  const down = menu.getByLabel('Download');
  await expect(down).toHaveValue('0');
  await down.fill('300');
  await expect(bar).toBeVisible();
  await expect(bar, 'the bar stays at the bottom of the window').toBeInViewport();
  await bar.getByRole('button', { name: 'Discard' }).click();
  await expect(bar).toHaveCount(0);
  await expect(down).toHaveValue('0');

  const regions = page.getByLabel('Region order');
  await regions.fill('Europe, USA');
  await expect(bar).toBeVisible();
  page.once('dialog', (d) => void d.dismiss());
  await page.getByRole('link', { name: 'Platforms' }).click();
  await expect(page).toHaveURL(/#\/system$/);
  await bar.getByRole('button', { name: 'Save' }).click();
  await expect(bar).toHaveCount(0);
  await expect(page.locator('.toasts').getByText('Settings saved.')).toBeVisible();
  await expect(regions).toHaveValue('Europe, USA');
});

test('the settings section list works from the keyboard', async ({ page }) => {
  await page.goto('/#/system');
  const nav = page.getByRole('navigation', { name: 'Settings sections' });
  const scanning = nav.getByRole('button', { name: 'Scanning' });
  await nav.getByRole('button', { name: 'Download client' }).focus();
  for (let i = 0; i < 4; i++) {
    await page.keyboard.press('Tab');
  }
  await expect(scanning).toBeFocused();
  await page.keyboard.press('Enter');
  await expect(page.getByRole('heading', { name: 'Scanning' })).toBeFocused();
  await expect(scanning).toHaveAttribute('aria-current', 'true');
  await page.keyboard.press('Tab');
  await expect(page.getByLabel('Identify CHD images by their tracks')).toBeFocused();
});

test('at phone width the tiles sit two across and nothing scrolls sideways', async ({ page }) => {
  await page.setViewportSize({ width: 360, height: 740 });
  await page.goto('/#/system');
  const tiles = page.getByRole('list', { name: 'Status' }).getByRole('listitem');
  await expect(tiles).toHaveCount(6);
  const [a, b, c] = await Promise.all([0, 1, 2].map((i) => tiles.nth(i).boundingBox()));
  expect(a?.y).toBe(b?.y);
  expect(c?.y ?? 0).toBeGreaterThan(a?.y ?? 0);
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow).toBeLessThanOrEqual(1);
  await expect(page.getByRole('navigation', { name: 'Settings sections' })).toBeVisible();
});
