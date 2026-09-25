import { expect, test } from '@playwright/test';
import { familyFor, platformArt, renderArt, type ArtFormat } from '../src/lib/art/generate';
import { SLOTS, slotVars } from '../src/lib/art/palette';
import { fixturePlatforms } from '../src/lib/fixtures';

const ids = [
  'nes', 'fds', 'snes', 'n64', 'gb', 'gbc', 'gba', 'megadrive', 's32x', 'sms', 'gg', 'sg1000',
  'pce', 'sgx', 'atari2600', 'atari5200', 'atari7800', 'lynx', 'coleco', 'intv', 'ws', 'wsc',
  'ngp', 'vectrex', 'pokemini', 'sv', 'psx', 'saturn', 'megacd', 'pcecd', 'neocd', 'neogeo', 'arcade'
];
const formats: ArtFormat[] = ['card', 'wide'];

test('each platform maps to its family, then its kind, then the default', () => {
  expect(familyFor('nes', 'cartridge')).toBe('pixel');
  expect(familyFor('snes')).toBe('parallax');
  expect(familyFor('vectrex', 'cartridge')).toBe('vector');
  expect(familyFor('gba')).toBe('lcd');
  expect(familyFor('saturn')).toBe('disc');
  expect(familyFor('n64')).toBe('poly');
  expect(familyFor('neogeo')).toBe('marquee');
  expect(familyFor('atari2600')).toBe('bands');
  expect(familyFor('amiga', 'computer')).toBe('phosphor');
  expect(familyFor('newdisc', 'disc')).toBe('disc');
  expect(familyFor('mystery', 'other')).toBe('contour');
  expect(familyFor('mystery')).toBe('contour');
});

test('the same id always draws the same art, and siblings differ', () => {
  const seen = new Set<string>();
  for (const id of ids) {
    for (const format of formats) {
      const art = renderArt(id, undefined, format);
      expect(renderArt(id, undefined, format).svg).toBe(art.svg);
      seen.add(art.svg);
    }
  }
  expect(seen.size).toBe(ids.length * formats.length);
});

/** Hue of colour slot `slot` in the art's root style. */
function hue(id: string, slot: number): number {
  const match = new RegExp(`--c${slot}:hsl\\((\\d+) `).exec(renderArt(id, undefined, 'card').svg);
  return Number(match?.[1]);
}

test('pixel siblings each take a clearly different hue', () => {
  const pixel = ['nes', 'fds', 'sms', 'sg1000', 'atari7800', 'coleco', 'intv', 'pce', 'sgx'];
  const hues = pixel.map((id) => hue(id, 0));
  for (const [i, a] of hues.entries()) {
    for (const b of hues.slice(i + 1)) {
      const apart = Math.min(Math.abs(a - b), 360 - Math.abs(a - b));
      expect(apart, `${pixel.join(',')} -> ${hues.join(',')}`).toBeGreaterThanOrEqual(20);
    }
  }
});

test('no handheld screen is tinted pea green', () => {
  for (const id of ['gb', 'gbc', 'gba', 'gg', 'lynx', 'ngp', 'ws', 'wsc', 'pokemini']) {
    for (let slot = 0; slot <= 4; slot += 1) {
      const h = hue(id, slot);
      expect(h < 60 || h > 110, `${id} slot ${slot} hue ${h}`).toBe(true);
    }
  }
});

test('art is memoised and stays small, local and free of script', () => {
  for (const id of [...ids, 'amiga', 'mystery']) {
    for (const format of formats) {
      const art = platformArt(id, id === 'amiga' ? 'computer' : undefined, format);
      expect(platformArt(id, id === 'amiga' ? 'computer' : undefined, format)).toBe(art);
      const nodes = art.svg.match(/<[a-zA-Z]/g)?.length ?? 0;
      expect(nodes, `${id} ${format}`).toBeLessThan(120);
      expect(art.svg).not.toMatch(/<script|on[a-z]+=|href=|https?:\/\/(?!www\.w3\.org\/2000\/svg)/);
    }
  }
  expect(renderArt('a"><x', undefined, 'card').svg).not.toContain('"><x');
});

test('every colour slot runs from its dark to its light value', () => {
  const vars = slotVars({ primary: 10, secondary: 40, tertiary: 190 }, 1);
  for (let i = 0; i < SLOTS; i += 1) {
    expect(vars).toMatch(new RegExp(`--c${i}:hsl\\(\\d+ calc\\([^;]*var\\(--l\\)`));
  }
});

test('each platform card shows its art, hidden from assistive tech', async ({ page }) => {
  await page.goto('/#/');
  const cards = page.locator('.art-card');
  await expect(cards).toHaveCount(fixturePlatforms.length);
  for (const card of await cards.all()) {
    const art = card.locator('[data-art-family]');
    await expect(art).toHaveCount(1);
    await expect(art).toHaveAttribute('aria-hidden', 'true');
    await expect(art.locator('svg')).toHaveCount(1);
  }
  const first = cards.first();
  await expect(first.getByRole('link', { name: 'Nintendo Entertainment System' })).toBeVisible();
  const tree = await first.ariaSnapshot();
  expect(tree).not.toMatch(/img|graphics/);
  expect(tree).toContain('heading "Nintendo Entertainment System"');
});

test('the Browse header shows the platform art behind the name', async ({ page }) => {
  await page.goto('/#/p/nes');
  const art = page.locator('.head [data-art-family="pixel"]');
  await expect(art).toHaveAttribute('aria-hidden', 'true');
  await expect(page.getByRole('heading', { name: 'Nintendo Entertainment System' })).toBeVisible();
});
