import { test, expect, type Page } from '@playwright/test';
import { emitEvent, setMockKnob } from './helpers';

const dat = (name: string) => ({ name, mimeType: 'application/xml', buffer: Buffer.from('<datafile/>') });

async function loaded(page: Page, file: string): Promise<void> {
  await emitEvent(page, { name: 'dat.loaded', data: { dat_version_id: 1, file, platform_id: null } });
}

function toasts(page: Page, text: string) {
  return page.locator('.toasts').getByText(text, { exact: true });
}

test('a DAT that finishes before the upload answer still ends in its loaded toast', async ({ page }) => {
  await page.goto('/#/dats');
  await page.getByLabel('Add DAT files').setInputFiles(dat('Early sample.dat'));
  await loaded(page, 'Early sample.dat');
  await expect(toasts(page, 'Early sample.dat loaded.')).toBeVisible();
  await expect(page.locator('.toasts').getByText(/^DAT received: Early sample\.dat/)).toBeVisible();
  await expect(toasts(page, 'Early sample.dat loaded.')).toHaveCount(1);
});

test('a DAT that finishes after the answer is toasted once', async ({ page }) => {
  await page.goto('/#/dats');
  await page.getByLabel('Add DAT files').setInputFiles(dat('Late sample.dat'));
  await expect(page.locator('.toasts').getByText(/^DAT received: Late sample\.dat/)).toBeVisible();
  await loaded(page, 'Late sample.dat');
  await loaded(page, 'Late sample.dat');
  await expect(toasts(page, 'Late sample.dat loaded.')).toHaveCount(1);
});

test('an ending heard before the request began does not end a re-upload', async ({ page }) => {
  await page.goto('/#/dats');
  await loaded(page, 'Again sample.dat');
  await page.waitForTimeout(50);
  await page.getByLabel('Add DAT files').setInputFiles(dat('Again sample.dat'));
  await expect(page.locator('.toasts').getByText(/^DAT received: Again sample\.dat/)).toBeVisible();
  await expect(toasts(page, 'Again sample.dat loaded.')).toHaveCount(0);
  await loaded(page, 'Again sample.dat');
  await expect(toasts(page, 'Again sample.dat loaded.')).toHaveCount(1);
});

test('a rejected DAT toasts its reason', async ({ page }) => {
  await page.goto('/#/dats');
  await page.getByLabel('Add DAT files').setInputFiles(dat('Bad sample.dat'));
  await emitEvent(page, { name: 'dat.rejected', data: { file: 'Bad sample.dat', reason: 'not a DAT' } });
  await expect(toasts(page, 'Bad sample.dat was rejected: not a DAT')).toBeVisible();
});

test('a running scan counts the folders it has walked on its card', async ({ page }) => {
  await page.goto('/#/');
  await setMockKnob(page, 'scanJobId', 74);
  await setMockKnob(page, 'scanDirs', 4);
  const card = page.locator('.card').filter({ hasText: '240 titles' });
  await card.getByRole('button', { name: 'Scan' }).click();
  const bar = card.getByRole('progressbar', { name: /^Scan of / });
  await expect(bar).toHaveAttribute('aria-valuetext', '25% · 1 of 4 folders');
  await expect(card.getByText('25% · 1 of 4 folders', { exact: true })).toBeVisible();
});

test('two scans of one platform each toast their own outcome', async ({ page }) => {
  await page.goto('/#/');
  const card = page.locator('.card').filter({ hasText: '240 titles' });
  const scan = card.getByRole('button', { name: 'Scan' });
  for (const id of [71, 72]) {
    await setMockKnob(page, 'scanJobId', id);
    await scan.click();
    await expect(scan).toBeEnabled();
  }
  for (const id of [71, 72]) {
    await emitEvent(page, {
      name: 'job.progress',
      data: { id, kind: 'scan', state: 'done', progress: { matched: id, unmatched: 0 } }
    });
  }
  await expect(page.locator('.toasts').getByText(/: 71 matched, 0 unmatched$/)).toHaveCount(1);
  await expect(page.locator('.toasts').getByText(/: 72 matched, 0 unmatched$/)).toHaveCount(1);
});

test('a failed re-read shows its error with Retry even after an earlier read succeeded', async ({ page }) => {
  await page.goto('/#/');
  const card = page.locator('.card').filter({ hasText: '240 titles' });
  await expect(card).toBeVisible();
  await setMockKnob(page, 'platformsFail', true);
  await emitEvent(page, {
    name: 'job.progress',
    data: { id: 90, kind: 'scan', state: 'done', progress: { matched: 1, unmatched: 0 } }
  });
  const alert = page.getByRole('alert').filter({ hasText: 'The platforms could not be read.' });
  await expect(alert).toBeVisible();
  await expect(card).toBeVisible();
  await setMockKnob(page, 'platformsFail', null);
  await alert.getByRole('button', { name: 'Retry' }).click();
  await expect(alert).toHaveCount(0);
  await expect(card).toBeVisible();
});

test('a scan answered with the same job after leaving the page toasts once', async ({ page }) => {
  await page.goto('/#/');
  await setMockKnob(page, 'scanJobId', 73);
  const scan = () => page.locator('.card').filter({ hasText: '240 titles' }).getByRole('button', { name: 'Scan' });
  await scan().click();
  await expect(scan()).toBeEnabled();
  await page.goto('/#/activity');
  await page.goto('/#/');
  await scan().click();
  await expect(scan()).toBeEnabled();
  await emitEvent(page, {
    name: 'job.progress',
    data: { id: 73, kind: 'scan', state: 'done', progress: { matched: 73, unmatched: 0 } }
  });
  await expect(page.locator('.toasts').getByText(/: 73 matched, 0 unmatched$/)).toHaveCount(1);
});

test('a platform that fails to save returns to the enabled grid with a toast', async ({ page }) => {
  await page.goto('/#/');
  const card = page.locator('.card').filter({ hasText: '240 titles' });
  await expect(card).toBeVisible();
  await setMockKnob(page, 'setPlatformFail', true);
  await card.getByRole('button', { name: 'Disable' }).click();
  await expect(page.locator('.toasts').getByText('The platform could not be saved.')).toBeVisible();
  await expect(card).toBeVisible();
});
