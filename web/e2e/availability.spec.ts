import { expect, test } from '@playwright/test';
import { availabilityLine } from '../src/lib/availability';
import type { TitleAvailability } from '../src/lib/types';

const found: TitleAvailability = {
  source_id: 1,
  source_name: 'Example Pack',
  file_index: 0,
  path: 'Example Pack/nova.nes',
  rom_id: 2,
  confidence: 'fuzzy'
};

test('an availability line names the file, the source and the confidence', () => {
  expect(availabilityLine(found)).toBe('nova.nes in Example Pack (name guess)');
});

test('a file without a stored confidence reads without the parenthesis', () => {
  expect(availabilityLine({ ...found, confidence: null })).toBe('nova.nes in Example Pack');
});
