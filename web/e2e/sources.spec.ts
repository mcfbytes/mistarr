import { expect, test } from '@playwright/test';

test('a mock with 150 sources lists all 150, past the API default page limit', async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('mistarr.mockSourceCount', '148'));
  await page.goto('/#/sources');
  await expect(page.locator('table tbody tr')).toHaveCount(150);
});

test('deleting a source asks for confirmation inline before it runs', async ({ page }) => {
  await page.goto('/#/sources');
  const row = page.locator('table tbody tr').first();
  await row.getByRole('button', { name: 'Delete' }).click();
  await expect(row.getByRole('group')).toContainText('placed files stay');
  await expect(row.getByRole('button', { name: 'Delete' })).toHaveCount(0);
  await expect(row.getByRole('button', { name: 'Remove' })).toBeVisible();

  await row.getByRole('button', { name: 'Keep' }).click();
  await expect(row.getByRole('button', { name: 'Delete' })).toBeVisible();
  await expect(row.getByRole('button', { name: 'Remove' })).toHaveCount(0);
});
