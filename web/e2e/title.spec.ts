import { expect, test } from '@playwright/test';
import { setMockKnob } from './helpers';

test('a missing title shows a not-found state, never stuck on Loading', async ({ page }) => {
  await setMockKnob(page, 'removedIds', [1]);
  await page.goto('/#/t/1');
  await expect(page.getByRole('heading', { name: 'Title not found' })).toBeVisible();
  await expect(page.getByText('Loading…')).toHaveCount(0);
});
