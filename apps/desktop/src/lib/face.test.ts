import { test } from "node:test";
import assert from "node:assert/strict";
import {
  ACCESSORIES,
  BLINK_MAX_MS,
  BLINK_MIN_MS,
  BLINK_MS,
  BOB_MAX,
  BROW_KINDS,
  EYE_KINDS,
  HAIR_KINDS,
  HEAD_KINDS,
  MOUTH_KINDS,
  TILT_MAX,
  blinkAmount,
  bobTilt,
  bobY,
  browLift,
  faceOf,
  mouthHeight,
  mouthPath,
  mouthShape,
  nextBlink,
  rng,
  smoothLevel,
  type Face,
  type Pt,
} from "./face.ts";

const seeds = Array.from({ length: 200 }, (_, i) =>
  i % 3 === 0 ? `user-${i}` : i % 3 === 1 ? `${"abcdefghij"[i % 10]}${i * 7919}` : `имя${i}`,
);

/// Every number in a path built from absolute M/L/C/Q/Z, as points.
function points(d: string): Pt[] {
  assert.match(d, /^[MLCQZ0-9.\s-]+$/, `unexpected command in ${d}`);
  const n = (d.match(/-?\d+(\.\d+)?/g) ?? []).map(Number);
  assert.equal(n.length % 2, 0, d);
  const out: Pt[] = [];
  for (let i = 0; i < n.length; i += 2) out.push({ x: n[i], y: n[i + 1] });
  return out;
}

function allPoints(face: Face): Pt[] {
  const out: Pt[] = [];
  for (const p of [...face.back, ...face.brow, ...face.front]) out.push(...points(p.d));
  for (const at of face.eyeAt)
    for (const p of face.eye) for (const q of points(p.d)) out.push({ x: q.x + at.x, y: q.y + at.y });
  return out;
}

function inside(poly: Pt[], p: Pt): boolean {
  let hit = false;
  for (let i = 0, j = poly.length - 1; i < poly.length; j = i++) {
    const a = poly[i];
    const b = poly[j];
    if (a.y > p.y !== b.y > p.y && p.x < ((b.x - a.x) * (p.y - a.y)) / (b.y - a.y) + a.x) hit = !hit;
  }
  return hit;
}

/// Points along the mouth's two cubics (not their control points).
function mouthCurve(d: string, at: Pt): Pt[] {
  const p = points(d);
  // M p0, C p1 p2 p3, C p4 p5 p0'
  const segs = [
    [p[0], p[1], p[2], p[3]],
    [p[3], p[4], p[5], p[6]],
  ];
  const out: Pt[] = [];
  for (const [a, b, c, e] of segs) {
    for (let i = 0; i <= 20; i++) {
      const t = i / 20;
      const u = 1 - t;
      out.push({
        x: at.x + u * u * u * a.x + 3 * u * u * t * b.x + 3 * u * t * t * c.x + t * t * t * e.x,
        y: at.y + u * u * u * a.y + 3 * u * u * t * b.y + 3 * u * t * t * c.y + t * t * t * e.y,
      });
    }
  }
  return out;
}

test("faceOf is deterministic per seed", () => {
  for (const s of seeds.slice(0, 40)) assert.deepEqual(faceOf(s), faceOf(s));
});

test("different seeds give different faces", () => {
  const sigs = new Set(seeds.map((s) => JSON.stringify(faceOf(s))));
  assert.equal(sigs.size, seeds.length);
});

test("every kind of feature turns up across 200 seeds", () => {
  const faces = seeds.map(faceOf);
  const seen = (key: keyof Face, kinds: readonly string[]) => {
    const got = new Set(faces.map((f) => f[key] as string));
    for (const k of kinds) assert.ok(got.has(k), `${String(key)} never ${k}`);
  };
  seen("head", HEAD_KINDS);
  seen("eyes", EYE_KINDS);
  seen("brows", BROW_KINDS);
  seen("hair", HAIR_KINDS);
  seen("accessory", ACCESSORIES);
  seen("mouth", MOUTH_KINDS);
  // Accessories are the exception, not the rule.
  const plain = faces.filter((f) => f.accessory === "none").length;
  assert.ok(plain > faces.length * 0.45, `only ${plain} faces without an accessory`);
});

test("every feature stays inside the viewBox", () => {
  for (const s of seeds) {
    const face = faceOf(s);
    for (const p of allPoints(face)) {
      assert.ok(p.x >= 1 && p.x <= 99 && p.y >= 1 && p.y <= 99, `${s}: ${p.x},${p.y}`);
    }
  }
});

test("eyes and a wide-open mouth stay inside the head", () => {
  for (const s of seeds) {
    const face = faceOf(s);
    const head = points(face.back.find((p) => p.ink === "skin" && p.d.length > 400)!.d);
    for (const at of face.eyeAt) assert.ok(inside(head, at), `${s}: eye outside`);
    for (const o of [0, 0.5, 1]) {
      const d = mouthPath(o, face.mouth, face.mouthWidth, face.mouthOpen);
      for (const p of mouthCurve(d, face.mouthAt)) assert.ok(inside(head, p), `${s}@${o}: ${p.x},${p.y}`);
    }
    assert.ok(face.mouthOpen > 5, `${s}: mouth can barely open (${face.mouthOpen})`);
  }
});

test("mouth: closed at silence, opens monotonically, bounded", () => {
  for (const kind of MOUTH_KINDS) {
    const shut = mouthShape(0, kind);
    assert.equal(mouthHeight(shut), 0, `${kind} closed`);
    assert.equal(shut.upper, shut.lower, `${kind} lips coincide`);
    let prev = -1;
    for (let i = 0; i <= 100; i++) {
      const m = mouthShape(i / 100, kind, 15, 12);
      const h = mouthHeight(m);
      assert.ok(h > prev || (i === 0 && h === 0), `${kind} at ${i / 100}: ${h} after ${prev}`);
      prev = h;
      for (const p of points(mouthPath(i / 100, kind, 15, 12))) {
        assert.ok(Math.abs(p.x) <= 7.5 + 1e-9, `${kind} x ${p.x}`);
        assert.ok(p.y >= -12 && p.y <= 14, `${kind} y ${p.y}`);
      }
    }
  }
  // Garbage in is a closed mouth or a fully open one, never a broken path.
  assert.equal(mouthPath(Number.NaN, "line"), mouthPath(0, "line"));
  assert.equal(mouthPath(-3, "smile"), mouthPath(0, "smile"));
  assert.equal(mouthPath(7, "small"), mouthPath(1, "small"));
});

test("smoothLevel: attack faster than release, converges, never overshoots", () => {
  const dt = 1 / 60;
  let up = 0;
  let down = 1;
  let upAt = -1;
  let downAt = -1;
  for (let i = 1; i <= 120; i++) {
    up = smoothLevel(up, 1, dt);
    down = smoothLevel(down, 0, dt);
    assert.ok(up <= 1 && down >= 0);
    if (upAt < 0 && up > 0.9) upAt = i;
    if (downAt < 0 && down < 0.1) downAt = i;
  }
  assert.ok(upAt > 0 && downAt > 0);
  assert.ok(upAt < downAt, `attack ${upAt} frames, release ${downAt}`);
  assert.ok(up > 0.999 && down < 0.001);
  // Frame-rate independent: two half steps land where one full step does.
  const a = smoothLevel(smoothLevel(0, 1, dt / 2), 1, dt / 2);
  assert.ok(Math.abs(a - smoothLevel(0, 1, dt)) < 1e-12);
  assert.equal(smoothLevel(0.4, 0.9, 0), 0.4);
});

test("blinks: intervals within bounds, deterministic for a seeded rand", () => {
  const a = rng(42);
  const b = rng(42);
  let now = 0;
  for (let i = 0; i < 500; i++) {
    const next = nextBlink(a, now);
    assert.equal(next, nextBlink(b, now));
    assert.ok(next - now >= BLINK_MIN_MS && next - now <= BLINK_MAX_MS, `${next - now}`);
    now = next + BLINK_MS;
  }
  assert.equal(blinkAmount(-1), 0);
  assert.equal(blinkAmount(0), 0);
  assert.ok(Math.abs(blinkAmount(BLINK_MS / 2) - 1) < 1e-9);
  assert.equal(blinkAmount(BLINK_MS), 0);
  assert.equal(blinkAmount(Number.NaN), 0);
});

test("brows rise only on a loud syllable", () => {
  assert.equal(browLift(0), 0);
  assert.equal(browLift(0.64), 0, "everyday speech leaves the brows alone");
  assert.equal(browLift(1), 1);
  let prev = 0;
  for (let i = 0; i <= 100; i++) {
    const l = browLift(i / 100);
    assert.ok(l >= prev && l <= 1);
    prev = l;
  }
});

test("head bob is bounded and still at silence", () => {
  assert.equal(Math.abs(bobY(0)), 0);
  assert.equal(Math.abs(bobTilt(0, 12.3, 1)), 0);
  for (let i = 0; i <= 50; i++) {
    const level = i / 25 - 0.5; // includes out-of-range levels
    for (let t = 0; t < 10; t += 0.07) {
      assert.ok(Math.abs(bobY(level)) <= BOB_MAX + 1e-9);
      assert.ok(bobY(level) <= 0, "the head only rises");
      assert.ok(Math.abs(bobTilt(level, t, 2.1)) <= TILT_MAX + 1e-9);
    }
  }
});
