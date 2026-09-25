import { expect, test } from '@playwright/test';
import { posterVariation } from '../src/lib/poster';

const names = [
  'Example Quest', 'Sample Racer', 'Fixture Fighters', 'Placeholder Park', 'Synthetic Story',
  'Test Tactics', 'Mock Manor', 'Demo Dungeon', 'Trial Trail', 'Stub Squad'
];

test('a name always gets the same poster variation', () => {
  for (const name of names) {
    expect(posterVariation(name)).toEqual(posterVariation(name));
  }
  expect(posterVariation('Example Quest (USA)')).not.toEqual(posterVariation('Example Quest (Europe)'));
});

test('every variation stays within its bounds', () => {
  for (let i = 0; i < 2000; i += 1) {
    const { hue, shift, scale } = posterVariation(`Title ${i}`);
    expect(Number.isInteger(hue) && hue >= -30 && hue <= 30).toBe(true);
    expect(Number.isInteger(shift) && shift >= -4 && shift <= 4).toBe(true);
    expect(scale).toBeGreaterThanOrEqual(1.08);
    expect(scale).toBeLessThanOrEqual(1.14 + 1e-9);
    expect(scale, 'the zoom covers the shift on both sides').toBeGreaterThanOrEqual(1 + (2 * Math.abs(shift)) / 100);
  }
});

test('titles on one platform get visibly different hues', () => {
  const hues = new Set(names.map((name) => posterVariation(`${name} (USA)`).hue));
  expect(hues.size).toBeGreaterThanOrEqual(7);
  const spread = Math.max(...hues) - Math.min(...hues);
  expect(spread).toBeGreaterThanOrEqual(30);
});

test('the empty name has a variation too', () => {
  const { hue, shift, scale } = posterVariation('');
  expect([hue, shift, scale].every(Number.isFinite)).toBe(true);
});
