import { test } from "node:test";
import assert from "node:assert/strict";
import {
  SCORE_AUDIBLE,
  SCORE_BAR_MIN,
  SCORE_BAR_SCALE,
  SCORE_HISTORY_SECS,
  SCORE_LYRICS_MAX,
  SCORE_MEASURE_SECS,
  SCORE_PAD,
  SCORE_PX_PER_SEC,
  SCORE_QUIET_ROOM,
  SCORE_RING,
  SCORE_ROW_MAX,
  SCORE_ROW_MIN,
  SCORE_RULER_CLEAR,
  ScoreRecorder,
  barHeight,
  baselineOf,
  clampView,
  clockLabel,
  fadeOf,
  forEachMeasure,
  lyricsFit,
  onCanvas,
  rowHeight,
  spanOf,
  stavesThatFit,
  xOfTime,
} from "./score.ts";

const T0 = 1_760_000_000;

function samples(r: ScoreRecorder, name: string, from = -Infinity, to = Infinity) {
  const out: { t: number; level: number; onset: boolean }[] = [];
  r.forEachInWindow(name, from, to, (t, level, onset) => out.push({ t, level, onset }));
  return out;
}

test("window query is newest first and inclusive of both bounds", () => {
  const r = new ScoreRecorder();
  for (let i = 0; i < 10; i++) r.record("a", 0.5, T0 + i * 0.2);
  const at = (i: number) => T0 + i * 0.2;
  const got = samples(r, "a", at(2), at(5)).map((s) => s.t);
  assert.deepEqual(got, [at(5), at(4), at(3), at(2)]);
  assert.equal(samples(r, "nobody").length, 0);
});

test("silence is not stored", () => {
  const r = new ScoreRecorder();
  r.record("a", 0, T0);
  r.record("a", SCORE_AUDIBLE - 0.001, T0 + 0.2);
  assert.equal(samples(r, "a").length, 0);
  assert.equal(r.lastAudible("a"), -Infinity);
  r.record("a", SCORE_AUDIBLE, T0 + 0.4);
  assert.equal(samples(r, "a").length, 1);
  assert.equal(r.lastAudible("a"), T0 + 0.4);
});

test("onsets mark the first sample after a gap", () => {
  const r = new ScoreRecorder();
  r.record("a", 0.5, T0);
  r.record("a", 0.5, T0 + 0.2);
  r.record("a", 0, T0 + 0.4); // a short breath is not a new phrase
  r.record("a", 0.5, T0 + 0.6);
  r.record("a", 0.5, T0 + 2.0); // 1.4 s of silence
  r.record("a", 0.5, T0 + 2.2);
  const onsets = samples(r, "a")
    .reverse()
    .map((s) => s.onset);
  assert.deepEqual(onsets, [true, false, false, true, false]);
});

test("ring wraps and keeps only the newest samples", () => {
  const r = new ScoreRecorder();
  const n = SCORE_RING + 50;
  // At the stored maximum rate the ring holds exactly the history.
  for (let i = 0; i < n; i++) r.record("a", 0.5, T0 + i * 0.125);
  const got = samples(r, "a");
  assert.equal(got.length, SCORE_RING);
  assert.equal(got[0].t, T0 + (n - 1) * 0.125);
  assert.equal(got[got.length - 1].t, T0 + 50 * 0.125);
});

test("samples older than the history are evicted even before their slot is reused", () => {
  const r = new ScoreRecorder();
  r.record("a", 0.5, T0);
  r.record("b", 0.5, T0 + SCORE_HISTORY_SECS + 5);
  assert.equal(samples(r, "a").length, 0);
  assert.equal(samples(r, "b").length, 1);
  assert.equal(r.since(), T0 + 5);
});

test("faster-than-ring samples and clock steps back are dropped", () => {
  const r = new ScoreRecorder();
  r.record("a", 0.5, T0);
  r.record("a", 0.5, T0 + 0.01);
  r.record("a", 0.5, T0 - 3);
  assert.equal(samples(r, "a").length, 1);
});

test("rename moves voice and lyrics to the new name", () => {
  const r = new ScoreRecorder();
  r.record("old", 0.5, T0);
  r.addLyric("old", "hi", T0, T0);
  r.rename("old", "new");
  assert.equal(samples(r, "old").length, 0);
  assert.equal(samples(r, "new").length, 1);
  assert.equal(r.lastAudible("new"), T0);
  const who: string[] = [];
  r.lyricsInWindow(-Infinity, Infinity, (l) => who.push(l.who));
  assert.deepEqual(who, ["new"]);
});

test("a chat line counts as being heard", () => {
  const r = new ScoreRecorder();
  assert.equal(r.lastHeard("a"), -Infinity);
  r.record("a", 0.5, T0);
  assert.equal(r.lastHeard("a"), T0);
  r.addLyric("a", "hi", T0 + 5, T0 + 5);
  assert.equal(r.lastHeard("a"), T0 + 5);
  assert.equal(r.lastAudible("a"), T0);
  // An older line arriving late does not move it back.
  r.addLyric("a", "earlier", T0 + 1, T0 + 6);
  assert.equal(r.lastHeard("a"), T0 + 5);
  r.addLyric("b", "only wrote", T0 + 2, T0 + 6);
  assert.equal(r.lastHeard("b"), T0 + 2);
  r.rename("b", "c");
  assert.equal(r.lastHeard("b"), -Infinity);
  assert.equal(r.lastHeard("c"), T0 + 2);
  r.clear();
  assert.equal(r.lastHeard("a"), -Infinity);
});

test("staves: everyone who fits, otherwise room for the quiet line", () => {
  assert.equal(stavesThatFit(1000, 5), 5);
  assert.equal(stavesThatFit(5 * SCORE_ROW_MIN, 5), 5);
  // One too many: the quiet line takes its room out of the staves.
  const avail = 10 * SCORE_ROW_MIN;
  assert.equal(stavesThatFit(avail, 11), Math.floor((avail - SCORE_QUIET_ROOM) / SCORE_ROW_MIN));
  assert.ok(stavesThatFit(avail, 40) * SCORE_ROW_MIN + SCORE_QUIET_ROOM <= avail);
  // Never fewer than one, even in a window too small for anything.
  assert.equal(stavesThatFit(10, 40), 1);
  assert.equal(stavesThatFit(0, 0), 0);
});

test("clear forgets everything", () => {
  const r = new ScoreRecorder();
  r.record("a", 0.5, T0);
  r.addLyric("a", "x", T0, T0);
  r.clear();
  assert.equal(samples(r, "a").length, 0);
  assert.equal(r.since(), Infinity);
  let n = 0;
  r.lyricsInWindow(-Infinity, Infinity, () => n++);
  assert.equal(n, 0);
});

test("lyrics: window, order, age cap and count cap", () => {
  const r = new ScoreRecorder();
  const now = T0 + 200;
  // A replay of stored chat: too old to be part of this conversation.
  r.addLyric("a", "ancient", now - SCORE_HISTORY_SECS - 1, now);
  r.addLyric("a", "two", now - 10, now);
  r.addLyric("b", "one", now - 20, now); // out of order
  r.addLyric("a", "three", now - 1, now);
  const said: string[] = [];
  r.lyricsInWindow(now - 15, now, (l) => said.push(l.said));
  assert.deepEqual(said, ["three", "two"]);
  const all: string[] = [];
  r.lyricsInWindow(-Infinity, Infinity, (l) => all.push(l.said));
  assert.deepEqual(all, ["three", "two", "one"]);

  // Time moves on: "one" falls out of the history.
  r.addLyric("a", "four", now + SCORE_HISTORY_SECS - 15, now + SCORE_HISTORY_SECS - 15);
  const left: string[] = [];
  r.lyricsInWindow(-Infinity, Infinity, (l) => left.push(l.said));
  assert.deepEqual(left, ["four", "three", "two"]);

  const flood = new ScoreRecorder();
  for (let i = 0; i < SCORE_LYRICS_MAX + 10; i++) flood.addLyric("a", `${i}`, T0 + i * 0.1, T0 + i * 0.1);
  let n = 0;
  let newest = "";
  flood.lyricsInWindow(-Infinity, Infinity, (l) => {
    if (!n) newest = l.said;
    n++;
  });
  assert.equal(n, SCORE_LYRICS_MAX);
  assert.equal(newest, `${SCORE_LYRICS_MAX + 9}`);
});

test("x of time: now sits at the pad, the past runs left at the scroll rate", () => {
  const w = 800;
  assert.equal(xOfTime(T0, T0, w), w - SCORE_PAD);
  assert.equal(xOfTime(T0 - 1, T0, w), w - SCORE_PAD - SCORE_PX_PER_SEC);
  assert.equal(xOfTime(T0 - 2, T0, w, 10, 0), w - 20);
  assert.equal(spanOf(w), (w - SCORE_PAD) / SCORE_PX_PER_SEC);
  // The oldest visible time lands on the left edge.
  assert.ok(Math.abs(xOfTime(T0 - spanOf(w), T0, w)) < 1e-3);
  assert.equal(spanOf(10), 0);
});

test("culling keeps only what touches the canvas", () => {
  assert.ok(onCanvas(0, 100));
  assert.ok(onCanvas(100, 100));
  assert.ok(!onCanvas(-1, 100));
  assert.ok(onCanvas(-1, 100, 2));
  assert.ok(!onCanvas(103, 100, 2));
});

test("bar height is bounded by the minimum and the row", () => {
  assert.equal(barHeight(0, 60), SCORE_BAR_MIN);
  assert.equal(barHeight(-1, 60), SCORE_BAR_MIN);
  assert.equal(barHeight(1, 60), SCORE_BAR_SCALE * 60);
  assert.equal(barHeight(5, 60), SCORE_BAR_SCALE * 60);
  assert.equal(barHeight(0.5, 60), 0.5 * SCORE_BAR_SCALE * 60);
});

test("rows: height clamp, lyrics fit, baseline", () => {
  assert.equal(rowHeight(1000, 2), SCORE_ROW_MAX);
  assert.equal(rowHeight(100, 8), SCORE_ROW_MIN);
  assert.equal(rowHeight(300, 6), 50);
  assert.equal(rowHeight(300, 0), SCORE_ROW_MAX);
  assert.ok(lyricsFit(SCORE_ROW_MAX));
  assert.ok(!lyricsFit(SCORE_ROW_MIN));
  assert.equal(baselineOf(SCORE_ROW_MIN), SCORE_ROW_MIN / 2);
  assert.ok(baselineOf(68) < 34);
});

test("fade runs from full at now to the floor at the left edge", () => {
  assert.equal(fadeOf(0, 20), 1);
  assert.ok(Math.abs(fadeOf(20, 20) - 0.45) < 1e-9);
  assert.ok(Math.abs(fadeOf(99, 20) - 0.45) < 1e-9);
  assert.equal(fadeOf(5, 0), 1);
});

test("measures fall on 10 s boundaries, culled and clear of now", () => {
  const w = 800;
  const view = T0 + 7.5;
  const seen: { t: number; x: number }[] = [];
  forEachMeasure(view, w, (t, x) => seen.push({ t, x }));
  assert.ok(seen.length > 0);
  for (let i = 0; i < seen.length; i++) {
    const { t, x } = seen[i];
    assert.equal(t % SCORE_MEASURE_SECS, 0);
    assert.ok(x >= 0 && w - x >= SCORE_RULER_CLEAR);
    if (i) assert.equal(seen[i - 1].t - t, SCORE_MEASURE_SECS);
  }
  // T0 + 0 is 7.5 s back: 24 + 270 px from the right, so it is drawn.
  assert.equal(seen[0].t, T0);
  // Exactly everything that fits.
  const oldest = seen[seen.length - 1].t;
  assert.ok(xOfTime(oldest - SCORE_MEASURE_SECS, view, w) < 0);
});

test("clock labels are local HH:MM:SS and end in 0 on a measure", () => {
  const t = T0 + 30;
  const d = new Date(t * 1000);
  const two = (n: number) => String(n).padStart(2, "0");
  assert.equal(clockLabel(t), `${two(d.getHours())}:${two(d.getMinutes())}:${two(d.getSeconds())}`);
  assert.match(clockLabel(t), /^\d\d:\d\d:\d0$/);
});

test("scrolling is clamped to the recorded history and resumes at now", () => {
  const now = T0 + 500;
  const span = 20;
  assert.equal(clampView(now, now, span, now - 60), null);
  assert.equal(clampView(now + 3, now, span, now - 60), null);
  assert.equal(clampView(now - 10, now, span, now - 60), now - 10);
  // Not past the oldest sample (its time on the left edge).
  assert.equal(clampView(now - 100, now, span, now - 60), now - 40);
  // Not past the history, however old the oldest sample claims to be.
  assert.equal(clampView(now - 1000, now, span, now - 900), now - SCORE_HISTORY_SECS + span);
  // Nothing older than the window: there is nowhere to go back to.
  assert.equal(clampView(now - 10, now, span, now - 5), null);
  assert.equal(clampView(now - 10, now, span, Infinity), null);
});
