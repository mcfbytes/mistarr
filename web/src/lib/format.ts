const UNITS = ['B', 'kB', 'MB', 'GB', 'TB'];

/**
 * A byte count in decimal units, such as "812 kB" or "1.4 GB": one decimal below 10, none above.
 * "unknown" for null, undefined or a non-finite count.
 */
export function bytesText(bytes: number | null | undefined): string {
  if (bytes === null || bytes === undefined || !Number.isFinite(bytes)) {
    return 'unknown';
  }
  let value = Math.max(0, bytes);
  let unit = 0;
  while (value >= 1000 && unit < UNITS.length - 1) {
    value /= 1000;
    unit += 1;
  }
  return unit === 0 ? `${Math.round(value)} B` : `${value.toFixed(value < 10 ? 1 : 0)} ${UNITS[unit]}`;
}
