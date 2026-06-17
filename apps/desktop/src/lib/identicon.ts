// Bit stream derived from a string seed (hex pubkey or any free-form name).
// Used by the spiral identicon to decide which dots are filled.
//
// Strategy:
//   - If `seed` parses as hex bytes, walk those bytes — each byte yields 8 bits
//     and we wrap around if we need more.
//   - Otherwise (e.g. a peer name) hash the string with FNV-1a and expand it
//     with a fast LCG so we get a deterministic, uniform-looking bit stream.

export function seedBits(seed: string, count: number): boolean[] {
  const out = new Array<boolean>(count);
  const clean = seed.replace(/^0x/, "");
  const bytes: number[] = [];
  if (/^[0-9a-fA-F]+$/.test(clean) && clean.length >= 8 && clean.length % 2 === 0) {
    for (let i = 0; i < clean.length; i += 2) {
      bytes.push(parseInt(clean.substr(i, 2), 16));
    }
  }
  if (bytes.length > 0) {
    for (let i = 0; i < count; i++) {
      const byte = bytes[i % bytes.length];
      const bit = (i / bytes.length) | 0;
      out[i] = ((byte >> (bit % 8)) & 1) === 1;
    }
    return out;
  }
  // Free-form seed → FNV-1a + LCG expansion.
  let h = 2166136261 >>> 0;
  for (let i = 0; i < seed.length; i++) {
    h ^= seed.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  for (let i = 0; i < count; i++) {
    h = (Math.imul(h, 1103515245) + 12345) >>> 0;
    out[i] = ((h >> 16) & 1) === 1;
  }
  return out;
}

// ── deterministic colour for the squircle avatars ───────────────────────────
// A 32-bit hash of the seed feeds a small PRNG; from it we pick a base hue and
// a harmonious scheme, then build a 2–3 colour palette + a muted background.
// Same key → same palette, always.

function hash32(seed: string): number {
  const clean = seed.replace(/^0x/, "");
  // Prefer raw key entropy when the seed is hex; otherwise hash the string.
  const src = /^[0-9a-fA-F]{8,}$/.test(clean) ? clean : seed;
  let h = 2166136261 >>> 0;
  for (let i = 0; i < src.length; i++) {
    h ^= src.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return h >>> 0;
}

function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

export interface Palette {
  /// Muted dark tone derived from the base hue — fills the squircle.
  bg: string;
  /// Light tone for thin separators / outlines on top of `bg`.
  ink: string;
  /// 2–3 vivid harmonious colours that draw the actual sigil.
  colors: string[];
}

// Hue offsets (degrees) for a few classic harmonious schemes.
const SCHEMES = [
  [0, 150, 210], // split-complementary
  [0, 120, 240], // triad
  [0, 40, 200], // analogous + accent
  [0, 180, 60], // complementary + accent
];

export function seedPalette(seed: string): Palette {
  const rnd = mulberry32(hash32(seed) ^ 0x9e3779b9);
  const base = Math.floor(rnd() * 360);
  const scheme = SCHEMES[Math.floor(rnd() * SCHEMES.length)];
  const sat = 62 + Math.floor(rnd() * 18); // 62–80%
  const light = 56 + Math.floor(rnd() * 10); // 56–66%
  const colors = scheme.map(
    (d) => `hsl(${(base + d) % 360} ${sat}% ${light}%)`,
  );
  return {
    bg: `hsl(${base} ${Math.round(sat * 0.45)}% 15%)`,
    ink: `hsl(${base} 30% 90%)`,
    colors,
  };
}

/// Stable per-cell choices for the Truchet weave: each cell gets an orientation
/// (0/1) and a colour index, derived deterministically from the seed.
export function seedCells(
  seed: string,
  count: number,
): { flip: boolean; color: number }[] {
  const rnd = mulberry32(hash32(seed));
  const out: { flip: boolean; color: number }[] = new Array(count);
  for (let i = 0; i < count; i++) {
    out[i] = { flip: rnd() < 0.5, color: Math.floor(rnd() * 3) };
  }
  return out;
}
