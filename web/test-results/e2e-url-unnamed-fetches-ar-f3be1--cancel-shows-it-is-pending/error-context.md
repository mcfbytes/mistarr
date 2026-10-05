# Instructions

- Following Playwright test failed.
- Explain why, be concise, respect Playwright best practices.
- Provide a snippet of code with the fix, if possible.

# Test info

- Name: e2e/url.spec.ts >> unnamed fetches are told apart without the URL and a cancel shows it is pending
- Location: e2e/url.spec.ts:94:1

# Error details

```
Error: page.goto: Protocol error (Page.navigate): Cannot navigate to invalid URL
Call log:
  - navigating to "/#/dats", waiting until "load"

```

# Test source

```ts
  1   | import { test, expect, type Page } from '@playwright/test';
  2   | 
  3   | const toasts = (page: Page) => page.locator('.toasts');
  4   | 
  5   | async function expectUrlField(page: Page): Promise<void> {
  6   |   const input = page.getByLabel('Add from a URL');
  7   |   await expect(input).toBeVisible();
  8   |   await expect(input).toHaveAttribute('autocomplete', 'off');
  9   |   await expect(input).toHaveAttribute('placeholder', 'https://example.invalid/…');
  10  |   await expect(page.locator('form.url').filter({ hasText: 'Add from a URL' })).toHaveAttribute('autocomplete', 'off');
  11  |   await expect(page.locator('datalist')).toHaveCount(0);
  12  |   await expect(page.getByRole('button', { name: 'Fetch' })).toBeVisible();
  13  | }
  14  | 
  15  | test('the URL field is on DATs, Sources and both wizard steps', async ({ page }) => {
  16  |   await page.goto('/#/dats');
  17  |   await expectUrlField(page);
  18  |   await page.goto('/#/sources');
  19  |   await expectUrlField(page);
  20  |   await page.goto('/#/wizard');
  21  |   await page.getByRole('button', { name: 'Next' }).click();
  22  |   await expectUrlField(page);
  23  |   await page.getByRole('button', { name: 'Next' }).click();
  24  |   await page.getByRole('button', { name: 'Next' }).click();
  25  |   await expectUrlField(page);
  26  | });
  27  | 
  28  | test('a fetch shows its progress in the activity panel and a toast when it lands', async ({ page }) => {
  29  |   await page.goto('/#/dats');
  30  |   await page.getByLabel('Add from a URL').fill('https://example.invalid/dats/Example.dat');
  31  |   await page.getByRole('button', { name: 'Fetch' }).click();
  32  |   await expect(toasts(page).getByText('Fetching the file. Its progress is under Background work.')).toBeVisible();
  33  |   await expect(page.getByLabel('Add from a URL')).toHaveValue('');
  34  |   await page.getByRole('button', { name: /^Background work:/ }).click();
  35  |   const running = page.getByRole('region', { name: 'Background work' }).getByRole('list', { name: 'Running' });
  36  |   const row = running.getByRole('listitem').filter({ hasText: 'URL fetch: Example.dat' });
  37  |   await expect(row).toBeVisible();
  38  |   await expect(row.getByRole('progressbar')).toBeVisible();
  39  |   await expect(row).toContainText(/Receiving · \d+% · [\d.]+ MB of 2\.4 MB/);
  40  |   await expect(row.getByRole('button', { name: 'Cancel URL fetch: Example.dat' })).toBeVisible();
  41  |   await expect(row.getByRole('link')).toHaveAttribute('href', '#/activity');
  42  |   await expect(toasts(page).getByText('DAT received: Example.dat. Queued.')).toBeVisible({ timeout: 10_000 });
  43  |   await expect(row).toHaveCount(0);
  44  | });
  45  | 
  46  | test('a page that is not a DAT or torrent is refused with a toast', async ({ page }) => {
  47  |   await page.goto('/#/sources');
  48  |   await page.getByLabel('Add from a URL').fill('https://example.invalid/index.html');
  49  |   await page.getByRole('button', { name: 'Fetch' }).click();
  50  |   await expect(
  51  |     toasts(page).getByText("The fetch failed: This isn't a DAT, DAT pack or torrent file.")
  52  |   ).toBeVisible({ timeout: 10_000 });
  53  | });
  54  | 
  55  | test('a bad link is refused at once and a magnet is placed', async ({ page }) => {
  56  |   await page.goto('/#/sources');
  57  |   const input = page.getByLabel('Add from a URL');
  58  |   await input.fill('ftp://example.invalid/a.dat');
  59  |   await page.getByRole('button', { name: 'Fetch' }).click();
  60  |   await expect(toasts(page).getByText('Only http, https and magnet links are accepted.')).toBeVisible();
  61  |   await expect(input).toHaveValue('ftp://example.invalid/a.dat');
  62  |   await input.fill('magnet:?xt=urn:btih:example');
  63  |   await page.getByRole('button', { name: 'Fetch' }).click();
  64  |   await expect(toasts(page).getByText(/^Magnet received: Example magnet\.magnet\./)).toBeVisible();
  65  | });
  66  | 
  67  | test('a running fetch can be cancelled from the panel', async ({ page }) => {
  68  |   await page.goto('/#/dats');
  69  |   await page.getByLabel('Add from a URL').fill('https://example.invalid/slow.zip');
  70  |   await page.getByRole('button', { name: 'Fetch' }).click();
  71  |   await page.getByRole('button', { name: /^Background work:/ }).click();
  72  |   const panel = page.getByRole('region', { name: 'Background work' });
  73  |   await panel.getByRole('button', { name: /^Cancel URL fetch/ }).click();
  74  |   await expect(toasts(page).getByText('The fetch was cancelled.')).toBeVisible();
  75  | });
  76  | 
  77  | test('no link is remembered after a reload', async ({ page }) => {
  78  |   const link = 'https://example.invalid/kept/Example.dat';
  79  |   await page.goto('/#/dats');
  80  |   await page.getByLabel('Add from a URL').fill(link);
  81  |   await page.getByRole('button', { name: 'Fetch' }).click();
  82  |   await expect(toasts(page).getByText('Fetching the file. Its progress is under Background work.')).toBeVisible();
  83  |   await page.getByLabel('Add from a URL').fill('https://example.invalid/typed-not-sent.dat');
  84  |   await page.reload();
  85  |   await expect(page.getByLabel('Add from a URL')).toHaveValue('');
  86  |   const stored = await page.evaluate(() =>
  87  |     [localStorage, sessionStorage]
  88  |       .flatMap((store) => Object.keys(store).map((key) => `${key}=${store.getItem(key) ?? ''}`))
  89  |       .join('\n')
  90  |   );
  91  |   expect(stored).not.toContain('example.invalid');
  92  | });
  93  | 
  94  | test('unnamed fetches are told apart without the URL and a cancel shows it is pending', async ({ page }) => {
> 95  |   await page.goto('/#/dats');
      |              ^ Error: page.goto: Protocol error (Page.navigate): Cannot navigate to invalid URL
  96  |   for (const link of ['https://example.invalid/wait-one', 'https://example.invalid/wait-two']) {
  97  |     await page.getByLabel('Add from a URL').fill(link);
  98  |     await page.getByRole('button', { name: 'Fetch' }).click();
  99  |     await expect(page.getByLabel('Add from a URL')).toHaveValue('');
  100 |   }
  101 |   await page.getByRole('button', { name: /^Background work:/ }).click();
  102 |   const panel = page.getByRole('region', { name: 'Background work' });
  103 |   const titles = panel.getByRole('link', { name: /^URL fetch: sent at / });
  104 |   await expect(titles).toHaveCount(2);
  105 |   const [first, second] = await titles.allTextContents();
  106 |   expect(first).not.toEqual(second);
  107 |   expect(`${first} ${second}`).not.toMatch(/example|wait/);
  108 |   const cancel = panel.getByRole('button', { name: `Cancel ${first}` });
  109 |   await cancel.click();
  110 |   const pending = panel.getByRole('button', { name: `Cancelling ${first}` });
  111 |   await expect(pending).toBeDisabled();
  112 |   await expect(pending).toHaveText('Cancelling…');
  113 |   await expect(toasts(page).getByText('The fetch was cancelled.')).toBeVisible();
  114 | });
  115 | 
  116 | test('a fetch answered without a job id still says when it fails', async ({ page }) => {
  117 |   await page.goto('/#/sources');
  118 |   await page.getByLabel('Add from a URL').fill('https://example.invalid/busy.html');
  119 |   await page.getByRole('button', { name: 'Fetch' }).click();
  120 |   await expect(
  121 |     toasts(page).getByText("The fetch failed: This isn't a DAT, DAT pack or torrent file.")
  122 |   ).toBeVisible({ timeout: 10_000 });
  123 | });
  124 | 
```