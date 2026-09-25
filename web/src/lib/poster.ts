// Per-title variation of the generated poster; see docs/UI.md "Browse".

/** How one generated poster's art band differs from its platform's art. */
export interface PosterVariation {
  /** Hue rotation in degrees, from -30 to 30. */
  hue: number;
  /** Horizontal shift of the art in percent of the band, from -4 to 4. */
  shift: number;
  /** Zoom of the art, from 1.08 to 1.14: enough that the shift never bares an edge. */
  scale: number;
}

/** FNV-1a over the UTF-16 code units of `text`. */
function fnv1a(text: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < text.length; i += 1) {
    h ^= text.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return h >>> 0;
}

/** The variation for a title name; the same name always gets the same one. */
export function posterVariation(name: string): PosterVariation {
  const h = fnv1a(name);
  return {
    hue: (h % 61) - 30,
    shift: ((h >>> 8) % 9) - 4,
    scale: 1.08 + ((h >>> 16) % 7) / 100
  };
}
