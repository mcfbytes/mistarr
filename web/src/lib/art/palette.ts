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

/**
 * Per slot: hue (primary, secondary, tertiary, their midpoint, beyond the
 * secondary), then dark saturation and lightness, then light. Slots: 0-1
 * background, 2-3 fills, 4 line, 5 tertiary, 6-7 ramp, 8 silhouette ink, 9-10
 * hardware body and panels.
 */
const SLOT: readonly (readonly [number, number, number, number, number])[] = [
  [0, 30, 8, 45, 97],
  [1, 34, 15, 55, 88],
  [0, 52, 36, 62, 72],
  [1, 60, 52, 66, 62],
  [0, 90, 74, 60, 44],
  [2, 70, 62, 62, 56],
  [3, 58, 44, 64, 67],
  [4, 60, 54, 70, 58],
  [0, 32, 6, 28, 52],
  [0, 40, 14, 62, 82],
  [0, 34, 22, 52, 92]
];

/** Number of colour slots a generator may use, `--c0` to `--c10`. */
export const SLOTS = SLOT.length;

/** `from` at `--l: 0`, `to` at `--l: 1`, as a CSS percentage. */
const mix = (from: number, to: number): string => `calc(${from}% + ${to - from}% * var(--l))`;

/**
 * Custom properties `--c0` to `--c10`, each slot's colour running from its dark
 * value at `--l: 0` to its light value at `--l: 1`; `sat` scales saturation.
 */
export function slotVars(hues: Hues, sat: number): string {
  const { primary: p, secondary: s, tertiary: t } = hues;
  const choices = [p, s, t, (p + s) / 2, s + (s - p) / 2];
  return SLOT.map(([which, sd, ld, sl, ll], i) => {
    const h = Math.round((((choices[which] ?? p) % 360) + 360) % 360);
    return `--c${i}:hsl(${h} ${mix(Math.round(sd * sat), Math.round(sl * sat))} ${mix(ld, ll)})`;
  }).join(';');
}
