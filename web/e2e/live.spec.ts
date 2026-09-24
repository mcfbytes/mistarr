import { test, expect, type Page } from '@playwright/test';

// A synthetic Logiqx DAT whose header matches the NES platform's binding
// pattern, so the upload auto-binds and loads without a manual step.
const DAT_XML = `<?xml version="1.0"?>
<datafile>
  <header><name>Nintendo Entertainment System</name><version>20260101</version></header>
  <game name="Example Quest (USA)">
    <rom name="Example Quest (USA).nes" size="4" crc="0a0b0c0d"/>
  </game>
</datafile>
`;

const viewports = [
  { name: 'phone', width: 360, height: 740 },
  { name: 'desktop', width: 1280, height: 800 }
];

let titleId = '';

async function goto(page: Page, hash: string): Promise<void> {
  await page.goto(`/${hash}`);
  await page.waitForTimeout(150);
}

test.describe.configure({ mode: 'serial' });

test('wizard completes and uploads a synthetic DAT', async ({ page }) => {
  await page.goto('/');
  await expect(page).toHaveURL(/#\/wizard$/);

  // Step 1: paths, with the detected-cores re-detect button.
  await expect(page.getByRole('heading', { name: 'Paths' })).toBeVisible();
  await expect(page.getByText('None detected yet.')).toBeVisible({ timeout: 10_000 });
  await page.getByRole('button', { name: 'Re-detect' }).click();
  await expect(page.getByRole('button', { name: 'Re-detect' })).toBeVisible({ timeout: 10_000 });
  await expect(page.getByText('None detected yet.')).toBeVisible();
  await page.getByRole('button', { name: 'Next' }).click();

  // Step 2: DATs.
  await expect(page.getByRole('heading', { name: 'DATs' })).toBeVisible();
  await page
    .locator('input[type="file"]')
    .first()
    .setInputFiles({ name: 'example-console.dat', mimeType: 'application/xml', buffer: Buffer.from(DAT_XML) });
  await expect(page.getByText('Example Console DAT').or(page.getByText('Nintendo Entertainment System'))).toBeVisible(
    { timeout: 10_000 }
  );
  await page.getByRole('button', { name: 'Next' }).click();

  // Step 3: client.
  await expect(page.getByRole('heading', { name: 'Client' })).toBeVisible();
  await page.getByRole('button', { name: 'Next' }).click();

  // Step 4: sources.
  await expect(page.getByRole('heading', { name: 'Sources' })).toBeVisible();
  await page.getByRole('button', { name: 'Finish' }).click();

  await expect(page).toHaveURL(/#\/$/);
});

test('browse to the title and want it', async ({ page }) => {
  await goto(page, '#/p/nes');
  const link = page.getByRole('link', { name: /Example Quest/ });
  await expect(link).toBeVisible({ timeout: 10_000 });
  const href = await link.getAttribute('href');
  titleId = href?.split('/t/')[1] ?? '';
  expect(titleId).not.toBe('');

  await goto(page, `#/t/${titleId}`);
  await expect(page.getByRole('heading', { name: 'Example Quest' })).toBeVisible();
  await page.getByRole('button', { name: 'Want', exact: true }).click();
  await expect(page.getByRole('button', { name: 'Unwant' })).toBeVisible({ timeout: 10_000 });
});

for (const viewport of viewports) {
  test(`screens render at ${viewport.name} width`, async ({ page }) => {
    await page.setViewportSize({ width: viewport.width, height: viewport.height });

    const routes = [
      { name: 'wizard', hash: '#/wizard' },
      { name: 'platforms', hash: '#/' },
      { name: 'browse', hash: '#/p/nes' },
      { name: 'title', hash: `#/t/${titleId}` },
      { name: 'activity', hash: '#/activity' },
      { name: 'sources', hash: '#/sources' },
      { name: 'system', hash: '#/system' }
    ];

    for (const route of routes) {
      await goto(page, route.hash);
      const overflow = await page.evaluate(() => {
        const doc = document.documentElement;
        return doc.scrollWidth - doc.clientWidth;
      });
      expect(overflow).toBeLessThanOrEqual(1);
      await page.screenshot({ path: `e2e/out/live-${route.name}-${viewport.name}.png`, fullPage: true });
    }
  });
}
