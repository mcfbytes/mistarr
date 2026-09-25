import { test, type Page } from '@playwright/test';
import { renderArt } from '../src/lib/art/generate';
import { fixturePlatforms } from '../src/lib/fixtures';

// README screenshots from mock data, written only when README_SHOTS_DIR is set; see docs/TESTING.md.
const dir = process.env.README_SHOTS_DIR;

test.use({ deviceScaleFactor: 2, locale: 'en-US', timezoneId: 'UTC' });

/** A few minutes after the fixtures' job times, so "ago" reads as recent. */
const NOW = new Date(1_770_032_700_000);

/** Blocks cover art so every poster is the generated one; covers are third-party images. */
async function noCovers(page: Page): Promise<void> {
  await page.route(/^https?:\/\/(?!localhost)/, (route) => route.abort());
}

async function openPanel(page: Page): Promise<void> {
  await page.getByRole('button', { name: /^Background work:/ }).click();
  await page.getByRole('region', { name: 'Background work' }).waitFor();
  await page.waitForTimeout(1200);
}

interface Shot {
  name: string;
  url: string;
  width: number;
  height: number;
  act?: (page: Page) => Promise<void>;
}

const shots: Shot[] = [
  { name: 'platforms', url: '/?mock=showcase#/', width: 1280, height: 860 },
  { name: 'browse', url: '/?mock=showcase#/p/snes', width: 1280, height: 760 },
  { name: 'title', url: '/?mock=showcase#/t/10', width: 1280, height: 660 },
  { name: 'activity', url: '/#/dats', width: 1280, height: 800, act: openPanel },
  { name: 'system', url: '/?mock=showcase#/system', width: 1280, height: 800 },
  { name: 'wizard', url: '/?mock=showcase#/wizard', width: 1280, height: 640 },
  { name: 'phone', url: '/?mock=showcase#/', width: 390, height: 844 }
];

for (const scheme of ['dark', 'light'] as const) {
  for (const shot of shots) {
    test(`${shot.name} ${scheme}`, async ({ page }) => {
      test.skip(!dir, 'README_SHOTS_DIR is not set');
      await noCovers(page);
      await page.clock.install({ time: NOW });
      await page.emulateMedia({ colorScheme: scheme, reducedMotion: 'reduce' });
      await page.setViewportSize({ width: shot.width, height: shot.height });
      await page.goto(shot.url);
      await page.waitForLoadState('networkidle');
      await page.waitForTimeout(400);
      await shot.act?.(page);
      await page.screenshot({ path: `${dir ?? ''}/${shot.name}-${scheme}.png` });
    });
  }
}

type Scheme = 'dark' | 'light';

/** The page tokens of `app.css` as the running app computes them for `scheme`. */
interface Theme {
  bg: string;
  fg: string;
  dim: string;
  border: string;
  l: number;
}

async function theme(page: Page, scheme: Scheme): Promise<Theme> {
  await page.emulateMedia({ colorScheme: scheme });
  await page.goto('/?mock=idle#/');
  const [bg, fg, dim, border] = await page.evaluate(() => {
    const css = getComputedStyle(document.documentElement);
    return ['--bg', '--fg', '--fg-dim', '--border'].map((name) => css.getPropertyValue(name).trim());
  });
  return { bg: bg ?? '', fg: fg ?? '', dim: dim ?? '', border: border ?? '', l: scheme === 'light' ? 1 : 0 };
}

/** A page of platform art tiles beside the project name, drawn from the art generator alone. */
function banner(t: Theme, ids: string[], cols: number, big: boolean): string {
  const tiles = ids
    .map((id) => {
      const kind = fixturePlatforms.find((p) => p.id === id)?.kind;
      return `<div class="tile">${renderArt(id, kind, 'card').svg}</div>`;
    })
    .join('');
  return `<!doctype html><html><head><style>
    html, body { margin: 0; height: 100%; background: ${t.bg}; color: ${t.fg};
      font-family: system-ui, -apple-system, 'Segoe UI', sans-serif; }
    body { display: flex; align-items: center; gap: 48px; padding: 0 48px; box-sizing: border-box; --l: ${t.l}; }
    .text { flex: 1; min-width: 0; }
    h1 { margin: 0; font-size: ${big ? 96 : 80}px; letter-spacing: -0.03em; line-height: 1; }
    p { margin: 20px 0 0; font-size: ${big ? 30 : 24}px; line-height: 1.35; color: ${t.dim}; }
    .grid { display: grid; grid-template-columns: repeat(${cols}, 360px); gap: 16px; }
    .tile { width: 360px; height: 160px; border-radius: 10px; overflow: hidden; border: 1px solid ${t.border}; }
    .tile svg { display: block; width: 100%; height: 100%; }
  </style></head><body>
    <div class="text"><h1>mistarr</h1><p>Verify and organise MiSTer game libraries, on the board itself.</p></div>
    <div class="grid">${tiles}</div>
  </body></html>`;
}

test.describe('banners', () => {
  test.use({ deviceScaleFactor: 2 });

  for (const scheme of ['dark', 'light'] as const) {
    test(`hero ${scheme}`, async ({ page }) => {
      test.skip(!dir, 'README_SHOTS_DIR is not set');
      await page.setViewportSize({ width: 1280, height: 400 });
      await page.setContent(banner(await theme(page, scheme), ['snes', 'gba', 'saturn', 'arcade'], 2, false));
      await page.screenshot({ path: `${dir ?? ''}/hero-${scheme}.png` });
    });
  }
});

test.describe('social preview', () => {
  test.use({ deviceScaleFactor: 1 });

  test('social preview', async ({ page }) => {
    test.skip(!dir, 'README_SHOTS_DIR is not set');
    await page.setViewportSize({ width: 1280, height: 640 });
    await page.setContent(banner(await theme(page, 'dark'), ['nes', 'snes', 'n64', 'gba', 'saturn', 'arcade'], 2, true));
    await page.screenshot({ path: `${dir ?? ''}/social-preview.png` });
  });
});
