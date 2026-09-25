// Stylised hardware drawn over platform art; see docs/UI.md "Platform art".
import type { PlatformKind } from '../types';

/** A console drawing: its width and height in local units, and its SVG markup. */
export type Hardware = readonly [width: number, height: number, markup: string];

// Parts inherit the body fill and rim stroke from the group; these override them.
const FACE = ' style="fill:var(--c10);stroke-opacity:.55"';
const HOLE = ' style="fill:var(--c8);stroke-opacity:.35"';
const KEY = ' style="fill:var(--c4);stroke:none"';
const GLASS = ' style="fill:var(--c3);stroke:none"';
const LINE = ' style="fill:none;stroke-opacity:.5"';

const n = (v: number): string => String(Math.round(v * 10) / 10);
const rect = (x: number, y: number, w: number, h: number, rx = 0, s = ''): string =>
  `<rect x="${n(x)}" y="${n(y)}" width="${n(w)}" height="${n(h)}"${rx ? ` rx="${rx}"` : ''}${s}/>`;
const circle = (x: number, y: number, r: number, s = ''): string =>
  `<circle cx="${n(x)}" cy="${n(y)}" r="${n(r)}"${s}/>`;
const path = (d: string, s = ''): string => `<path d="${d}"${s}/>`;
const slot = (x: number, y: number, w: number, h: number): string => rect(x, y, w, h, 1, HOLE);

/** A D-pad of span `s` centred at (x, y). */
function dpad(x: number, y: number, s: number): string {
  const a = n(s / 3);
  const b = n(s / 3);
  return path(`M${n(x - s / 6)} ${n(y - s / 2)}h${a}v${b}h${a}v${a}h-${b}v${b}h-${a}v-${b}h-${a}v-${a}h${b}z`, KEY);
}

/** Round keys of radius `r` at each centre, as one node. */
function keys(r: number, ...at: (readonly [number, number])[]): string {
  const d = at.map(([x, y]) => `M${n(x - r)} ${n(y)}a${r} ${r} 0 1 0 ${n(2 * r)} 0a${r} ${r} 0 1 0 ${n(-2 * r)} 0`);
  return path(d.join(''), KEY);
}

/** `count` horizontal lines `gap` apart: vents, ribs or a grille. */
function vents(x: number, y: number, w: number, count: number, gap: number): string {
  let d = '';
  for (let i = 0; i < count; i += 1) {
    d += `M${n(x)} ${n(y + i * gap)}h${n(w)}`;
  }
  return path(d, LINE);
}

/** A bezel with a glowing screen inset by `m`. */
const screen = (x: number, y: number, w: number, h: number, m = 3): string =>
  rect(x, y, w, h, 2, FACE) + rect(x + m, y + m, w - 2 * m, h - 2 * m, 1, GLASS);

/** A round disc lid, its track ring and its hub. */
const lid = (x: number, y: number, r: number): string =>
  circle(x, y, r, FACE) + circle(x, y, r * 0.62, LINE) + circle(x, y, r * 0.2, HOLE);

/** A grid of small square keys: keypads and keyboards. */
function grid(x: number, y: number, cols: number, rows: number, s: number, gap: number): string {
  let d = '';
  for (let j = 0; j < rows; j += 1) {
    for (let i = 0; i < cols; i += 1) {
      d += `M${n(x + i * (s + gap))} ${n(y + j * (s + gap))}h${s}v${s}h-${s}z`;
    }
  }
  return path(d, ' style="fill:var(--c4);fill-opacity:.7;stroke:none"');
}

/** A domed cartridge console; `mushroom` adds a stacked add-on in its slot. */
function dome(mushroom: boolean): Hardware {
  const top = mushroom ? 12 : 0;
  const addOn = mushroom ? path('M38 16L30 2H70L62 16Z', FACE) + vents(36, 6, 28, 2, 4) : '';
  return [
    100,
    34 + top,
    addOn +
      rect(0, 10 + top, 100, 24, 10) +
      circle(50, 16 + top, 17, FACE) +
      slot(38, 8 + top, 24, 4) +
      rect(8, 18 + top, 12, 4, 1, KEY) +
      keys(2, [26, 20 + top]) +
      vents(72, 18 + top, 22, 3, 4)
  ];
}

/** A landscape handheld with a screen, D-pad and face keys. */
function slab(w: number, h: number, rx: number, sw: number): Hardware {
  const sx = (w - sw) / 2;
  return [
    w,
    h,
    rect(0, 0, w, h, rx) +
      screen(sx, 6, sw, h - 18) +
      dpad(sx / 2, h / 2, 14) +
      keys(4, [w - sx / 2 - 5, h / 2 + 4], [w - sx / 2 + 5, h / 2 - 4]) +
      keys(1.6, [w / 2 - 5, h - 6], [w / 2 + 5, h - 6])
  ];
}

/** A handheld with a side screen and two diamonds of small keys. */
function swan(sw: number, rx: number): Hardware {
  return [
    100,
    54,
    rect(0, 0, 100, 54, rx) +
      screen(8, 8, sw, 38) +
      keys(2.6, [sw + 20, 18], [sw + 26, 24], [sw + 20, 30], [sw + 14, 24]) +
      keys(3.4, [sw + 22, 42], [sw + 30, 36])
  ];
}

/** A top-down disc console: body, lid, keys and front ports. */
function tray(w: number, h: number, rx: number, lx: number, extra: string): Hardware {
  return [w, h, rect(0, 0, w, h, rx) + lid(lx, h * 0.44, h * 0.34) + slot(10, h - 8, 14, 5) + slot(30, h - 8, 14, 5) + extra];
}

/** A portrait handheld; `classic` gives the rounded corner and angled grille. */
function pocket(classic: boolean): Hardware {
  const [w, h] = classic ? [56, 92] : [52, 84];
  const body = classic
    ? path('M0 4Q0 0 4 0H52Q56 0 56 4V74Q56 92 38 92H4Q0 92 0 88Z') + path('M40 86l8-8M44 88l8-8M48 90l6-6', LINE)
    : rect(0, 0, w, h, 7);
  return [
    w,
    h,
    body +
      screen(6, 6, w - 12, h * 0.42, 5) +
      dpad(14, h * 0.66, 14) +
      keys(4, [w - 16, h * 0.68], [w - 8, h * 0.6]) +
      rect(w / 2 - 11, h - 12, 8, 3, 1.5, KEY) +
      rect(w / 2 + 1, h - 12, 8, 3, 1.5, KEY)
  ];
}

const BOX: Hardware = [
  90,
  34,
  rect(0, 4, 90, 30, 4) + rect(20, 0, 50, 8, 2, FACE) + slot(26, 2, 38, 3) + keys(2.4, [12, 24], [22, 24]) + vents(40, 20, 40, 3, 4)
];

const KEYBOARD: Hardware = [
  110,
  40,
  path('M0 40V16L8 4H102L110 16V40Z') + grid(13, 9, 14, 3, 5, 1.6) + rect(34, 30, 40, 4, 1, KEY) + keys(1.4, [100, 35])
];

const CABINET: Hardware = [
  56,
  100,
  rect(3, 0, 50, 100, 2) +
    rect(5, 2, 46, 12, 1, ' style="fill:var(--c5);stroke:none"') +
    screen(8, 18, 40, 32) +
    rect(0, 56, 56, 10, 1, FACE) +
    path('M15 60V49', ' style="stroke-width:2.5"') +
    keys(3, [15, 48]) +
    keys(2.4, [29, 61], [37, 61], [45, 61]) +
    rect(18, 74, 20, 18, 1, FACE) +
    slot(23, 78, 2, 7) +
    slot(31, 78, 2, 7)
];

/** Nested twisting squares: the vector tunnel on a portrait screen. */
function tunnel(x: number, y: number): string {
  let d = '';
  let r = 20;
  for (let k = 0; k < 6; k += 1) {
    const pts = [0, 1, 2, 3].map((q) => {
      const a = q * 1.571 + k * 0.28 + 0.785;
      return `${n(x + Math.cos(a) * r)} ${n(y + Math.sin(a) * r)}`;
    });
    d += `M${pts.join('L')}z`;
    r *= 0.7;
  }
  return path(d, ' style="fill:none;stroke-opacity:.9"');
}

const HARDWARE: Readonly<Record<string, Hardware>> = {
  nes: [
    100,
    36,
    rect(0, 0, 100, 36, 2) +
      rect(34, 4, 62, 14, 1, FACE) +
      vents(34, 23, 62, 3, 4) +
      path('M30 3V33', LINE) +
      rect(8, 6, 9, 4, 1, KEY) +
      rect(19, 6, 9, 4, 1, KEY) +
      slot(8, 24, 9, 6) +
      slot(19, 24, 9, 6)
  ],
  fds: [
    90,
    40,
    rect(22, 0, 46, 22, 1, FACE) +
      slot(38, 3, 14, 8) +
      rect(0, 14, 90, 26, 3) +
      slot(18, 12, 54, 4) +
      rect(74, 30, 10, 4, 1, KEY) +
      keys(1.6, [10, 32])
  ],
  sms: [
    100,
    34,
    path('M0 34V18L16 6H100V34Z') + rect(0, 20, 100, 5, 0, FACE) + slot(44, 9, 40, 4) + slot(56, 28, 26, 3) + keys(2.2, [10, 29], [18, 29])
  ],
  sg1000: [90, 34, path('M0 34V14H26V4H90V34Z') + slot(40, 7, 40, 4) + vents(6, 20, 78, 3, 4) + keys(2.2, [8, 9], [16, 9])],
  atari7800: [
    100,
    30,
    path('M0 30V16L12 4H88L100 16V30Z') +
      slot(34, 7, 32, 4) +
      rect(0, 17, 100, 4, 0, FACE) +
      rect(14, 24, 8, 3, 1, KEY) +
      rect(26, 24, 8, 3, 1, KEY) +
      slot(70, 23, 8, 5) +
      slot(82, 23, 8, 5)
  ],
  coleco: [
    100,
    44,
    rect(0, 0, 100, 44, 3) +
      rect(6, 6, 22, 32, 2, FACE) +
      grid(10, 16, 3, 4, 3.5, 1.5) +
      keys(3, [17, 11], [43, 11]) +
      rect(32, 6, 22, 32, 2, FACE) +
      grid(36, 16, 3, 4, 3.5, 1.5) +
      slot(62, 6, 32, 5) +
      vents(62, 18, 32, 5, 4)
  ],
  intv: [
    110,
    40,
    rect(0, 0, 110, 40, 3) +
      rect(6, 5, 20, 30, 2, FACE) +
      grid(9.5, 8, 3, 4, 3, 1.2) +
      rect(30, 5, 20, 30, 2, FACE) +
      grid(33.5, 8, 3, 4, 3, 1.2) +
      circle(16, 29, 4, LINE) +
      circle(40, 29, 4, LINE) +
      vents(58, 8, 44, 7, 3.6)
  ],
  pce: [60, 40, rect(0, 4, 60, 36, 3) + rect(6, 0, 48, 8, 2, FACE) + slot(14, 16, 32, 3) + vents(8, 26, 44, 3, 3.5)],
  sgx: [90, 44, path('M0 44V14L14 2H76L90 14V44Z') + slot(24, 18, 42, 3) + vents(8, 28, 74, 3, 4) + rect(66, 6, 10, 4, 1, KEY)],
  sv: [
    60,
    76,
    rect(0, 0, 60, 76, 8) + screen(8, 6, 44, 34) + path('M2 45H58', LINE) + dpad(16, 60, 14) + keys(3.4, [42, 62], [50, 55])
  ],
  snes: [
    100,
    38,
    rect(0, 8, 100, 30, 8) +
      rect(28, 0, 44, 16, 5, FACE) +
      slot(36, 4, 28, 4) +
      rect(8, 15, 12, 5, 2, KEY) +
      rect(80, 15, 12, 5, 2, KEY) +
      keys(2.6, [18, 30]) +
      slot(34, 28, 9, 6) +
      slot(57, 28, 9, 6)
  ],
  megadrive: dome(false),
  s32x: dome(true),
  n64: [
    100,
    40,
    path('M0 40V24Q0 16 8 16H28L34 4H66L72 16H92Q100 16 100 24V40Z') +
      slot(40, 8, 20, 4) +
      slot(14, 30, 10, 6) +
      slot(34, 30, 10, 6) +
      slot(56, 30, 10, 6) +
      slot(76, 30, 10, 6)
  ],
  atari2600: [
    100,
    36,
    rect(0, 20, 100, 16, 2) +
      rect(18, 6, 64, 16, 2, FACE) +
      slot(34, 9, 32, 4) +
      vents(22, 16, 56, 2, 3) +
      rect(6, 25, 5, 7, 1, KEY) +
      rect(15, 25, 5, 7, 1, KEY) +
      rect(80, 25, 5, 7, 1, KEY) +
      rect(89, 25, 5, 7, 1, KEY)
  ],
  atari5200: [
    100,
    40,
    path('M0 40V20L22 4H100V40Z') + slot(40, 8, 52, 6) + rect(0, 24, 100, 4, 0, FACE) + vents(30, 32, 64, 2, 3.5) + keys(2.2, [10, 33])
  ],
  vectrex: [
    64,
    100,
    rect(0, 0, 64, 86, 4) +
      rect(6, 6, 52, 62, 2, ' style="fill:#000;fill-opacity:.75;stroke-opacity:.5"') +
      tunnel(32, 37) +
      vents(12, 74, 40, 3, 3.5) +
      rect(8, 88, 48, 12, 2, FACE) +
      keys(3, [17, 94]) +
      keys(1.8, [32, 94], [38, 94], [44, 94], [50, 94])
  ],
  gb: pocket(true),
  gbc: pocket(false),
  gba: slab(104, 56, 26, 44),
  gg: slab(112, 58, 12, 50),
  lynx: slab(136, 52, 11, 60),
  ngp: [
    96,
    60,
    rect(0, 0, 96, 60, 22) + screen(28, 6, 42, 40) + circle(15, 30, 8, FACE) + keys(4, [15, 30]) + keys(4, [80, 34], [88, 26])
  ],
  ws: swan(56, 8),
  wsc: swan(60, 12),
  pokemini: [
    62,
    66,
    rect(0, 0, 62, 66, 16) + screen(10, 6, 42, 30) + dpad(17, 50, 12) + keys(5, [44, 47]) + keys(2.6, [36, 58], [52, 58])
  ],
  psx: tray(100, 72, 5, 62, keys(3.5, [18, 14], [18, 26]) + keys(5, [18, 42])),
  saturn: tray(104, 68, 12, 48, slot(24, 3, 56, 4) + keys(3, [90, 22], [90, 32], [90, 42])),
  megacd: tray(112, 60, 6, 78, circle(28, 26, 15, FACE) + slot(18, 23, 20, 5)),
  pcecd: tray(80, 70, 4, 40, slot(24, 66, 32, 2) + keys(2.6, [70, 10])),
  neocd: tray(106, 64, 6, 36, rect(70, 8, 28, 44, 3, FACE) + keys(3.5, [84, 18], [84, 30], [84, 42])),
  arcade: CABINET,
  neogeo: [
    120,
    48,
    rect(0, 18, 78, 30, 3) +
      rect(10, 12, 58, 8, 2, FACE) +
      slot(14, 14, 50, 3) +
      vents(8, 30, 62, 3, 4) +
      rect(84, 30, 36, 18, 3, FACE) +
      path('M94 30V12', ' style="stroke-width:3"') +
      keys(5, [94, 10]) +
      keys(2.6, [102, 40], [109, 40], [116, 40])
  ]
};

/** The drawing for platform `id`: its own, else a generic form for its kind. */
export function hardware(id: string, kind?: PlatformKind): Hardware {
  const own = HARDWARE[id];
  if (own) {
    return own;
  }
  if (kind === 'computer') {
    return KEYBOARD;
  }
  return kind === 'arcade' || kind === 'romset' ? CABINET : BOX;
}
