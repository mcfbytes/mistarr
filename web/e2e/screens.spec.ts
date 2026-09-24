import { test, expect } from '@playwright/test';

const routes = [
  { name: 'wizard', hash: '#/wizard' },
  { name: 'platforms', hash: '#/' },
  { name: 'browse', hash: '#/p/nes' },
  { name: 'title', hash: '#/t/1' },
  { name: 'activity', hash: '#/activity' },
  { name: 'sources', hash: '#/sources' },
  { name: 'system', hash: '#/system' }
];

const viewports = [
  { name: 'phone', width: 360, height: 740 },
  { name: 'desktop', width: 1280, height: 800 }
];

for (const viewport of viewports) {
  for (const route of routes) {
    test(`${route.name} at ${viewport.name}`, async ({ page }) => {
      await page.setViewportSize({ width: viewport.width, height: viewport.height });
      await page.goto(`/${route.hash}`);
      await page.waitForTimeout(200);

      const overflow = await page.evaluate(() => {
        const doc = document.documentElement;
        return doc.scrollWidth - doc.clientWidth;
      });
      expect(overflow).toBeLessThanOrEqual(1);

      await page.screenshot({
        path: `e2e/out/${route.name}-${viewport.name}.png`,
        fullPage: true
      });
    });
  }
}

test('requiring a hidden-by-default flag auto-shows hidden entries', async ({ page }) => {
  await page.goto('/#/p/nes');
  await page.waitForTimeout(200);

  const showHidden = page.getByLabel('Show hidden');
  await expect(showHidden).not.toBeChecked();

  await page.getByLabel('bios', { exact: true }).check();

  await expect(showHidden).toBeChecked();
  await expect(showHidden).toBeDisabled();
  await expect(page.getByText(/hidden by default/i)).toBeVisible();

  await page.getByLabel('bios', { exact: true }).uncheck();
  await expect(showHidden).not.toBeChecked();
  await expect(showHidden).toBeEnabled();
});

test('Play starts an entry that is in the collection', async ({ page }) => {
  await page.goto('/#/t/1');
  const play = page.getByRole('button', { name: 'Play' });
  await expect(play).toHaveCount(1);
  await expect(play).toBeEnabled();
  await play.click();
  await expect(page.getByText('Started on the MiSTer.')).toBeVisible();
});

test('Play is hidden for a BIOS entry even when its file is present', async ({ page }) => {
  await page.goto('/#/t/1');
  const biosRow = page.getByRole('row', { name: /\(BIOS\)/ });
  await expect(biosRow).toHaveCount(1);
  await expect(biosRow.getByText('verified')).toBeVisible();
  await expect(biosRow.getByRole('button', { name: 'Play' })).toHaveCount(0);
});

test('Start core is offered with its reason when it cannot run', async ({ page }) => {
  await page.goto('/#/p/nes');
  const start = page.getByRole('button', { name: 'Start core' });
  await expect(start).toBeEnabled();
  await start.click();
  await expect(page.getByText('Core started on the MiSTer.')).toBeVisible();

  await page.goto('/#/p/psx');
  await expect(page.getByRole('button', { name: 'Start core' })).toBeDisabled();
  await expect(page.getByText('No core for this platform is installed.')).toBeVisible();
});

test('the launch setting is editable', async ({ page }) => {
  await page.goto('/#/system');
  const allow = page.getByLabel('Allow starting cores and games from mistarr');
  await expect(allow).toBeChecked();
  await allow.uncheck();
  await expect(allow).not.toBeChecked();
});

test('held jobs show a banner with Run now', async ({ page }) => {
  await page.goto('/#/');
  const banner = page.getByRole('status').filter({ hasText: 'paused while FCEUmm is running' });
  await expect(banner).toBeVisible();
  await expect(banner.getByRole('button', { name: 'Run now' })).toBeVisible();
});

test('the wizard lists files waiting in dats and sources', async ({ page }) => {
  await page.goto('/#/wizard');
  await page.getByRole('button', { name: 'Next' }).click();
  const dats = page.getByRole('list', { name: 'Files in dats' });
  await expect(dats.getByText('Importing')).toBeVisible();
  await expect(dats.getByText(/Rejected: not a DAT/)).toBeVisible();

  await page.getByRole('button', { name: 'Next' }).click();
  await page.getByRole('button', { name: 'Next' }).click();
  const sources = page.getByRole('list', { name: 'Files in sources' });
  await expect(sources.getByText(/Waiting: Waiting for the file to stop changing/)).toBeVisible();
});

test('a path mapping can be removed and a half-filled one is refused', async ({ page }) => {
  await page.goto('/#/system');
  await page.getByRole('button', { name: 'Add mapping' }).click();
  await expect(page.getByLabel('Remote path')).toHaveCount(1);
  await page.getByRole('button', { name: 'Remove' }).click();
  await expect(page.getByLabel('Remote path')).toHaveCount(0);

  await page.getByRole('button', { name: 'Add mapping' }).click();
  await page.getByLabel('Remote path').fill('/downloads');
  await page.getByRole('button', { name: 'Save' }).click();
  await expect(page.getByText(/needs a remote path and an absolute local path/)).toBeVisible();
});

test('an unbound source offers its suggested platform', async ({ page }) => {
  await page.goto('/#/sources');
  await expect(page.getByRole('button', { name: /^Bind to / })).toBeVisible();
});
