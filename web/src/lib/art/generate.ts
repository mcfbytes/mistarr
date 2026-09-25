// Procedural platform backdrops; see docs/UI.md "Platform art".
import type { PlatformKind } from '../types';
import { hardware } from './hardware';
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

/**
 * Per id: its family, then an optional hue, saturation scale and secondary hue
 * offset. Siblings take compositions in this order; see `layout`. A Map, so ids
 * such as `constructor` or `__proto__` never reach Object.prototype.
 */
const PLATFORMS: ReadonlyMap<string, readonly [ArtFamily, number?, number?, number?]> = new Map(
  Object.entries({
    nes: ['pixel', 176],
    fds: ['pixel', 256],
    sms: ['pixel', HUE.warn, 1, -30],
    sg1000: ['pixel', 212],
    atari7800: ['pixel', 140],
    coleco: ['pixel', 330],
    intv: ['pixel', HUE.coral],
    pce: ['pixel', 292],
    sgx: ['pixel', 92, 0.9, -32],
    sv: ['pixel', 230, 0.4],
    snes: ['parallax', 205, 1, -40],
    megadrive: ['parallax', 30, 1, 40],
    s32x: ['parallax', HUE.violet, 1, 74],
    atari2600: ['bands', 200, 1, 70],
    atari5200: ['bands', HUE.teal, 1, -30],
    vectrex: ['vector'],
    gb: ['lcd', 168, 0.55, 30],
    ngp: ['lcd', 32, 0.4],
    gbc: ['lcd', HUE.rose],
    ws: ['lcd', 205, 0.5],
    pokemini: ['lcd', 300, 0.45],
    gg: ['lcd', 196],
    gba: ['lcd', HUE.violet],
    lynx: ['lcd', HUE.warn],
    wsc: ['lcd', HUE.coral],
    psx: ['disc', 240],
    saturn: ['disc', 192],
    megacd: ['disc', 318],
    pcecd: ['disc', 28],
    neocd: ['disc', 150],
    n64: ['poly'],
    arcade: ['marquee', HUE.rose],
    neogeo: ['marquee', 250, 1, 90]
  } satisfies Record<string, readonly [ArtFamily, number?, number?, number?]>)
);

/** Keyed by string: the server may send a kind this client does not know. */
const FAMILY_BY_KIND: ReadonlyMap<string, ArtFamily> = new Map([
  ['cartridge', 'pixel'],
  ['disc', 'disc'],
  ['computer', 'phosphor'],
  ['romset', 'marquee'],
  ['arcade', 'marquee'],
  ['other', 'contour']
]);

/** Per family: base hue, secondary and tertiary offsets, hue spread either way, saturation. */
const TONES: Readonly<Record<ArtFamily, readonly [number, number, number, number, number]>> = {
  pixel: [HUE.rose + 18, 30, 170, 22, 0.85],
  parallax: [HUE.violet, 74, 150, 30, 0.95],
  bands: [HUE.teal, -30, -133, 12, 1],
  vector: [HUE.accent - 16, -30, 120, 14, 0.9],
  lcd: [HUE.teal - 12, -30, 160, 28, 0.8],
  disc: [HUE.accent + 20, 60, 150, 34, 0.85],
  poly: [HUE.ok, 60, -110, 14, 0.7],
  marquee: [HUE.rose, 70, -130, 24, 1],
  phosphor: [HUE.ok - 10, 20, 180, 10, 0.9],
  contour: [HUE.accent, 46, 170, 20, 0.8]
};

/** Canvas size per format, in viewBox units. */
const SIZE: Readonly<Record<ArtFormat, readonly [number, number]>> = {
  card: [360, 160],
  wide: [960, 200]
};

/** The family an id draws with: its own mapping, else its kind's, else the default. */
export function familyFor(id: string, kind?: PlatformKind): ArtFamily {
  return PLATFORMS.get(id)?.[0] ?? FAMILY_BY_KIND.get(kind ?? '') ?? 'contour';
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
  `style="fill:none;stroke:var(--c${slot});stroke-width:${num(width)};stroke-opacity:${op(opacity)}"`;
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
  id: string;
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

/** A composition for the canvas: mapped siblings take turns through `shapes`, others pick one. */
function layout<T>(c: Canvas, shapes: readonly [T, ...T[]]): T {
  const family = PLATFORMS.get(c.id)?.[0];
  const turn = [...PLATFORMS]
    .filter(([, own]) => own[0] === family)
    .findIndex(([k]) => k === c.id);
  return turn < 0 ? pick(c, shapes) : (shapes[turn % shapes.length] ?? shapes[0]);
}

/** A focal x as a fraction of the width, visible at every banner aspect in use. */
function focusX(c: Canvas): number {
  return c.w > 400 ? between(c, 0.48, 0.52) : between(c, 0.32, 0.68);
}

/** `count` round stars in slot 4 across x 0 to `x1` and y `y0` to `y1`. */
function stars(c: Canvas, count: number, x1: number, y0: number, y1: number, opacity: number): void {
  let d = '';
  for (let k = 0; k < count; k += 1) {
    d += dot(between(c, 0, x1), between(c, y0, y1), between(c, 0.5, 1.3));
  }
  c.body.push(`<path d="${d}" ${fill(4, opacity)}/>`);
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
  const { w, h } = c;
  const cell = pick(c, [10, 12]);
  const cols = Math.ceil(w / cell);
  const rows = Math.ceil(h / cell);
  const aspect = h / w;
  const fx = focusX(c) - 0.5;
  const fy = (between(c, 0.25, 0.5) - 0.5) * aspect;
  const angle = between(c, 0.3, 0.9) * (c.rand() < 0.5 ? 1 : -1) + (c.rand() < 0.5 ? 0 : Math.PI);
  const corner = c.rand() < 0.5 ? -0.5 : 0.5;
  const freq = between(c, 9, 15);
  const phase = between(c, 0, 6.3);
  const shape = layout(c, ['sweep', 'burst', 'corner', 'wave', 'checker'] as const);
  const ramp = [2, 6, 3, 7];
  const levels = ramp.map(() => '');
  for (let j = 0; j < rows; j += 1) {
    for (let i = 0; i < cols; i += 1) {
      const x = (i + 0.5) / cols - 0.5;
      const y = ((j + 0.5) / rows - 0.5) * aspect;
      const d = Math.hypot(x - fx, y - fy);
      let v: number;
      switch (shape) {
        case 'sweep':
          v = 0.45 + (x * Math.cos(angle) + y * Math.sin(angle)) * 1.2;
          break;
        case 'burst':
          v = 1.05 - d * 2.6;
          break;
        case 'corner':
          v = 1 - Math.hypot(x - corner, y + aspect / 2) * 1.3;
          break;
        case 'wave':
          v = 0.5 + Math.sin(x * freq + phase) * 0.3 + Math.sin(y * freq * 2 - phase) * 0.18;
          break;
        case 'checker':
          v = (1 - d * 2) * ((Math.floor(i / 4) + Math.floor(j / 4)) % 2 ? 1 : 0.62);
      }
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
  const sx = w * focusX(c);
  const sy = h * between(c, 0.32, 0.44);
  const sr = h * between(c, 0.16, 0.32);
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
  stars(c, 18, w, 0, h * 0.4, 0.6);
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
  c.body.push(`<path d="${blocks}" ${fill(5)}/>`);
  scanlines(c, 0, 0.3);
}

/** Vector: a twisting wireframe tunnel with a soft glow on black. */
function vector(c: Canvas): void {
  backdrop(c, 0, 0);
  const { w, h } = c;
  const cx = w > 400 ? w * focusX(c) : w * between(c, 0.55, 0.72);
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
  stars(c, 14, w * 0.45, 0, h, 0.7);
}

/** Handhelds: abstract lit shapes on an LCD dot matrix, with ghosting and a backlight glow. */
function lcd(c: Canvas): void {
  backdrop(c, 1, 0);
  const { w, h } = c;
  const cell = pick(c, [5, 6]);
  const cols = Math.floor(w / cell);
  const rows = Math.floor(h / cell);
  const fx = Math.round(focusX(c) * cols);
  const fy = Math.round(rows * between(c, 0.3, 0.4));
  const r = Math.round(rows * (w > 400 ? between(c, 0.16, 0.2) : between(c, 0.2, 0.26)));
  const span = Math.round(cols * (w > 400 ? 0.12 : 0.32));
  const ridge = waves(c, 2);
  const shape = layout(c, ['bars', 'land', 'rings', 'glyphs', 'dots'] as const);
  c.defs.push(
    `<pattern id="${c.uid}dm" width="${cell}" height="${cell}" patternUnits="userSpaceOnUse"><rect x=".5" y=".5" width="${cell - 1}" height="${cell - 1}" ${fill(2, 0.3)}/></pattern>`
  );
  glow(c, 'bl', fx * cell, fy * cell, w * 0.5, h * 0.9, 3, 0.45);
  c.body.push(`<rect width="${w}" height="${h}" fill="url(#${c.uid}dm)"/>`);
  const on = (i: number, j: number): boolean => {
    const dx = i - fx;
    const dy = j - fy;
    switch (shape) {
      case 'bars': {
        const tall = Math.round((ridge(Math.floor(i / 3) * 0.9) * 0.5 + 0.5) * r * 2) + 1;
        return Math.abs(dx) <= span && i % 3 !== 2 && dy <= r && dy > r - tall;
      }
      case 'land': {
        const ground = r - Math.round((ridge(i * 0.07) * 0.5 + 0.5) * r * 1.4);
        const sun = Math.hypot(dx - span / 2, dy + r * 0.6) < r * 0.5;
        return sun || (dy >= ground && dy <= r + 3 && (dy === ground || (i + j) % 2 === 0));
      }
      case 'dots':
        return i % 2 === 0 && j % 2 === 0 && Math.abs(dx) + Math.abs(dy) * 2 < r * 3;
      case 'rings':
        return Math.hypot(dx, dy * 1.2) < r * 1.7 && Math.round(Math.hypot(dx, dy * 1.2)) % 3 === 0;
      case 'glyphs': {
        // Circle, square and diamond outlines: one radius under three norms.
        const k = Math.round(dx / (r * 2.2));
        const x = Math.abs(dx - k * r * 2.2);
        const y = Math.abs(dy);
        const norm = [Math.hypot(x, y), Math.max(x, y), (x + y) * 0.8][k + 1];
        return norm !== undefined && Math.abs(norm - r * 0.85) < 0.6;
      }
    }
  };
  let lit = '';
  let ghost = '';
  for (let j = 0; j < rows; j += 1) {
    for (let i = 0; i < cols; i += 1) {
      if (on(i, j)) {
        lit += square(i * cell + 0.5, j * cell + 0.5, cell - 1);
        ghost += square((i - 1) * cell + 0.5, j * cell + 0.5, cell - 1);
      }
    }
  }
  c.body.push(`<path d="${ghost}" ${fill(4, 0.18)}/>`, `<path d="${lit}" ${fill(4, 0.85)}/>`);
}

/** Discs: concentric tracks under a thin-film sheen and a sweep of light. */
function disc(c: Canvas): void {
  backdrop(c);
  const { w, h } = c;
  const cx = w > 400 ? w * focusX(c) : w * (pick(c, [0.3, 0.72, 0.78]) + between(c, -0.05, 0.05));
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

/** Arcade: a perspective grid running to a glowing horizon, under a chase of marquee bulbs. */
function marquee(c: Canvas): void {
  backdrop(c);
  const { w, h } = c;
  const horizon = h * (w > 400 ? between(c, 0.44, 0.52) : between(c, 0.3, 0.36));
  const vx = w * focusX(c);
  glow(c, 'hz', vx, horizon, w * 0.4, h * 0.45, 3, 0.9);
  stars(c, w / 12, w, 16, horizon - 6, 0.6);
  c.body.push(`<rect y="${num(horizon)}" width="${w}" height="${num(h - horizon)}" ${fill(0, 0.6)}/>`);
  let grid = `M0 ${num(horizon)}H${w}`;
  const pitch = between(c, 34, 46);
  for (let k = -Math.ceil(w / pitch); k <= w / pitch; k += 1) {
    grid += `M${num(vx + k * pitch * 0.1)} ${num(horizon)}L${num(vx + k * pitch * 1.4)} ${h}`;
  }
  for (let t = 1; t < 8; t += 1) {
    grid += `M0 ${num(horizon + (h - horizon) * (t / 7) ** 2)}H${w}`;
  }
  c.body.push(`<path d="${grid}" ${stroke(3, 3, 0.25)}/>`, `<path d="${grid}" ${stroke(4, 0.8, 0.75)}/>`);
  const gap = pick(c, [9, 10, 11]);
  const phase = Math.floor(between(c, 0, 2));
  let lit = '';
  let off = '';
  let halo = '';
  for (let k = 0; k * gap < w; k += 1) {
    const x = k * gap + gap / 2;
    if ((k + phase) % 2 === 0) {
      lit += dot(x, 7, 2.1);
      halo += dot(x, 7, 5);
    } else {
      off += dot(x, 7, 1.6);
    }
  }
  c.body.push(`<path d="${halo}" ${fill(5, 0.25)}/>`, `<path d="${off}" ${fill(2, 0.8)}/>`, `<path d="${lit}" ${fill(5)}/>`);
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

/**
 * Draws the platform's hardware centred on the canvas, standing on a soft
 * shadow in front of a glow, scaled to fit the format's box.
 */
function stage(c: Canvas, kind: PlatformKind | undefined): void {
  const { w } = c;
  const card = w < 400;
  const [cw, ch, markup] = hardware(c.id, kind);
  const k = Math.min((card ? 78 : 84) / ch, (card ? 190 : 240) / cw);
  const ground = card ? 106 : 130;
  const x = w / 2;
  glow(c, 'back', x, ground - (ch * k) / 2, cw * k * 0.9, ch * k * 0.9, 3, 0.4);
  glow(c, 'floor', x, ground + 1, cw * k * 0.7, 7, 8, 0.8);
  c.body.push(
    `<g class="hw" transform="translate(${num(x - (cw * k) / 2)} ${num(ground - ch * k)}) scale(${op(k)})" style="fill:var(--c9);stroke:var(--c4);stroke-width:${op(1.4 / k)};stroke-linejoin:round">${markup}</g>`
  );
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
  const [base, second, third, spread, sat] = TONES[family];
  const rand = prng(hash(`${family}:${id}`));
  const [, hue, idSat = 1, idSecond = second] = PLATFORMS.get(id) ?? [];
  const primary = (hue ?? base) + (rand() * 2 - 1) * (hue === undefined ? spread : 6);
  const hues: Hues = {
    primary,
    secondary: primary + idSecond + (rand() * 2 - 1) * 10,
    tertiary: primary + third
  };
  const [w, h] = SIZE[format];
  const canvas: Canvas = {
    w,
    h,
    rand: prng(hash(`${format}:${id}`)),
    id,
    uid: `pa-${id.replace(/[^a-zA-Z0-9_-]/g, '')}-${format}-`,
    defs: [],
    body: []
  };
  DRAW[family](canvas);
  stage(canvas, kind);
  const anchor = format === 'card' ? 'YMin' : 'YMid';
  const svg =
    `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${w} ${h}" preserveAspectRatio="xMid${anchor} slice" focusable="false" style="${slotVars(hues, sat * idSat)}">` +
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
