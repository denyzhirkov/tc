// A face for every name, drawn from the name alone — the same way its colour
// is (`lib/tint.ts`), so two clients draw the same person the same way and
// nobody picks one. Line art in the identity hue, a whisper of fill: these
// stand above typeset names and must read as part of the same page, not as
// stickers on it.
//
// Pure on purpose — no Solid, no DOM — so node can test it with type
// stripping alone (no enums, namespaces or parameter properties here). The
// geometry is computed once per face; per frame only `mouthPath` builds a
// string, and the rest of the animation is numbers.
//
// Coordinates are a 100×100 viewBox. Every path uses only absolute M/L/C/Q/Z,
// so a test can read the numbers in it as points and check the bounds.

export type HeadKind = "round" | "squircle" | "egg" | "wide" | "tall";
export type EyeKind = "dot" | "oval" | "arc" | "line" | "ring";
export type BrowKind = "none" | "straight" | "arched" | "slanted";
export type HairKind = "none" | "fringe" | "side" | "curls" | "spikes" | "bun" | "cap" | "long";
export type Accessory = "none" | "glasses" | "freckles" | "ears";
export type MouthKind = "line" | "smile" | "small";

export const HEAD_KINDS: HeadKind[] = ["round", "squircle", "egg", "wide", "tall"];
export const EYE_KINDS: EyeKind[] = ["dot", "oval", "arc", "line", "ring"];
export const BROW_KINDS: BrowKind[] = ["none", "straight", "arched", "slanted"];
export const HAIR_KINDS: HairKind[] = ["none", "fringe", "side", "curls", "spikes", "bun", "cap", "long"];
export const ACCESSORIES: Accessory[] = ["none", "glasses", "freckles", "ears"];
export const MOUTH_KINDS: MouthKind[] = ["line", "smile", "small"];

/// How a part is painted (see `.face-*` in styles.css): `skin` is the head's
/// outline over a near-invisible wash, `hair` a stroke over a heavier wash,
/// `ink` a solid fill with no stroke, `line` a stroke alone.
export type Ink = "skin" | "hair" | "ink" | "line";
export type Part = { d: string; ink: Ink };
export type Pt = { x: number; y: number };

export type HeadBox = {
  cx: number;
  cy: number;
  rx: number;
  ry: number;
  /// Superellipse exponent: 2 is an ellipse, higher squares the corners.
  n: number;
  /// Narrowing toward the chin, as a share of the width.
  taper: number;
};

export type Face = {
  head: HeadKind;
  eyes: EyeKind;
  brows: BrowKind;
  hair: HairKind;
  accessory: Accessory;
  mouth: MouthKind;
  box: HeadBox;
  /// Drawn first, in this order: ears, head, hair.
  back: Part[];
  /// The two eye centres; `eye` is drawn around each, in local coordinates,
  /// so a blink is one scale about the origin.
  eyeAt: [Pt, Pt];
  eye: Part[];
  /// Absolute; raised as a group on a loud syllable.
  brow: Part[];
  /// Mouth centre, its closed width and how far it may open.
  mouthAt: Pt;
  mouthWidth: number;
  mouthOpen: number;
  /// Over the eyes: glasses, freckles.
  front: Part[];
  /// Per-face phase for the head's sway, so a room does not sway in step.
  phase: number;
  /// Seed for this face's blink schedule.
  blinkSeed: number;
};

// ── determinism ─────────────────────────────────────────────────────────

/// FNV-1a, salted so a face is not correlated with the hue `tint.ts` takes
/// from the same name.
function hash(seed: string): number {
  let h = 2166136261;
  const s = `face:${seed}`;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return h >>> 0;
}

/// mulberry32: a small seeded generator, uniform in [0, 1).
export function rng(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function pick<T>(r: () => number, list: readonly T[], weights?: readonly number[]): T {
  if (!weights) return list[Math.floor(r() * list.length)];
  const total = weights.reduce((a, b) => a + b, 0);
  let x = r() * total;
  for (let i = 0; i < list.length; i++) {
    x -= weights[i];
    if (x < 0) return list[i];
  }
  return list[list.length - 1];
}

const between = (r: () => number, lo: number, hi: number) => lo + r() * (hi - lo);

// ── geometry helpers ────────────────────────────────────────────────────

const f = (v: number) => (Math.round(v * 100) / 100).toString();
const pt = (p: Pt) => `${f(p.x)} ${f(p.y)}`;

/// A point on the head's outline at angle `th` (0 = right, π/2 = chin in
/// SVG's y-down), `k` times its size.
export function contour(b: HeadBox, th: number, k = 1): Pt {
  const c = Math.cos(th);
  const s = Math.sin(th);
  const e = 2 / b.n;
  const px = Math.sign(c) * Math.abs(c) ** e;
  const py = Math.sign(s) * Math.abs(s) ** e;
  return { x: b.cx + b.rx * k * px * (1 - b.taper * py), y: b.cy + b.ry * k * py };
}

/// Points along the outline from `a` to `b` (radians), inclusive.
function arc(box: HeadBox, a: number, b: number, k: number, steps: number): Pt[] {
  const out: Pt[] = [];
  for (let i = 0; i <= steps; i++) out.push(contour(box, a + ((b - a) * i) / steps, k));
  return out;
}

const poly = (ps: Pt[], move = true) =>
  ps.map((p, i) => `${i === 0 && move ? "M" : "L"}${pt(p)}`).join("");

/// An ellipse as four cubics (no arc commands, so the bounds stay readable).
function oval(cx: number, cy: number, rx: number, ry = rx): string {
  const k = 0.5523;
  return (
    `M${f(cx + rx)} ${f(cy)}` +
    `C${f(cx + rx)} ${f(cy + k * ry)} ${f(cx + k * rx)} ${f(cy + ry)} ${f(cx)} ${f(cy + ry)}` +
    `C${f(cx - k * rx)} ${f(cy + ry)} ${f(cx - rx)} ${f(cy + k * ry)} ${f(cx - rx)} ${f(cy)}` +
    `C${f(cx - rx)} ${f(cy - k * ry)} ${f(cx - k * rx)} ${f(cy - ry)} ${f(cx)} ${f(cy - ry)}` +
    `C${f(cx + k * rx)} ${f(cy - ry)} ${f(cx + rx)} ${f(cy - k * ry)} ${f(cx + rx)} ${f(cy)}Z`
  );
}

const PI = Math.PI;

// ── the face ────────────────────────────────────────────────────────────

const BOXES: Record<HeadKind, Omit<HeadBox, "cx" | "cy">> = {
  round: { rx: 29, ry: 30, n: 2, taper: 0 },
  squircle: { rx: 28, ry: 29, n: 3.4, taper: 0 },
  egg: { rx: 28, ry: 32, n: 2, taper: 0.15 },
  wide: { rx: 33, ry: 27, n: 2.3, taper: 0.05 },
  tall: { rx: 24, ry: 33, n: 2.4, taper: 0.04 },
};

function hairOf(kind: HairKind, b: HeadBox, r: () => number): Part[] {
  const { cx, cy, rx, ry } = b;
  const top = PI * 1.5;
  switch (kind) {
    case "none":
      return [];
    case "fringe": {
      const A = contour(b, PI + 0.14);
      const B = contour(b, 2 * PI - 0.14);
      const low = cy - ry * between(r, 0.44, 0.52);
      const d =
        `M${pt(A)}` +
        poly(arc(b, PI + 0.14, 2 * PI - 0.14, 1.07, 20), false) +
        `L${pt(B)}` +
        `Q${f(cx + rx * 0.8)} ${f(cy - ry * 0.3)} ${f(cx + rx * 0.35)} ${f(low)}` +
        `Q${f(cx + rx * 0.05)} ${f(low + ry * 0.1)} ${f(cx - rx * 0.2)} ${f(low - ry * 0.02)}` +
        `Q${f(cx - rx * 0.75)} ${f(cy - ry * 0.32)} ${pt(A)}Z`;
      return [{ d, ink: "hair" }];
    }
    case "side": {
      // The part falls to one side, chosen per face.
      const s = r() < 0.5 ? 1 : -1;
      const A = contour(b, s > 0 ? PI + 0.36 : 2 * PI - 0.36);
      const B = contour(b, s > 0 ? 2 * PI - 0.22 : PI + 0.22);
      const part = { x: cx - s * rx * 0.42, y: cy - ry * 0.64 };
      const crown = contour(b, top - s * 0.42, 1.07);
      const outer = s > 0 ? arc(b, PI + 0.36, 2 * PI - 0.22, 1.08, 20) : arc(b, 2 * PI - 0.36, PI + 0.22, 1.08, 20);
      const d =
        `M${pt(A)}` +
        poly(outer, false) +
        `L${pt(B)}` +
        `Q${f(cx + s * rx * 0.1)} ${f(cy - ry * 0.32)} ${pt(part)}` +
        `L${pt(A)}Z` +
        `M${pt(part)}L${pt(crown)}`;
      return [{ d, ink: "hair" }];
    }
    case "curls": {
      const n = 7 + Math.floor(r() * 3);
      const a = PI - 0.12;
      const z = 2 * PI + 0.12;
      let d = `M${pt(contour(b, a, 1.01))}`;
      for (let i = 0; i < n; i++) {
        const t0 = a + ((z - a) * i) / n;
        const t1 = a + ((z - a) * (i + 1)) / n;
        const c = contour(b, (t0 + t1) / 2, 1.17);
        d += `Q${pt(c)} ${pt(contour(b, t1, 1.01))}`;
      }
      d += poly(arc(b, z, a, 0.99, 20), false) + "Z";
      return [{ d, ink: "hair" }];
    }
    case "spikes": {
      const n = 6 + Math.floor(r() * 3);
      const a = PI + 0.2;
      const z = 2 * PI - 0.2;
      let d = `M${pt(contour(b, a))}`;
      for (let i = 0; i < n; i++) {
        const t0 = a + ((z - a) * i) / n;
        const t1 = a + ((z - a) * (i + 1)) / n;
        d += `L${pt(contour(b, (t0 + t1) / 2, between(r, 1.18, 1.27)))}L${pt(contour(b, t1, 1.02))}`;
      }
      d += `Q${f(cx)} ${f(cy - ry * 0.8)} ${pt(contour(b, a))}Z`;
      return [{ d, ink: "hair" }];
    }
    case "bun": {
      const A = contour(b, PI + 0.26);
      const B = contour(b, 2 * PI - 0.26);
      const cap =
        `M${pt(A)}` +
        poly(arc(b, PI + 0.26, 2 * PI - 0.26, 1.05, 20), false) +
        `L${pt(B)}Q${f(cx)} ${f(cy - ry * 0.78)} ${pt(A)}Z`;
      const rb = between(r, 6.5, 8.5);
      const crown = contour(b, top, 1.05);
      const bx = cx + (r() < 0.6 ? 0 : (r() < 0.5 ? -1 : 1) * rx * 0.32);
      const by = crown.y - rb * 0.62 + (bx === cx ? 0 : ry * 0.08);
      return [
        { d: oval(bx, by, rb), ink: "hair" },
        { d: cap, ink: "hair" },
      ];
    }
    case "cap": {
      // A knitted cap: a dome over a band across the forehead.
      const th = Math.asin(0.62 / 1.08);
      const L = contour(b, PI + th, 1.08);
      const R = contour(b, 2 * PI - th, 1.08);
      const bandLow = cy - ry * 0.45;
      const dome =
        `M${pt(L)}` + poly(arc(b, PI + th, 2 * PI - th, 1.08, 20), false) + "Z";
      const band =
        `M${f(L.x - 1.5)} ${f(L.y)}L${f(R.x + 1.5)} ${f(R.y)}` +
        `L${f(R.x + 1.5)} ${f(bandLow)}L${f(L.x - 1.5)} ${f(bandLow)}Z`;
      const crown = contour(b, top, 1.08);
      return [
        { d: dome, ink: "hair" },
        { d: band, ink: "hair" },
        { d: oval(crown.x, crown.y - 2.6, 2.8), ink: "line" },
      ];
    }
    case "long": {
      const a = PI - 0.5;
      const z = 2 * PI + 0.5;
      const d =
        poly(arc(b, a, z, 1.1, 28)) +
        poly(arc(b, z - 0.05, 2 * PI - 0.3, 0.99, 10), false) +
        `Q${f(cx)} ${f(cy - ry * 0.74)} ${pt(contour(b, PI + 0.3, 0.99))}` +
        poly(arc(b, PI + 0.3, a + 0.05, 0.99, 10), false) +
        "Z";
      return [{ d, ink: "hair" }];
    }
  }
}

function eyeOf(kind: EyeKind, s: number, look: number): Part[] {
  switch (kind) {
    case "dot":
      return [{ d: oval(0, 0, s), ink: "ink" }];
    case "oval":
      return [{ d: oval(0, 0, s * 0.72, s * 1.3), ink: "ink" }];
    case "arc":
      return [{ d: `M${f(-s * 1.4)} ${f(s * 0.5)}Q0 ${f(-s * 1.3)} ${f(s * 1.4)} ${f(s * 0.5)}`, ink: "line" }];
    case "line":
      return [{ d: `M${f(-s * 1.4)} 0L${f(s * 1.4)} 0`, ink: "line" }];
    case "ring":
      return [
        { d: oval(0, 0, s * 1.5), ink: "line" },
        { d: oval(look * s * 0.4, 0, s * 0.62), ink: "ink" },
      ];
  }
}

function browOf(kind: BrowKind, e: Pt, side: number, y: number, w: number, tilt: number): Part[] {
  // `side` is -1 for the left eye, 1 for the right; the inner end faces the
  // nose. Slanted brows mirror, so the face reads as one expression.
  const inner = { x: e.x - side * w, y };
  const outer = { x: e.x + side * w, y };
  switch (kind) {
    case "none":
      return [];
    case "straight":
      return [{ d: `M${pt(inner)}L${pt(outer)}`, ink: "line" }];
    case "arched":
      return [
        {
          d: `M${f(inner.x)} ${f(y + 1)}Q${f(e.x)} ${f(y - 2.6)} ${f(outer.x)} ${f(y + 1.2)}`,
          ink: "line",
        },
      ];
    case "slanted":
      return [{ d: `M${f(inner.x)} ${f(y + tilt)}L${f(outer.x)} ${f(y - tilt)}`, ink: "line" }];
  }
}

/// Everything about how `seed` looks, decided once. Same seed, same face.
export function faceOf(seed: string): Face {
  const h = hash(seed);
  const r = rng(h);

  const head = pick(r, HEAD_KINDS);
  const base = BOXES[head];
  const box: HeadBox = {
    cx: 50,
    cy: 57,
    rx: base.rx + between(r, -1.2, 1.2),
    ry: base.ry + between(r, -1.2, 1.2),
    n: base.n,
    taper: base.taper,
  };
  const { cx, cy, rx, ry } = box;

  const hair = pick(r, HAIR_KINDS, [0.7, 1, 1, 1, 1, 1, 1, 1]);
  let accessory = pick(r, ACCESSORIES, [0.58, 0.16, 0.13, 0.13]);
  // Long hair covers the ears it would draw.
  if (hair === "long" && accessory === "ears") accessory = "none";
  let eyes = pick(r, EYE_KINDS);
  // A ring inside a lens is a target, not an eye.
  if (accessory === "glasses" && eyes === "ring") eyes = "dot";
  const brows = pick(r, BROW_KINDS, [0.8, 1, 1, 1]);
  const mouth = pick(r, MOUTH_KINDS, [1, 1.2, 0.8]);

  const s = between(r, 2.6, 3.6) * (eyes === "line" || eyes === "arc" ? 0.9 : 1);
  const spread = rx * between(r, 0.32, 0.43);
  const ey = cy + ry * between(r, -0.04, 0.06);
  const eyeAt: [Pt, Pt] = [
    { x: cx - spread, y: ey },
    { x: cx + spread, y: ey },
  ];
  const look = between(r, -1, 1);

  const by = ey - s * 1.4 - between(r, 4, 5.5);
  const bw = between(r, 4.2, 6);
  // Inner ends up only: the other way reads as a scowl, and a room of strangers
  // scowling at you is not the room this is.
  const tilt = -between(r, 0.7, 1.4);
  const brow = [
    ...browOf(brows, eyeAt[0], -1, by, bw, tilt),
    ...browOf(brows, eyeAt[1], 1, by, bw, tilt),
  ];

  const mouthAt = { x: cx, y: cy + ry * between(r, 0.44, 0.5) };
  const mouthWidth = (mouth === "small" ? 0.62 : 1) * between(r, 13, 17) * (rx / 29);
  // Room under the mouth before the chin, kept clear of the outline.
  const chin = contour(box, PI / 2).y;
  const mouthOpen = Math.min(
    13,
    ((chin - 4 - mouthAt.y) * 0.75 - smileOf(mouth, mouthWidth)) / MOUTH_DOWN,
  );

  const back: Part[] = [];
  if (accessory === "ears") {
    for (const side of [-1, 1]) {
      const edge = contour(box, side < 0 ? PI : 0, 0.99);
      const out = edge.x + side * 6.5;
      back.push({
        d:
          `M${f(edge.x)} ${f(cy - 6)}` +
          `C${f(out)} ${f(cy - 8)} ${f(out)} ${f(cy + 7)} ${f(edge.x)} ${f(cy + 5)}`,
        ink: "skin",
      });
    }
  }
  back.push({ d: poly(arc(box, 0, 2 * PI, 1, 72)) + "Z", ink: "skin" });
  back.push(...hairOf(hair, box, r));

  const front: Part[] = [];
  if (accessory === "glasses") {
    const lr = s * 1.5 + 3.4;
    const round = r() < 0.5;
    for (const e of eyeAt) {
      front.push({ d: round ? oval(e.x, e.y, lr) : oval(e.x, e.y, lr * 1.12, lr * 0.84), ink: "line" });
    }
    const gap = round ? lr : lr * 1.12;
    front.push({
      d: `M${f(eyeAt[0].x + gap)} ${f(ey)}Q${f(cx)} ${f(ey - 2.4)} ${f(eyeAt[1].x - gap)} ${f(ey)}`,
      ink: "line",
    });
  } else if (accessory === "freckles") {
    for (const e of eyeAt) {
      const fy = ey + s + 5.5;
      for (const [dx, dy] of [
        [-2.6, 0],
        [0, 1.6],
        [2.6, 0.2],
      ]) {
        front.push({ d: oval(e.x + dx, fy + dy, 0.95), ink: "ink" });
      }
    }
  }

  return {
    head,
    eyes,
    brows,
    hair,
    accessory,
    mouth,
    box,
    back,
    eyeAt,
    eye: eyeOf(eyes, s, look),
    brow,
    mouthAt,
    mouthWidth,
    mouthOpen,
    front,
    phase: r() * PI * 2,
    blinkSeed: (h ^ 0x9e3779b9) >>> 0,
  };
}

// ── the mouth ───────────────────────────────────────────────────────────

/// Share of the opening that goes below the resting line: speech is mostly
/// the jaw dropping, very little the upper lip rising.
const MOUTH_UP = 0.28;
const MOUTH_DOWN = 0.72;

/// How far a closed mouth's middle sits below its corners.
function smileOf(kind: MouthKind, width: number): number {
  return kind === "smile" ? width * 0.12 : kind === "small" ? width * 0.06 : 0;
}

export type MouthShape = {
  /// Half-width at this openness.
  hw: number;
  /// Horizontal reach of the control points, as a share of `hw`.
  k: number;
  /// y of the upper and lower lip's control points (local; + is down).
  upper: number;
  lower: number;
};

/// The mouth at `openness` 0..1, around its centre. Closed, both lips are the
/// same curve — a short line or a soft smile. Opening, the jaw drops, the
/// corners draw in and the shape rounds out toward an "o".
export function mouthShape(openness: number, kind: MouthKind, width = 15, maxOpen = 12): MouthShape {
  const o = Math.max(0, Math.min(1, Number.isFinite(openness) ? openness : 0));
  const hw = (width / 2) * (1 - 0.28 * o);
  // A smile's middle sits below its corners; an open mouth's upper lip
  // straightens, which is what makes it look like speech and not a grin.
  const curve = smileOf(kind, width);
  const h = o * maxOpen;
  // A cubic whose two controls share a y reaches 3/4 of the way to them.
  return {
    hw,
    k: 0.5 + 0.5 * o,
    upper: (curve * (1 - o) - h * MOUTH_UP) / 0.75,
    lower: (curve + h * MOUTH_DOWN) / 0.75,
  };
}

/// Distance between the lips' midpoints — how open the mouth reads.
export function mouthHeight(m: MouthShape): number {
  return (m.lower - m.upper) * 0.75;
}

/// The mouth as a path in local coordinates (translate it to `mouthAt`).
/// The one string the frame loop builds, and only when the shape changed.
export function mouthPath(openness: number, kind: MouthKind, width = 15, maxOpen = 12): string {
  const m = mouthShape(openness, kind, width, maxOpen);
  const x = m.hw;
  const c = m.hw * m.k;
  const u = m.upper.toFixed(2);
  const l = m.lower.toFixed(2);
  return (
    `M${(-x).toFixed(2)} 0C${(-c).toFixed(2)} ${u} ${c.toFixed(2)} ${u} ${x.toFixed(2)} 0` +
    `C${c.toFixed(2)} ${l} ${(-c).toFixed(2)} ${l} ${(-x).toFixed(2)} 0Z`
  );
}

// ── the animation ───────────────────────────────────────────────────────

/// Per-second rates of the mouth's smoothing. A syllable's onset is sharp and
/// its decay is not: opening at ~25/s keeps up with 4–8 Hz syllables, closing
/// at ~10/s keeps a 20 ms dip between frames from snapping the mouth shut.
export const MOUTH_ATTACK = 25;
export const MOUTH_RELEASE = 10;
/// Reduced motion: the mouth still opens (it is information), but softer.
export const MOUTH_ATTACK_CALM = 12;
export const MOUTH_RELEASE_CALM = 5;

/// One-pole smoothing toward `target` over `dt` seconds, faster up than down.
/// Exponential, so it never overshoots and doesn't depend on the frame rate.
export function smoothLevel(
  prev: number,
  target: number,
  dt: number,
  attack = MOUTH_ATTACK,
  release = MOUTH_RELEASE,
): number {
  const rate = target > prev ? attack : release;
  return prev + (target - prev) * (1 - Math.exp(-rate * Math.max(0, dt)));
}

/// Eyes stay open 2.5–6 s between blinks — inside the human range, and
/// irregular enough that a room of faces never blinks in step.
export const BLINK_MIN_MS = 2500;
export const BLINK_MAX_MS = 6000;
export const BLINK_MS = 120;

/// When the next blink starts, from `now` (ms).
export function nextBlink(rand: () => number, now: number): number {
  return now + BLINK_MIN_MS + rand() * (BLINK_MAX_MS - BLINK_MIN_MS);
}

/// How shut the eyes are `since` ms into a blink: 0 open, 1 shut, a quick
/// half-sine. Outside the blink, 0.
export function blinkAmount(since: number): number {
  if (!(since >= 0) || since >= BLINK_MS) return 0;
  return Math.sin((PI * since) / BLINK_MS);
}

/// Brows rise only on a loud syllable — above everyday speech (≈0.64 on the
/// room's scale), so they punctuate rather than twitch.
export const BROW_AT = 0.78;
/// How far they go, in viewBox units.
export const BROW_RISE = 2.6;

/// 0..1: how raised the brows are at this level.
export function browLift(level: number): number {
  const x = Math.max(0, Math.min(1, (level - BROW_AT) / (1 - BROW_AT)));
  return x * x * (3 - 2 * x);
}

/// The head rises with the voice and sways a little while it talks. In
/// viewBox units: 3 is 1.2–2.5 px at the sizes the room draws.
export const BOB_MAX = 3;
export const TILT_MAX = 1.8;
/// Sway speed, in cycles per second — slow enough to read as posture.
export const TILT_HZ = 0.55;

/// Upward shift of the head at `level` (negative = up).
export function bobY(level: number): number {
  const l = Math.max(0, Math.min(1, level));
  return -BOB_MAX * l ** 1.4;
}

/// Head tilt in degrees at `level`, `t` seconds in, with the face's phase.
export function bobTilt(level: number, t: number, phase: number): number {
  const l = Math.max(0, Math.min(1, level));
  return TILT_MAX * l * Math.sin(t * TILT_HZ * 2 * PI + phase);
}
