import { expect, test } from '@playwright/test';

test('a mock with 150 sources lists all 150 over several small pages', async ({ page }) => {
  await page.addInitScript(() => {
    localStorage.setItem('mistarr.mockSourceCount', '148');
    // Forces the store to loop several times instead of fitting in one page.
    localStorage.setItem('mistarr.mockPageCap', '40');
  });
  await page.goto('/#/sources');
  await expect(page.locator('table tbody tr')).toHaveCount(150);
});

test('deleting a source asks for confirmation inline before it runs', async ({ page }) => {
  await page.goto('/#/sources');
  const row = page.locator('table tbody tr').first();
  const trigger = row.locator('[data-action="remove"]');
  const id = await trigger.getAttribute('data-source');
  await trigger.click();
  const confirm = row.locator(`[data-source="${id}"][data-action="confirm"]`);

  await expect(row.getByRole('group')).toContainText('placed files stay');
  await expect(trigger).toHaveCount(0);
  await expect(confirm).toHaveText('Remove source');
  await expect(confirm).toBeFocused();

  await row.getByRole('button', { name: 'Keep' }).click();
  await expect(confirm).toHaveCount(0);
  await expect(trigger).toBeVisible();
  await expect(trigger).toBeFocused();
});
