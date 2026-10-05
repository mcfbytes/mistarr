import { expect, test } from '@playwright/test';
import { setMockKnob } from './helpers';

test('a mock with 150 sources lists all 150 over several small pages', async ({ page }) => {
  await setMockKnob(page, 'sourceCount', 148);
  // Forces the store to loop several times instead of fitting in one page.
  await setMockKnob(page, 'pageCap', 40);
  await page.goto('/#/sources');
  await expect(page.locator('table tbody tr')).toHaveCount(150);
});

test('deleting a source asks for confirmation inline before it runs', async ({ page }) => {
  await page.goto('/#/sources');
  const row = page.locator('table tbody tr').first();
  const trigger = row.locator('[data-confirm="ask"]');
  await trigger.click();
  const confirm = row.locator('[data-confirm="yes"]');

  await expect(row.getByRole('group')).toContainText('placed files stay');
  await expect(trigger).toHaveCount(0);
  await expect(confirm).toHaveText('Remove source');
  await expect(confirm).toBeFocused();

  await row.getByRole('button', { name: 'Keep' }).click();
  await expect(confirm).toHaveCount(0);
  await expect(trigger).toBeVisible();
  await expect(trigger).toBeFocused();
});

test('with no sources the page shows its empty text and no table', async ({ page }) => {
  await setMockKnob(page, 'noSources', true);
  await page.goto('/#/sources');
  await expect(page.getByText('No sources yet.')).toBeVisible();
  await expect(page.locator('table')).toHaveCount(0);
});

test('with no DATs the page shows its empty text and no list', async ({ page }) => {
  await setMockKnob(page, 'noDats', true);
  await page.goto('/#/dats');
  await expect(page.getByText('No DAT loaded yet.')).toBeVisible();
  await expect(page.getByRole('list', { name: 'Loaded DATs' })).toHaveCount(0);
});
