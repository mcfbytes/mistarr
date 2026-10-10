import { expect, test } from '@playwright/test';
import { fileLabel } from '../src/lib/sourceName';

test('a torrent file name loses its extension', () => {
  expect(fileLabel('example-bundle-one.torrent')).toBe('example-bundle-one');
});

test('the extension is matched whatever its case', () => {
  expect(fileLabel('Example-Bundle-Two.TORRENT')).toBe('Example-Bundle-Two');
  expect(fileLabel('Example Magnet.Magnet')).toBe('Example Magnet');
});

test('a magnet file name loses its extension', () => {
  expect(fileLabel('example-magnet.magnet')).toBe('example-magnet');
});

test('a name without an extension is left as it is', () => {
  expect(fileLabel('example-bundle')).toBe('example-bundle');
});

test('only a final extension goes', () => {
  expect(fileLabel('my.pack.v2.torrent')).toBe('my.pack.v2');
  expect(fileLabel('a.torrent.bak')).toBe('a.torrent.bak');
  expect(fileLabel('')).toBe('');
  expect(fileLabel('.torrent')).toBe('.torrent');
});
