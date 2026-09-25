// Colour slots for platform art; see docs/UI.md "Platform art".

/** Hues of the theme tokens in `app.css`, plus the few that harmonise with them. */
export const HUE = {
  accent: 216,
  ok: 145,
  warn: 45,
  violet: 262,
  rose: 336,
  teal: 178,
  coral: 16
} as const;

/** The three hues one piece of art is painted with, in degrees. */
export interface Hues {
  primary: number;
  secondary: number;
  tertiary: number;
}

/** Number of colour slots a generator may use, `--c0` to `--c8`. */
export const SLOTS = 9;

type Hsl = readonly [hueOffset: 'p' | 's' | 't' | 'm' | 'x', sat: number, light: number];

// Slots: 0-1 background, 2-3 fills, 4 high-contrast line, 5 tertiary, 6-7 ramp,
// 8 silhouette ink.
const DARK: readonly Hsl[] = [
  ['p', 30, 8],
  ['s', 34, 15],
  ['p', 52, 36],
  ['s', 60, 52],
  ['p', 90, 74],
  ['t', 70, 62],
  ['m', 58, 44],
  ['x', 60, 54],
  ['p', 32, 6]
];

const LIGHT: readonly Hsl[] = [
  ['p', 45, 97],
  ['s', 50, 90],
  ['p', 48, 80],
  ['s', 60, 67],
  ['p', 60, 44],
  ['t', 58, 62],
  ['m', 60, 74],
  ['x', 68, 66],
  ['p', 28, 52]
];

function hueOf(which: Hsl[0], hues: Hues): number {
  switch (which) {
    case 'p':
      return hues.primary;
    case 's':
      return hues.secondary;
    case 't':
      return hues.tertiary;
    case 'm':
      return (hues.primary + hues.secondary) / 2;
    case 'x':
      return hues.secondary + (hues.secondary - hues.primary) / 2;
  }
}

function css(entry: Hsl, hues: Hues, sat: number): string {
  const h = Math.round(((hueOf(entry[0], hues) % 360) + 360) % 360);
  return `hsl(${h} ${Math.round(entry[1] * sat)}% ${entry[2]}%)`;
}

/**
 * Custom properties `--cNd` and `--cNl` holding each slot's dark and light
 * colour; `sat` scales saturation so a family can run muted or vivid.
 */
export function slotVars(hues: Hues, sat: number): string {
  const out: string[] = [];
  for (let i = 0; i < SLOTS; i += 1) {
    const dark = DARK[i];
    const light = LIGHT[i];
    if (dark && light) {
      out.push(`--c${i}d:${css(dark, hues, sat)}`, `--c${i}l:${css(light, hues, sat)}`);
    }
  }
  return out.join(';');
}
