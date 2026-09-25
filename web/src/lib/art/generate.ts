// Procedural platform backdrops; see docs/UI.md "Platform art".
import type { PlatformKind } from '../types';
import { HUE, slotVars, type Hues } from './palette';

/** A motif family: one visual idea shared by the platforms of an era or medium. */
export type ArtFamily =
  | 'pixel'
  | 'parallax'
  | 'bands'
  | 'vector'
  | 'lcd'
  | 'disc'
  | 'poly'
  | 'marquee'
  | 'phosphor'
  | 'contour';

/** `card` fills a Platforms card banner, `wide` the Browse header. */
export type ArtFormat = 'card' | 'wide';

/** Generated art: its family and a self-contained `<svg>` string. */
export interface Art {
  family: ArtFamily;
  svg: string;
}

const FAMILY_BY_ID: Readonly<Record<string, ArtFamily>> = {
  nes: 'pixel',
  fds: 'pixel',
  sms: 'pixel',
  sg1000: 'pixel',
  atari7800: 'pixel',
  coleco: 'pixel',
  intv: 'pixel',
  pce: 'pixel',
  sgx: 'pixel',
  sv: 'pixel',
  snes: 'parallax',
  megadrive: 'parallax',
  s32x: 'parallax',
  atari2600: 'bands',
  atari5200: 'bands',
  vectrex: 'vector',
  gb: 'lcd',
  gbc: 'lcd',
  gba: 'lcd',
  gg: 'lcd',
  lynx: 'lcd',
  ngp: 'lcd',
  ws: 'lcd',
  wsc: 'lcd',
  pokemini: 'lcd',
  psx: 'disc',
  saturn: 'disc',
  megacd: 'disc',
  pcecd: 'disc',
  neocd: 'disc',
  n64: 'poly',
  arcade: 'marquee',
  neogeo: 'marquee'
};

const FAMILY_BY_KIND: Readonly<Record<PlatformKind, ArtFamily>> = {
  cartridge: 'pixel',
  disc: 'disc',
  computer: 'phosphor',
  romset: 'marquee',
  arcade: 'marquee',
  other: 'contour'
};

interface Tone {
  /** Base primary hue, the offset to the secondary and to the tertiary. */
  base: number;
  second: number;
  third: number;
  /** How far a platform's primary hue may wander from `base`, either way. */
  spread: number;
  sat: number;
}

const TONES: Readonly<Record<ArtFamily, Tone>> = {
  pixel: { base: HUE.rose + 18, second: 30, third: 170, spread: 22, sat: 0.85 },
  parallax: { base: HUE.violet, second: 74, third: 150, spread: 30, sat: 0.95 },
  bands: { base: HUE.warn, second: -34, third: 150, spread: 12, sat: 1 },
  vector: { base: HUE.accent - 16, second: -30, third: 120, spread: 14, sat: 0.9 },
  lcd: { base: HUE.teal - 12, second: -30, third: 160, spread: 28, sat: 0.8 },
  disc: { base: HUE.accent + 20, second: 60, third: 150, spread: 34, sat: 0.85 },
  poly: { base: HUE.ok, second: 60, third: -110, spread: 14, sat: 0.7 },
  marquee: { base: HUE.rose, second: 70, third: -130, spread: 24, sat: 1 },
  phosphor: { base: HUE.ok - 10, second: 20, third: 180, spread: 10, sat: 0.9 },
  contour: { base: HUE.accent, second: 46, third: 170, spread: 20, sat: 0.8 }
};

/** Canvas size per format, in viewBox units. */
const SIZE: Readonly<Record<ArtFormat, readonly [number, number]>> = {
  card: [360, 160],
  wide: [960, 200]
};

/** The family an id draws with: its own mapping, else its kind's, else the default. */
export function familyFor(id: string, kind?: PlatformKind): ArtFamily {
  return FAMILY_BY_ID[id] ?? (kind ? FAMILY_BY_KIND[kind] : 'contour');
}

/** FNV-1a over the UTF-16 code units of `text`. */
function hash(text: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < text.length; i += 1) {
    h ^= text.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return h >>> 0;
}

/** Mulberry32: small, fast and good enough to decorate with. */
function prng(seed: number): () => number {
  let a = seed;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const num = (v: number): string => String(Math.round(v * 10) / 10);
const op = (v: number): string => String(Math.round(v * 100) / 100);
const fill = (slot: number, opacity = 1): string =>
  `style="fill:var(--c${slot})${opacity < 1 ? `;fill-opacity:${op(opacity)}` : ''}"`;
const stroke = (slot: number, width: number, opacity = 1): string =>
  `style="fill:none;stroke:var(--c${slot});stroke-width:${num(width)};stroke-opacity:${op(opacity)};stroke-linejoin:round"`;
const stop = (offset: number, slot: number, opacity = 1): string =>
  `<stop offset="${num(offset)}" style="stop-color:var(--c${slot});stop-opacity:${op(opacity)}"/>`;
const square = (x: number, y: number, w: number, h = w): string =>
  `M${num(x)} ${num(y)}h${num(w)}v${num(h)}h${num(-w)}z`;
const dot = (x: number, y: number, r: number): string =>
  `M${num(x - r)} ${num(y)}a${num(r)} ${num(r)} 0 1 0 ${num(2 * r)} 0a${num(r)} ${num(r)} 0 1 0 ${num(-2 * r)} 0`;

interface Canvas {
  w: number;
  h: number;
  rand: () => number;
  /** Prefix for element ids, unique per platform and format. */
  uid: string;
  defs: string[];
  body: string[];
}

function between(c: Canvas, lo: number, hi: number): number {
  return lo + (hi - lo) * c.rand();
}

function pick<T>(c: Canvas, items: readonly [T, ...T[]]): T {
  return items[Math.floor(c.rand() * items.length)] ?? items[0];
}

/** Vertical background gradient from slot `top` to slot `bottom`. */
function backdrop(c: Canvas, top = 0, bottom = 1): void {
  c.defs.push(
    `<linearGradient id="${c.uid}bg" x1="0" y1="0" x2="0" y2="1">${stop(0, top)}${stop(1, bottom)}</linearGradient>`
  );
  c.body.push(`<rect width="${c.w}" height="${c.h}" fill="url(#${c.uid}bg)"/>`);
}

/** Horizontal scanlines in slot `slot` over the whole canvas. */
function scanlines(c: Canvas, slot: number, opacity: number, pitch = 3): void {
  c.defs.push(
    `<pattern id="${c.uid}sl" width="8" height="${pitch}" patternUnits="userSpaceOnUse"><rect width="8" height="1" ${fill(slot, opacity)}/></pattern>`
  );
  c.body.push(`<rect width="${c.w}" height="${c.h}" fill="url(#${c.uid}sl)"/>`);
}

/** Radial glow centred at (x, y) fading from slot `slot` to nothing. */
function glow(c: Canvas, name: string, x: number, y: number, rx: number, ry: number, slot: number, opacity: number): void {
  c.defs.push(
    `<radialGradient id="${c.uid}${name}">${stop(0, slot, opacity)}${stop(1, slot, 0)}</radialGradient>`
  );
  c.body.push(
    `<ellipse cx="${num(x)}" cy="${num(y)}" rx="${num(rx)}" ry="${num(ry)}" fill="url(#${c.uid}${name})"/>`
  );
}

const BAYER = [0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5];

/** 8-bit cartridges: a chunky tile mosaic, an ordered dither through a four-step ramp. */
function pixel(c: Canvas): void {
  backdrop(c);
  const cell = pick(c, [10, 12]);
  const aspect = c.h / c.w;
  const radial = c.rand() < 0.5;
  const fx = between(c, 0.1, 0.9) - 0.5;
  const fy = (between(c, 0.2, 0.8) - 0.5) * aspect;
  const reach = between(c, 0.45, 0.6);
  const angle = between(c, -0.6, 0.6) + (c.rand() < 0.5 ? 0 : Math.PI);
  const wave = between(c, 0.04, 0.1);
  const freq = between(c, 5, 10);
  const phase = between(c, 0, 6.3);
  const cols = Math.ceil(c.w / cell);
  const rows = Math.ceil(c.h / cell);
  const ramp = [2, 6, 3, 7];
  const levels = ramp.map(() => '');
  for (let j = 0; j < rows; j += 1) {
    for (let i = 0; i < cols; i += 1) {
      const x = (i + 0.5) / cols - 0.5;
      const y = ((j + 0.5) / rows - 0.5) * aspect;
      const v =
        (radial
          ? 1 - Math.hypot(x - fx, y - fy) / reach
          : 0.45 + (x * Math.cos(angle) + y * Math.sin(angle)) * 1.1) +
        Math.sin(x * freq + phase) * wave;
      const t = ((BAYER[(j % 4) * 4 + (i % 4)] ?? 0) + 0.5) / 16;
      const level = Math.floor(v * ramp.length + t - 0.45) - 1;
      if (level >= 0) {
        const k = Math.min(level, ramp.length - 1);
        levels[k] = `${levels[k] ?? ''}${square(i * cell, j * cell, cell - 1)}`;
      }
    }
  }
  levels.forEach((d, k) => {
    c.body.push(`<path d="${d}" ${fill(ramp[k] ?? 2, k === 0 ? 0.7 : 1)}/>`);
  });
}

/** Sum of seeded sines, for ridge lines and height fields. */
function waves(c: Canvas, count: number): (x: number) => number {
  const parts = Array.from({ length: count }, (_, k) => ({
    f: between(c, 1, 2) * (k + 1) ** 1.4,
    p: between(c, 0, 6.3),
    a: 1 / (k + 1)
  }));
  const norm = parts.reduce((s, q) => s + q.a, 0);
  return (x) => parts.reduce((s, q) => s + Math.sin(x * q.f + q.p) * q.a, 0) / norm;
}

/** 16-bit era: a banded sun behind stepped parallax ridges. */
function parallax(c: Canvas): void {
  backdrop(c, 0, 3);
  const { w, h } = c;
  const sx = w * (pick(c, [0.24, 0.5, 0.76]) + between(c, -0.06, 0.06));
  const sy = h * 0.42;
  const sr = h * between(c, 0.2, 0.3);
  c.defs.push(
    `<linearGradient id="${c.uid}sun" x1="0" y1="0" x2="0" y2="1">${stop(0, 4)}${stop(1, 7)}</linearGradient>`
  );
  let sun = '';
  const slices = 12;
  for (let k = 0; k < slices; k += 1) {
    const y0 = sy - sr + (2 * sr * k) / slices;
    const gap = k > slices / 2 ? ((k - slices / 2) * sr) / 40 : 0;
    const band = (2 * sr) / slices - gap;
    const mid = y0 + band / 2 - sy;
    const half = Math.sqrt(Math.max(0, sr * sr - mid * mid));
    sun += square(sx - half, y0, 2 * half, band);
  }
  glow(c, 'halo', sx, sy, sr * 2.4, sr * 1.6, 4, 0.35);
  c.body.push(`<path d="${sun}" fill="url(#${c.uid}sun)"/>`);
  let stars = '';
  for (let k = 0; k < 18; k += 1) {
    stars += square(between(c, 0, w), between(c, 0, h * 0.4), 2);
  }
  c.body.push(`<path d="${stars}" ${fill(4, 0.6)}/>`);
  const layers = [
    { y: 0.56, amp: 0.2, slot: 2, op: 0.45 },
    { y: 0.68, amp: 0.16, slot: 2, op: 0.85 },
    { y: 0.82, amp: 0.1, slot: 8, op: 1 }
  ];
  const step = 4;
  for (const layer of layers) {
    const ridge = waves(c, 3);
    const scale = between(c, 1.6, 3) / (w / 360);
    let d = `M0 ${h}`;
    for (let x = 0; x <= w + step; x += step) {
      const y = Math.round((h * (layer.y - layer.amp * ridge(x * scale * 0.01))) / step) * step;
      d += `V${y}H${x + step}`;
    }
    c.body.push(`<path d="${d}V${h}z" ${fill(layer.slot, layer.op)}/>`);
  }
}

/** Early home consoles: bold horizontal colour bands under a scanline raster. */
function bands(c: Canvas): void {
  backdrop(c);
  const { w, h } = c;
  const ramp = [7, 3, 6, 2];
  const count = ramp.length;
  const top = h * between(c, 0.3, 0.42);
  const weights = Array.from({ length: count }, (_, k) => 1 + k * between(c, 0.35, 0.7));
  const total = weights.reduce((s, v) => s + v, 0);
  let y = top;
  weights.forEach((weight, k) => {
    const bh = ((h - top) * weight) / total;
    c.body.push(`<rect y="${num(y)}" width="${w}" height="${num(bh + 0.5)}" ${fill(ramp[k] ?? 2)}/>`);
    y += bh;
  });
  const block = w / 40;
  let blocks = '';
  let x = between(c, 0, 6) * block;
  while (x < w) {
    const span = Math.ceil(between(c, 1, 4)) * block;
    const rise = Math.ceil(between(c, 1, 5)) * 6;
    if (c.rand() < 0.55) {
      blocks += square(x, top - rise, span, rise);
    }
    x += span + Math.ceil(between(c, 1, 5)) * block;
  }
  c.body.push(`<path d="${blocks}" ${fill(2)}/>`);
  scanlines(c, 0, 0.3);
}

/** Vector: a twisting wireframe tunnel with a soft glow on black. */
function vector(c: Canvas): void {
  backdrop(c, 0, 0);
  const { w, h } = c;
  const cx = w * between(c, 0.55, 0.72);
  const cy = h * between(c, 0.42, 0.58);
  const sides = pick(c, [4, 5, 6, 8]);
  const twist = between(c, 0.12, 0.28) * (c.rand() < 0.5 ? -1 : 1);
  const shrink = between(c, 0.66, 0.74);
  let r = h * 1.7;
  let prev: [number, number][] = [];
  let d = '';
  for (let k = 0; k < 11; k += 1) {
    const ring: [number, number][] = [];
    for (let s = 0; s < sides; s += 1) {
      const a = (s / sides) * Math.PI * 2 + k * twist;
      ring.push([cx + Math.cos(a) * r, cy + Math.sin(a) * r * 0.8]);
    }
    d += `M${ring.map(([x, y]) => `${num(x)} ${num(y)}`).join('L')}z`;
    ring.forEach(([x, y], s) => {
      const p = prev[s];
      if (p) {
        d += `M${num(p[0])} ${num(p[1])}L${num(x)} ${num(y)}`;
      }
    });
    prev = ring;
    r *= shrink;
  }
  glow(c, 'core', cx, cy, w * 0.2, h * 0.5, 3, 0.35);
  c.body.push(`<path d="${d}" ${stroke(3, 4, 0.16)}/>`, `<path d="${d}" ${stroke(4, 1, 0.8)}/>`);
  let stars = '';
  for (let k = 0; k < 14; k += 1) {
    stars += dot(between(c, 0, w * 0.45), between(c, 0, h), between(c, 0.6, 1.3));
  }
  c.body.push(`<path d="${stars}" ${fill(4, 0.7)}/>`);
}

/** A mirrored random sprite, as rows of booleans. */
function sprite(c: Canvas, size: number): boolean[][] {
  const half = Math.ceil(size / 2);
  return Array.from({ length: size }, () => {
    const left = Array.from({ length: half }, () => c.rand() < 0.5);
    return [...left, ...left.slice(0, size - half).reverse()];
  });
}

/** Handhelds: an LCD dot matrix with lit sprites, their ghosting and a backlight glow. */
function lcd(c: Canvas): void {
  backdrop(c, 1, 0);
  const { w, h } = c;
  const cell = pick(c, [5, 6]);
  c.defs.push(
    `<pattern id="${c.uid}dm" width="${cell}" height="${cell}" patternUnits="userSpaceOnUse"><rect x=".5" y=".5" width="${cell - 1}" height="${cell - 1}" ${fill(2, 0.3)}/></pattern>`
  );
  glow(c, 'bl', w * between(c, 0.3, 0.7), h * 0.35, w * 0.55, h * 0.9, 3, 0.4);
  c.body.push(`<rect width="${w}" height="${h}" fill="url(#${c.uid}dm)"/>`);
  let lit = '';
  let ghost = '';
  const count = Math.round(w / 110);
  const size = pick(c, [7, 8]);
  for (let k = 0; k < count; k += 1) {
    const ox = Math.round(((k + between(c, 0.15, 0.55)) * w) / count / cell) * cell;
    const oy = Math.round((h * between(c, 0.18, 0.6)) / cell) * cell;
    sprite(c, size).forEach((row, j) => {
      row.forEach((on, i) => {
        if (on) {
          lit += square(ox + i * cell + 0.5, oy + j * cell + 0.5, cell - 1);
          ghost += square(ox + (i - 1) * cell + 0.5, oy + j * cell + 0.5, cell - 1);
        }
      });
    });
  }
  c.body.push(`<path d="${ghost}" ${fill(4, 0.18)}/>`, `<path d="${lit}" ${fill(4, 0.8)}/>`);
}

/** Discs: concentric tracks under a thin-film sheen and a sweep of light. */
function disc(c: Canvas): void {
  backdrop(c);
  const { w, h } = c;
  const cx = w * (pick(c, [0.3, 0.72, 0.78]) + between(c, -0.05, 0.05));
  const cy = h * between(c, 0.36, 0.5);
  const r = h * between(c, 0.95, 1.25);
  const tilt = between(c, 0, Math.PI);
  const fx = Math.cos(tilt) * r;
  const fy = Math.sin(tilt) * r;
  c.defs.push(
    `<linearGradient id="${c.uid}film" gradientUnits="userSpaceOnUse" x1="${num(cx - fx)}" y1="${num(cy - fy)}" x2="${num(cx + fx)}" y2="${num(cy + fy)}">${stop(0, 5)}${stop(0.3, 3)}${stop(0.5, 4)}${stop(0.7, 7)}${stop(1, 5)}</linearGradient>`,
    `<radialGradient id="${c.uid}sweep" gradientUnits="userSpaceOnUse" cx="${num(cx)}" cy="${num(cy)}" r="${num(r)}">${stop(0.2, 4, 0)}${stop(0.7, 4, 0.35)}${stop(1, 4, 0)}</radialGradient>`
  );
  c.body.push(`<circle cx="${num(cx)}" cy="${num(cy)}" r="${num(r * 0.62)}" ${stroke(2, r * 0.7, 0.35)}/>`);
  let tracks = '';
  let rr = r * 0.3;
  const gaps = [between(c, 0.4, 0.6), between(c, 0.65, 0.9)].map((g) => g * r);
  while (rr < r) {
    const inGap = gaps.some((g) => rr > g && rr < g + r * 0.05);
    tracks += inGap
      ? ''
      : `<circle cx="${num(cx)}" cy="${num(cy)}" r="${num(rr)}" stroke="url(#${c.uid}film)" style="fill:none;stroke-width:${num(between(c, 0.4, 2.2))};stroke-opacity:${op(between(c, 0.25, 0.8))}"/>`;
    rr += between(c, 3, 9) * (h / 160);
  }
  c.body.push(tracks);
  const a0 = between(c, 2.4, 3.6);
  for (const [start, span] of [
    [a0, 0.32],
    [a0 + Math.PI, 0.22]
  ] as const) {
    const x1 = cx + Math.cos(start) * r;
    const y1 = cy + Math.sin(start) * r;
    const x2 = cx + Math.cos(start + span) * r;
    const y2 = cy + Math.sin(start + span) * r;
    c.body.push(
      `<path d="M${num(cx)} ${num(cy)}L${num(x1)} ${num(y1)}A${num(r)} ${num(r)} 0 0 1 ${num(x2)} ${num(y2)}z" fill="url(#${c.uid}sweep)"/>`
    );
  }
  c.body.push(
    `<circle cx="${num(cx)}" cy="${num(cy)}" r="${num(r * 0.15)}" ${fill(0)}/>`,
    `<circle cx="${num(cx)}" cy="${num(cy)}" r="${num(r * 0.24)}" ${stroke(4, 1, 0.45)}/>`
  );
}

/** Early 3D: flat-shaded low-poly terrain rolling towards a horizon. */
function poly(c: Canvas): void {
  backdrop(c, 0, 3);
  const { w, h } = c;
  const horizon = h * between(c, 0.24, 0.32);
  glow(c, 'sky', w * between(c, 0.3, 0.7), horizon, w * 0.5, h * 0.3, 4, 0.35);
  const cols = Math.round(w / 30);
  const rows = 7;
  const field = waves(c, 3);
  const field2 = waves(c, 2);
  const height = (i: number, j: number): number => field(i * 0.35 + j * 0.2) * 0.6 + field2(j * 0.5 - i * 0.15) * 0.4;
  const pts: [number, number, number][][] = [];
  for (let j = 0; j <= rows; j += 1) {
    const t = j / rows;
    const y0 = horizon + (h - horizon) * 1.15 * t ** 1.5;
    const spread = 0.7 + 1.6 * t;
    const row: [number, number, number][] = [];
    for (let i = -1; i <= cols + 1; i += 1) {
      const hz = height(i, j);
      const jitter = j > 0 ? between(c, -0.3, 0.3) : 0;
      const x = w / 2 + ((i + jitter) / cols - 0.5) * w * spread;
      row.push([x, y0 - hz * 12 * (0.3 + t), hz]);
    }
    pts.push(row);
  }
  const buckets: string[] = ['', '', '', '', '', ''];
  let mesh = '';
  const tri = (a: [number, number, number], b: [number, number, number], d: [number, number, number]): void => {
    const slope = (b[2] - a[2]) * 1.4 + (d[2] - a[2]) * 0.9;
    const shade = Math.min(0.999, Math.max(0, 0.45 + slope + between(c, -0.08, 0.08)));
    const path = `M${num(a[0])} ${num(a[1])}L${num(b[0])} ${num(b[1])}L${num(d[0])} ${num(d[1])}z`;
    const k = Math.floor(shade * buckets.length);
    buckets[k] = `${buckets[k] ?? ''}${path}`;
    mesh += path;
  };
  for (let j = 0; j < rows; j += 1) {
    const top = pts[j] ?? [];
    const bottom = pts[j + 1] ?? [];
    for (let i = 0; i + 1 < top.length; i += 1) {
      const a = top[i];
      const b = top[i + 1];
      const d = bottom[i];
      const e = bottom[i + 1];
      if (a && b && d && e) {
        tri(a, b, d);
        tri(b, e, d);
      }
    }
  }
  c.body.push(`<path d="${mesh}" ${fill(2, 0.9)}/>`);
  buckets.forEach((d, k) => {
    if (d) {
      c.body.push(`<path d="${d}" ${fill(4, 0.03 + k * 0.055)}/>`);
    }
  });
  c.body.push(`<path d="${mesh}" ${stroke(0, 0.6, 0.35)}/>`);
}

/** Arcade: a starfield over a glowing horizon, framed by marquee bulbs. */
function marquee(c: Canvas): void {
  backdrop(c);
  const { w, h } = c;
  const horizon = h * between(c, 0.55, 0.66);
  glow(c, 'hz', w * between(c, 0.35, 0.65), horizon, w * 0.7, h * 0.35, 3, 0.8);
  let far = '';
  let near = '';
  const stars = Math.round((w * h) / 900);
  for (let k = 0; k < stars; k += 1) {
    const x = between(c, 0, w);
    const y = between(c, 14, horizon - 4);
    if (c.rand() < 0.75) {
      far += dot(x, y, 0.7);
    } else {
      near += dot(x, y, 1.3);
    }
  }
  c.body.push(`<path d="${far}" ${fill(4, 0.45)}/>`, `<path d="${near}" ${fill(4, 0.9)}/>`);
  c.body.push(`<rect y="${num(horizon)}" width="${w}" height="${num(h - horizon)}" ${fill(0, 0.55)}/>`);
  c.body.push(`<rect y="${num(horizon)}" width="${w}" height="1" ${fill(4, 0.7)}/>`);
  const pitch = pick(c, [12, 14, 16]);
  const phase = Math.floor(between(c, 0, 3));
  let on = '';
  let off = '';
  let halo = '';
  for (let k = 0; k * pitch < w; k += 1) {
    const x = k * pitch + pitch / 2;
    if ((k + phase) % 3 === 0) {
      on += dot(x, 7, 2.2);
      halo += dot(x, 7, 5.5);
    } else {
      off += dot(x, 7, 1.8);
    }
  }
  c.body.push(`<path d="${halo}" ${fill(5, 0.22)}/>`, `<path d="${off}" ${fill(2, 0.8)}/>`, `<path d="${on}" ${fill(5)}/>`);
}

/** Computers: rows of glyph-like blocks on a phosphor raster, with a cursor. */
function phosphor(c: Canvas): void {
  backdrop(c, 0, 0);
  const { w, h } = c;
  glow(c, 'tube', w * 0.5, h * 0.5, w * 0.7, h * 0.8, 2, 0.5);
  const line = 12;
  let dim = '';
  let bright = '';
  let cursor = '';
  for (let y = 12; y + line < h; y += line) {
    let x = 14 + Math.floor(between(c, 0, 3)) * 12;
    const end = c.rand() < 0.2 ? 0 : w * between(c, 0.2, 0.8);
    const hot = c.rand() < 0.2;
    while (x < end) {
      const len = Math.ceil(between(c, 1, 8));
      for (let k = 0; k < len; k += 1) {
        const gh = pick(c, [5, 6, 7, 7]);
        const glyph = square(x + k * 6, y + 7 - gh, 5, gh);
        if (hot) {
          bright += glyph;
        } else {
          dim += glyph;
        }
      }
      x += len * 6 + 6;
    }
    cursor = square(x, y, 5, 7);
  }
  c.body.push(`<path d="${dim}" ${fill(4, 0.3)}/>`, `<path d="${bright}" ${fill(4, 0.6)}/>`, `<path d="${cursor}" ${fill(4, 0.9)}/>`);
  scanlines(c, 0, 0.5);
}

/** Default: quiet contour lines drifting across the canvas. */
function contour(c: Canvas): void {
  backdrop(c);
  const { w, h } = c;
  const field = waves(c, 3);
  let d = '';
  for (let k = 0; k < 14; k += 1) {
    const base = (h * (k + 0.5)) / 13;
    d += `M0 ${num(base)}`;
    for (let x = 0; x <= w; x += 12) {
      d += `L${x} ${num(base + field(x * 0.012 + k * 0.18) * 18)}`;
    }
  }
  c.body.push(`<path d="${d}" ${stroke(3, 1.2, 0.55)}/>`);
}

const DRAW: Readonly<Record<ArtFamily, (c: Canvas) => void>> = {
  pixel,
  parallax,
  bands,
  vector,
  lcd,
  disc,
  poly,
  marquee,
  phosphor,
  contour
};

/**
 * Draws the backdrop for platform `id` from scratch; identical arguments give an
 * identical string. Colours are custom properties the host maps to the theme.
 */
export function renderArt(id: string, kind: PlatformKind | undefined, format: ArtFormat): Art {
  const family = familyFor(id, kind);
  const tone = TONES[family];
  const rand = prng(hash(`${family}:${id}`));
  const primary = tone.base + (rand() * 2 - 1) * tone.spread;
  const hues: Hues = {
    primary,
    secondary: primary + tone.second + (rand() * 2 - 1) * 10,
    tertiary: primary + tone.third
  };
  const [w, h] = SIZE[format];
  const canvas: Canvas = {
    w,
    h,
    rand: prng(hash(`${format}:${id}`)),
    uid: `pa-${id.replace(/[^a-zA-Z0-9_-]/g, '')}-${format}-`,
    defs: [],
    body: []
  };
  DRAW[family](canvas);
  const anchor = format === 'card' ? 'YMin' : 'YMid';
  const svg =
    `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${w} ${h}" preserveAspectRatio="xMid${anchor} slice" focusable="false" style="${slotVars(hues, tone.sat)}">` +
    `<defs>${canvas.defs.join('')}</defs>${canvas.body.join('')}</svg>`;
  return { family, svg };
}

const cache = new Map<string, Art>();

/** `renderArt`, memoised per id, kind and format. */
export function platformArt(id: string, kind: PlatformKind | undefined, format: ArtFormat): Art {
  const key = `${id}|${kind ?? ''}|${format}`;
  const hit = cache.get(key);
  if (hit) {
    return hit;
  }
  const art = renderArt(id, kind, format);
  cache.set(key, art);
  return art;
}
