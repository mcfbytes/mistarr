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
