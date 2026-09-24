import { test, expect } from '@playwright/test';

const routes = [
  { name: 'wizard', hash: '#/wizard' },
  { name: 'platforms', hash: '#/' },
  { name: 'browse', hash: '#/p/nes' },
  { name: 'title', hash: '#/t/1' },
  { name: 'activity', hash: '#/activity' },
  { name: 'sources', hash: '#/sources' },
  { name: 'dats', hash: '#/dats' },
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
      const navClipped = await page.evaluate(() => {
        const nav = document.querySelector('nav');
        return nav ? nav.scrollWidth - nav.clientWidth : 0;
      });
      expect(navClipped, 'every nav link fits without scrolling').toBeLessThanOrEqual(1);

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

test('Play is hidden for a BIOS entry even when its file is present', async ({ page }) => {
  await page.goto('/#/t/1');
  const biosRow = page.getByRole('row', { name: /\(BIOS\)/ });
  await expect(biosRow).toHaveCount(1);
  await expect(biosRow.getByText('verified')).toBeVisible();
  await expect(biosRow.getByRole('button', { name: 'Play' })).toHaveCount(0);
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

test('platform cards show have, wanted and titles, plus any nonzero extra', async ({ page }) => {
  await page.goto('/#/');
  await expect(page.getByText('180 have · 12 wanted · 240 titles · 4 unmatched files')).toBeVisible();
  await expect(
    page.getByText('978 have · 0 wanted · 1298 titles · 14 failing check · 6 partial')
  ).toBeVisible();
});

test('held jobs show a banner with Run now', async ({ page }) => {
  await page.goto('/#/');
  const banner = page.getByRole('status').filter({ hasText: 'paused while FCEUmm is running' });
  await expect(banner).toBeVisible();
  await expect(banner.getByRole('button', { name: 'Run now' })).toBeVisible();
});

test('the wizard lists files waiting in dats and sources', async ({ page }) => {
  await page.goto('/#/wizard');
  await page.getByRole('button', { name: 'Next' }).click();
  const dats = page.getByRole('list', { name: 'Files in dats' });
  await expect(dats.getByText('Importing')).toBeVisible();
  await expect(dats.getByText(/^not a DAT/)).toBeVisible();

  await page.getByRole('button', { name: 'Next' }).click();
  await page.getByRole('button', { name: 'Next' }).click();
  const sources = page.getByRole('list', { name: 'Files in sources' });
  await expect(sources.getByText(/Waiting: Waiting for the file to stop changing/)).toBeVisible();
});

test('a path mapping can be removed and a half-filled one is refused', async ({ page }) => {
  await page.goto('/#/system');
  await page.getByRole('button', { name: 'Add mapping' }).click();
  await expect(page.getByLabel('Remote path')).toHaveCount(1);
  await page.getByRole('button', { name: 'Remove' }).click();
  await expect(page.getByLabel('Remote path')).toHaveCount(0);

  await page.getByRole('button', { name: 'Add mapping' }).click();
  await page.getByLabel('Remote path').fill('/downloads');
  await page.getByRole('button', { name: 'Save' }).click();
  await expect(page.getByText(/needs a remote path and an absolute local path/)).toBeVisible();
});

test('an unbound source offers its suggested platform', async ({ page }) => {
  await page.goto('/#/sources');
  await expect(page.getByRole('button', { name: /^Bind to / })).toBeVisible();
});

test('the DATs screen lists loaded versions and incoming files', async ({ page }) => {
  await page.goto('/#/');
  await page.getByRole('link', { name: 'DATs' }).click();
  await expect(page).toHaveURL(/#\/dats$/);

  const loaded = page.getByRole('list', { name: 'Loaded DATs' });
  const family = (name: RegExp) =>
    loaded.getByRole('listitem').filter({ has: page.getByRole('heading', { name }) });
  const exportItem = family(/DB Export/);
  await expect(exportItem).toContainText('Sega Mega Drive');
  await expect(exportItem).toContainText('20260101-000000');
  await expect(exportItem).toContainText('310');
  await expect(exportItem).toContainText('Current');
  await exportItem.getByText('Older versions (1)').click();
  await expect(exportItem.getByRole('list', { name: /^Older versions of/ })).toContainText(
    'Replaced by Example Vendor - Mega Drive - Genesis (DB Export) version 20260101-000000'
  );
  await expect(family(/Unbound Sample DAT/)).toContainText('Not bound');
  await expect(family(/^mistarr samples/)).toContainText('Current');
  await expect(family(/^Example Console DAT/)).toContainText('Current');
  await expect(page.getByRole('heading', { name: /Loaded/ })).toContainText('5 versions');
  await expect(page.getByText('/media/fat/mistarr/dats')).toBeVisible();

  const upload = page.getByLabel('Add DAT files');
  await expect(upload).toHaveAttribute('accept', '.dat,.xml,.zip');
  await expect(upload).toHaveAttribute('multiple', '');

  const incoming = page.getByRole('list', { name: 'Files in dats' });
  await expect(incoming.getByText('Importing')).toBeVisible();
  await expect(incoming.getByText(/1 of 1 files|0 of 1 files/)).toBeVisible();
  await expect(incoming.getByText(/expected a Logiqx DAT .* or a No-Intro DB export/)).toBeVisible();
});

test('a rejected DAT can be retried or deleted', async ({ page }) => {
  await page.goto('/#/dats');
  const incoming = page.getByRole('list', { name: 'Files in dats' });
  const rejected = incoming.getByRole('listitem').filter({ hasText: 'Example Handheld (20260101).xml' });
  await rejected.getByRole('button', { name: 'Retry Example Handheld (20260101).xml' }).click();
  await expect(rejected).toContainText('Waiting: Queued.');
  await expect(rejected.getByRole('button', { name: /^Retry/ })).toHaveCount(0);
  await expect(page.getByText('Example Handheld (20260101).xml queued to load again.')).toBeVisible();

  const notes = incoming.getByRole('listitem').filter({ hasText: 'notes.txt' });
  const del = notes.getByRole('button', { name: 'Delete notes.txt', exact: true });
  await del.click();
  await expect(notes.getByRole('button', { name: 'Delete the file notes.txt' })).toBeFocused();
  await notes.getByRole('button', { name: 'Keep notes.txt' }).click();
  await expect(del).toBeFocused();
  await del.click();
  await notes.getByRole('button', { name: 'Delete the file notes.txt' }).click();
  await expect(incoming.getByText('notes.txt')).toHaveCount(0);
  await expect(page.getByText('notes.txt deleted.')).toBeVisible();
});

test('a loaded DAT is removed only after a confirmation that files stay', async ({ page }) => {
  await page.goto('/#/dats');
  const label = 'Unbound Sample DAT version 20260102';
  const remove = page.getByRole('button', { name: `Remove ${label}`, exact: true });
  await remove.click();
  const confirm = page.getByRole('button', { name: `Remove ${label} from the catalogue` });
  await expect(confirm).toBeFocused();
  await expect(page.getByText(/Files on the card stay where they are/)).toBeVisible();
  await page.getByRole('button', { name: `Keep ${label}` }).click();
  await expect(remove).toBeFocused();
  await remove.click();
  await confirm.click();
  await expect(page.getByText(`${label} removed. Files on the card stay where they are.`)).toBeVisible();
  const item = page
    .getByRole('list', { name: 'Loaded DATs' })
    .getByRole('listitem')
    .filter({ has: page.getByRole('heading', { name: 'Unbound Sample DAT' }) });
  await expect(item).toContainText('Removed; its games are no longer listed');
  await expect(item.getByRole('button', { name: /^Remove/ })).toHaveCount(0);
  await expect(page.getByRole('heading', { name: 'Unbound Sample DAT' })).toBeFocused();
});

test('the title lists each file that may hold a variant with its confidence', async ({ page }) => {
  await page.goto('/#/t/1');
  const pick = page.getByRole('row', { name: /\(USA\) \(pick\)/ });
  await expect(pick.getByText('Sample Racer (USA).nes in Example Pack (name match)')).toBeVisible();
  await expect(pick.getByText('example.nes in examplepack1.0 (name guess)')).toBeVisible();
  const europe = page.getByRole('row', { name: /\(Europe\)/ });
  await expect(europe.getByText('example.nes in examplepack1.0 (name guess)')).toBeVisible();
  const bios = page.getByRole('row', { name: /\(BIOS\)/ });
  await expect(bios.getByText('None available')).toBeVisible();
});
