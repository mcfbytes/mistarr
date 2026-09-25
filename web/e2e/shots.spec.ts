import { test, type Page } from '@playwright/test';

// Review screenshots of the activity indicator and status pills, written only when SHOTS_DIR is set.
const dir = process.env.SHOTS_DIR;

const sizes = [
  { name: 'desktop', width: 1280, height: 900 },
  { name: 'phone', width: 390, height: 844 }
];
const schemes = ['dark', 'light'] as const;

async function openPanel(page: Page): Promise<void> {
  await page.getByRole('button', { name: /^Background work:/ }).click();
  await page.getByRole('region', { name: 'Background work' }).waitFor();
}

const shots: { name: string; url: string; act?: (page: Page) => Promise<void>; full?: boolean }[] = [
  { name: 'nav-idle', url: '/?mock=idle#/' },
  { name: 'nav-busy', url: '/#/' },
  { name: 'panel-open', url: '/#/sources', act: openPanel },
  {
    name: 'upload-toast',
    url: '/#/sources',
    act: async (page) => {
      await page.getByLabel('Add a .torrent file').setInputFiles({
        name: 'Example bundle four.torrent',
        mimeType: 'application/x-bittorrent',
        buffer: Buffer.from('d1:ae')
      });
      await page.locator('.toasts').getByText(/^Torrent received:/).waitFor();
    }
  },
  { name: 'sources-pills', url: '/#/sources', full: true },
  { name: 'dat-progress', url: '/#/dats', act: async (page) => page.waitForTimeout(1500) },
  { name: 'activity-page', url: '/#/activity', full: true }
];

for (const size of sizes) {
  for (const scheme of schemes) {
    for (const shot of shots) {
      test(`${shot.name} ${scheme} ${size.name}`, async ({ page }) => {
        test.skip(!dir, 'SHOTS_DIR is not set');
        await page.emulateMedia({ colorScheme: scheme });
        await page.setViewportSize({ width: size.width, height: size.height });
        await page.goto(shot.url);
        await page.waitForTimeout(300);
        await shot.act?.(page);
        await page.screenshot({
          path: `${dir ?? ''}/${shot.name}-${scheme}-${size.width}x${size.height}.png`,
          fullPage: shot.full ?? false
        });
      });
    }
  }
}
