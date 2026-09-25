import { expect, test, type Page } from '@playwright/test';

/** Opens the detail of the fixture source "Example bundle one" from the Sources list, by keyboard. */
async function openFromList(page: Page): Promise<void> {
  await page.goto('/?mock=idle#/sources');
  const link = page.getByRole('link', { name: 'Example bundle one' });
  await link.focus();
  await page.keyboard.press('Enter');
  await expect(page).toHaveURL(/#\/sources\/1$/);
  await expect(page.getByRole('heading', { level: 1, name: 'Example bundle one' })).toBeVisible();
}

test('a source opens from the list with the keyboard and shows what it holds', async ({ page }) => {
  await openFromList(page);
  const overview = page.getByRole('region', { name: 'Overview' });
  await expect(overview).toContainText('240');
  await expect(overview).toContainText('00000000…00000000');
  await expect(overview).toContainText('In the client, 2 files selected');
  await expect(page.getByRole('progressbar', { name: 'Transfer of the selected files' })).toBeVisible();
  await expect(page.getByRole('combobox', { name: 'Seed policy', exact: true })).toHaveValue('ratio:1');
  await expect(page.getByTestId('client-held-line')).toHaveText('Client paused while FCEUmm is running');
  await expect(page.getByText('Paused while a core runs')).toBeVisible();
  await expect(page.getByText('Bound to Nintendo Entertainment System automatically. 94% of its files match DAT entries.')).toBeVisible();
  const counts = page.getByRole('list', { name: 'Files by match' });
  await expect(counts).toContainText('233 matched');
  await expect(counts).toContainText('5 possible matches');
  await expect(counts).toContainText('1 unmatched');
  await expect(counts).toContainText('1 extra (text');
  await expect(counts).toContainText('3 wanted');

  await page.goBack();
  await expect(page).toHaveURL(/#\/sources$/);
});

test('the file table pages, filters and searches', async ({ page }) => {
  await openFromList(page);
  const status = page.getByText(/^Files \d/);
  const rows = page.locator('section[aria-labelledby="files-h"] tbody tr');
  await expect(status).toHaveText('Files 1–50 of 240');
  await expect(rows).toHaveCount(50);
  await expect(rows.first()).toContainText('Example Quest 1 (USA).nes');
  await expect(rows.first().getByRole('link', { name: 'Example Quest 1 (USA).nes' })).toHaveAttribute('href', /#\/t\/\d+$/);

  await page.getByRole('button', { name: 'Next' }).click();
  await expect(status).toHaveText('Files 51–100 of 240');
  await expect(page.getByRole('button', { name: 'Previous' })).toBeEnabled();

  const unmatched = page.getByRole('button', { name: 'Unmatched' });
  await unmatched.click();
  await expect(unmatched).toHaveAttribute('aria-pressed', 'true');
  await expect(status).toHaveText('Files 1–2 of 2');
  await expect(rows.nth(1)).toContainText('readme.txt');
  await expect(rows.nth(1)).toContainText('Not a game file.');
  await expect(page.getByRole('button', { name: 'Next' })).toBeDisabled();

  await page.getByRole('button', { name: 'Wanted' }).click();
  await expect(status).toHaveText('Files 1–3 of 3');
  await expect(rows.first()).toContainText('Transferring · 45%');

  await page.getByRole('button', { name: 'All' }).click();
  await page.getByRole('searchbox', { name: 'Search paths' }).fill('README');
  await expect(status).toHaveText('Files 1–1 of 1');
  await expect(rows).toHaveCount(1);
});

test('re-classify previews, runs as a job and marks the source as set by you', async ({ page }) => {
  await openFromList(page);
  const open = page.getByRole('button', { name: 'Re-classify…' });
  await open.click();
  await expect(open).toHaveAttribute('aria-expanded', 'true');
  const panel = page.getByRole('region', { name: 'Re-classify' });
  await expect(panel.getByText('would match 238 of 240 files')).toBeVisible();
  await panel.getByRole('radio', { name: /Sega Mega Drive/ }).check();
  await expect(panel.getByText(/^Binding to Sega Mega Drive would match \d+ of 240 files\./)).toBeVisible();
  await panel.getByRole('button', { name: 'Bind to Sega Mega Drive' }).click();
  await expect(panel).toHaveCount(0);
  await expect(open).toBeFocused();
  await expect(page.getByText('Binding to Sega Mega Drive…', { exact: true })).toBeVisible();

  // The job shows on the page and in the activity panel while it runs.
  await expect(page.getByRole('progressbar', { name: 'Binding progress' })).toHaveAttribute(
    'aria-valuetext',
    'Matching files to DAT entries'
  );
  const activity = page.getByRole('button', { name: /^Background work: 1 running/ });
  await activity.click();
  await expect(page.getByRole('list', { name: 'Running' })).toContainText('Source binding: Example bundle one');
  await page.keyboard.press('Escape');

  await expect(page.getByText(/^Binding of Example bundle one: \d+ of 240 files matched$/).first()).toBeVisible({
    timeout: 5000
  });
  await expect(page.getByTestId('overridden')).toHaveText('Set by you');
  await expect(page.getByText(/^Bound to Sega Mega Drive by you\./)).toBeVisible();

  await page.goBack();
  await expect(page).toHaveURL(/#\/sources$/);
  const row = page.getByRole('row').filter({ hasText: 'Example bundle one' });
  await expect(row).toContainText('megadrive');
  await expect(row.getByText('Set by you')).toBeVisible();
});

test('marking a source as not a game set, then Reset to automatic', async ({ page }) => {
  await openFromList(page);
  await page.getByRole('button', { name: 'Re-classify…' }).click();
  const panel = page.getByRole('region', { name: 'Re-classify' });
  await panel.getByRole('radio', { name: /Not a game set/ }).check();
  await panel.getByRole('button', { name: 'Set aside' }).click();
  await expect(page.getByText('Marked by you as not a game set. It is not bound automatically.')).toBeVisible({
    timeout: 5000
  });
  const rows = page.locator('section[aria-labelledby="files-h"] tbody tr');
  await expect(rows.first()).toContainText('Not matched: the source is not bound to a platform.');

  await page.getByRole('button', { name: 'Reset to automatic' }).click();
  await expect(page.getByTestId('overridden')).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Reset to automatic' })).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Re-classify…' })).toBeFocused();
  await expect(page.getByText('Returning to automatic binding…')).toBeVisible();
  await expect(page.getByText(/^Bound to Nintendo Entertainment System automatically\./)).toBeVisible({
    timeout: 5000
  });
});

test('Cancel closes Re-classify and puts focus back on its button', async ({ page }) => {
  await openFromList(page);
  const open = page.getByRole('button', { name: 'Re-classify…' });
  await open.click();
  const panel = page.getByRole('region', { name: 'Re-classify' });
  await panel.getByRole('radio', { name: /Sega Saturn/ }).check();
  await panel.getByRole('button', { name: 'Cancel' }).click();
  await expect(panel).toHaveCount(0);
  await expect(open).toBeFocused();
  await expect(open).toHaveAttribute('aria-expanded', 'false');
});

test('going from one source straight to another starts the page afresh', async ({ page }) => {
  await openFromList(page);
  await page.getByRole('button', { name: 'Unmatched' }).click();
  await expect(page.getByText(/^Files 1–2 of 2$/)).toBeVisible();
  await page.getByRole('button', { name: 'Re-classify…' }).click();
  await page.getByRole('region', { name: 'Re-classify' }).getByRole('radio', { name: /Sega Saturn/ }).check();

  await page.evaluate(() => {
    window.location.hash = '#/sources/2';
  });
  await expect(page.getByRole('heading', { level: 1, name: 'Example bundle two' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'All' })).toHaveAttribute('aria-pressed', 'true');
  await expect(page.getByRole('region', { name: 'Re-classify' })).toHaveCount(0);
  await expect(page.getByText(/^Files 1–50 of 60$/)).toBeVisible();
  await page.getByRole('button', { name: 'Re-classify…' }).click();
  await expect(page.getByRole('radio', { checked: true })).toHaveCount(0);
});
