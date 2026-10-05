const UNITS = ["B", "kB", "MB", "GB", "TB"];

/**
 * A byte count in decimal units, such as "812 kB" or "1.4 GB": one decimal below 10, none above.
 * "unknown" for null, undefined or a non-finite count.
 */
export function bytesText(bytes: number | null | undefined): string {
  if (bytes === null || bytes === undefined || !Number.isFinite(bytes)) {
    return "unknown";
  }
  let value = Math.max(0, bytes);
  let unit = 0;
  while (
    unit < UNITS.length - 1 &&
    (value >= 1000 || roundedText(value, unit) === "1000")
  ) {
    value /= 1000;
    unit += 1;
  }
  return `${roundedText(value, unit)} ${UNITS[unit]}`;
}

/** The count rounded for its unit: whole bytes, else one decimal below 10 and none from 10. */
function roundedText(value: number, unit: number): string {
  if (unit === 0) {
    return String(Math.round(value));
  }
  const one = value.toFixed(1);
  return Number(one) < 10 ? one : value.toFixed(0);
}
