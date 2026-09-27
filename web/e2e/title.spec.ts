import { expect, test } from '@playwright/test';

test('a missing title shows a not-found state, never stuck on Loading', async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('mistarr.mockRemovedIds', JSON.stringify([1])));
  await page.goto('/#/t/1');
  await expect(page.getByRole('heading', { name: 'Title not found' })).toBeVisible();
  await expect(page.getByText('Loading…')).toHaveCount(0);
});
