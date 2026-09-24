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
