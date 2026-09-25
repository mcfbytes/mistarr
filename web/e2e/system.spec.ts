import { test, expect, type Locator, type Page } from '@playwright/test';

test.use({ permissions: ['clipboard-read', 'clipboard-write'] });

function tile(page: Page, name: string): Locator {
  return page
    .getByRole('list', { name: 'Status' })
    .getByRole('listitem')
    .filter({ has: page.getByRole('heading', { name, exact: true }) });
}

async function clipboard(page: Page): Promise<string> {
  return page.evaluate(() => navigator.clipboard.readText());
}

test('the status tiles show the board, client, scheduler, memory, storage and uptime', async ({ page }) => {
  await page.goto('/#/system');
  const tiles = page.getByRole('list', { name: 'Status' }).getByRole('listitem');
  await expect(tiles).toHaveCount(6);
  await expect(tile(page, 'MiSTer')).toContainText('FCEUmm');
  await expect(tile(page, 'MiSTer')).toContainText('Launching: Ready');
  const client = tile(page, 'Download client');
  await expect(client).toContainText('rtorrent');
  await expect(client.locator('[data-status="done"]')).toHaveText('Reachable');
  await expect(client).toContainText('scgi://127.0.0.1:5000');
  const scheduler = tile(page, 'Scheduler');
  await expect(scheduler.locator('[data-status="paused"]')).toHaveText('Held for the core');
  await expect(scheduler).toContainText('1 job waiting');
  await expect(scheduler.getByRole('button', { name: 'Run now' })).toBeVisible();
  await expect(tile(page, 'Memory')).toContainText('41 MB used by mistarr');
  await expect(page.getByRole('meter', { name: 'Board memory in use' })).toHaveAttribute(
    'aria-valuetext',
    '214 MB available of 507 MB'
  );
  await expect(page.getByRole('meter', { name: 'Storage in use' })).toHaveAttribute('aria-valuenow', '61');
  await expect(tile(page, 'Uptime')).toContainText('1 h 15 min');
  const held = 'Download client paused while FCEUmm is running';
  await expect(tile(page, 'MiSTer').locator('[data-status="paused"]')).toHaveText(held);
  await expect(client.locator('[data-status="paused"]')).toHaveText(held);
});

test('held uploads on rtorrent show in both tiles, the 1 KiB/s line in the client tile only', async ({ page }) => {
  await page.goto('/#/');
  await page.evaluate(() => localStorage.setItem('mistarr.mockStatus', JSON.stringify({ client_hold: 'uploads' })));
  await page.goto('/#/system');
  const text = 'Uploads paused while FCEUmm is running';
  await expect(tile(page, 'MiSTer').locator('[data-status="paused"]')).toHaveText(text);
  await expect(tile(page, 'MiSTer')).not.toContainText('1 KiB/s');
  await expect(tile(page, 'Download client')).toContainText('rtorrent holds uploads at 1 KiB/s');
  await page.evaluate(() => localStorage.removeItem('mistarr.mockStatus'));
});

test('at the menu with nothing held, the board and scheduler say so', async ({ page }) => {
  await page.goto('/?mock=showcase#/system');
  await expect(tile(page, 'MiSTer')).toContainText('At the menu');
  const scheduler = tile(page, 'Scheduler');
  await expect(scheduler.locator('[data-status="running"]')).toHaveText('Running');
  await expect(scheduler).toContainText('Nothing waiting');
  await expect(page.locator('[data-testid^="client-held"]')).toHaveCount(0);
  await expect(scheduler.getByRole('button', { name: 'Pause' })).toBeVisible();
});

test('the version copies, and a release links its notes', async ({ page }) => {
  await page.goto('/#/system');
  const about = page.getByRole('region', { name: 'About' });
  await expect(about.getByText('0.3.0+dev.1a2b3c4')).toBeVisible();
  await expect(about.getByText('Development build')).toBeVisible();
  await expect(about.getByRole('link')).toHaveCount(0);
  await about.getByRole('button', { name: 'Copy version' }).click();
  expect(await clipboard(page)).toBe('0.3.0+dev.1a2b3c4');
  await expect(page.locator('.toasts').getByText('Version 0.3.0+dev.1a2b3c4 copied.')).toBeVisible();

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
  expect(text.split('\n')[0]).toBe('mistarr 0.3.0+dev.1a2b3c4 (development build)');
  for (const line of ['client: rtorrent version unknown, reachable', 'corename: FCEUmm', 'scheduler: paused (core), 1 job waiting', 'pause client while a core runs: yes', 'client held: frozen']) {
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

test('Back with unsaved settings asks, and stays when the answer is no', async ({ page }) => {
  await page.goto('/#/');
  await page.getByRole('link', { name: 'System', exact: true }).click();
  const regions = page.getByLabel('Region order');
  await regions.fill('Japan, USA');
  const bar = page.getByRole('region', { name: 'Unsaved changes' });
  await expect(bar).toBeVisible();

  page.once('dialog', (d) => void d.dismiss());
  await page.goBack();
  await expect(page).toHaveURL(/#\/system$/);
  await expect(regions).toHaveValue('Japan, USA');
  await expect(bar).toBeVisible();

  page.once('dialog', (d) => void d.accept());
  await page.goBack();
  await expect(page).toHaveURL(/#\/$/);
  await expect(bar).toHaveCount(0);
});

test('a modifier or middle click on a link opens elsewhere without asking', async ({ page, context }) => {
  await page.goto('/#/system');
  await page.getByLabel('Region order').fill('Japan');
  let asked = false;
  page.on('dialog', (d) => {
    asked = true;
    void d.dismiss();
  });
  const link = page.getByRole('link', { name: 'Platforms' });
  const opened = context.waitForEvent('page');
  await link.click({ modifiers: ['ControlOrMeta'] });
  await (await opened).close();
  await link.click({ button: 'middle' });
  await page.waitForTimeout(300);
  expect(asked).toBe(false);
  await expect(page).toHaveURL(/#\/system$/);
  await expect(page.getByRole('region', { name: 'Unsaved changes' })).toBeVisible();
});

test('a cleared speed limit is marked and blocks Save until fixed or discarded', async ({ page }) => {
  await page.goto('/#/system');
  const down = page.getByRole('group', { name: 'While a core runs' }).getByLabel('Download');
  await down.fill('');
  await expect(down).toHaveAttribute('aria-invalid', 'true');
  const bar = page.getByRole('region', { name: 'Unsaved changes' });
  await bar.getByRole('button', { name: 'Save' }).click();
  await expect(bar.getByRole('alert')).toHaveText('Each speed limit needs a whole number of kB/s, 0 or more.');
  await expect(page.locator('.toasts').getByText('Settings saved.')).toHaveCount(0);
  await down.fill('256');
  await expect(down).not.toHaveAttribute('aria-invalid', 'true');
  await bar.getByRole('button', { name: 'Discard' }).click();
  await expect(bar).toHaveCount(0);
  await expect(down).toHaveValue('512');
});

test('toasts sit above the save bar at phone width', async ({ page }) => {
  await page.setViewportSize({ width: 360, height: 740 });
  await page.goto('/#/system');
  await page.getByLabel('Region order').fill('Japan');
  const bar = page.getByRole('region', { name: 'Unsaved changes' });
  await expect(bar).toBeInViewport();
  await page.getByRole('button', { name: 'Copy version' }).click();
  const toast = page.locator('.toasts .toast').first();
  await expect(toast).toBeVisible();
  const [t, b] = await Promise.all([toast.boundingBox(), bar.boundingBox()]);
  expect((t?.y ?? 0) + (t?.height ?? 0)).toBeLessThanOrEqual((b?.y ?? 0) + 1);
});

test('without the Clipboard API, diagnostics copy through a text area', async ({ page }) => {
  await page.addInitScript(() => {
    Object.defineProperty(Navigator.prototype, 'clipboard', { get: () => undefined, configurable: true });
    Object.defineProperty(Document.prototype, 'execCommand', {
      configurable: true,
      value(this: Document): boolean {
        const el = this.activeElement;
        (window as unknown as { copied: string }).copied = el instanceof HTMLTextAreaElement ? el.value : '';
        return true;
      }
    });
  });
  await page.goto('/#/system');
  const button = page.getByRole('button', { name: 'Copy diagnostics' });
  await button.click();
  await expect(page.locator('.toasts').getByText('Diagnostics copied.')).toBeVisible();
  const copied = await page.evaluate(() => (window as unknown as { copied: string }).copied);
  expect(copied.split('\n')[0]).toBe('mistarr 0.3.0+dev.1a2b3c4 (development build)');
  await expect(button).toBeFocused();
});

test('when the browser refuses to copy, the diagnostics are shown to copy by hand', async ({ page }) => {
  await page.addInitScript(() => {
    Object.defineProperty(Navigator.prototype, 'clipboard', { get: () => undefined, configurable: true });
    Object.defineProperty(Document.prototype, 'execCommand', { configurable: true, value: (): boolean => false });
  });
  await page.goto('/#/system');
  await page.getByRole('button', { name: 'Copy diagnostics' }).click();
  const box = page.getByLabel('The browser did not allow copying. Select this text and copy it.');
  await expect(box).toBeVisible();
  await expect(box).toHaveValue(/^mistarr 0\.3\.0\+dev\.1a2b3c4 \(development build\)\n/);
  await expect(box).not.toHaveValue(/\/media\/fat|scgi:\/\//);
});
