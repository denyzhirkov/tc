import { test } from "node:test";
import assert from "node:assert/strict";
import {
  ENV_HOLD_MS,
  Envelope,
  FAST_DEAD_MS,
  FAST_STALE_MS,
  LIFT_AT,
  VOICE_GATE,
  fastFresh,
  loudnessOfRms,
  resolveLevel,
  voiceLevel,
  type LevelQuery,
} from "./loudness.ts";

const dbfs = (db: number) => 10 ** (db / 20);

test("loudnessOfRms mirrors the core's dBFS window", () => {
  assert.equal(loudnessOfRms(0), 0);
  assert.ok(Math.abs(loudnessOfRms(dbfs(-50))) < 1e-9);
  assert.ok(Math.abs(loudnessOfRms(dbfs(-12)) - 1) < 1e-9);
  assert.equal(loudnessOfRms(4), 1);
  for (const x of [-1, Number.NaN, Number.POSITIVE_INFINITY]) assert.equal(loudnessOfRms(x), 0);
  // The AGC target (≈ −21 dBFS) is everyday speech: ≈ 0.76.
  const speech = loudnessOfRms(dbfs(-21));
  assert.ok(speech > 0.7 && speech < 0.8, `${speech}`);
});

test("voiceLevel: room noise is silence, speech lands where the old proxy did", () => {
  assert.equal(voiceLevel(0), 0);
  assert.equal(voiceLevel(Number.NaN), 0);
  // An open microphone in a quiet room, a fan: −45..−40 dBFS.
  assert.equal(voiceLevel(loudnessOfRms(dbfs(-45))), 0);
  assert.equal(voiceLevel(loudnessOfRms(dbfs(-40))), 0);
  assert.equal(voiceLevel(VOICE_GATE), 0);
  // Everyday speech read 0.6–0.8 on the packet-size proxy everything was tuned on.
  const speech = voiceLevel(loudnessOfRms(dbfs(-21)));
  assert.ok(speech > 0.55 && speech < 0.75, `${speech}`);
  assert.equal(voiceLevel(1), 1);
  assert.equal(voiceLevel(2), 1);
  let prev = 0;
  for (let i = 0; i <= 100; i++) {
    const v = voiceLevel(i / 100);
    assert.ok(v >= prev);
    prev = v;
  }
});

test("room thresholds sit where they did on the old scale", () => {
  // The sender's VAD threshold (≈ −36 dBFS) is where a voice starts to count:
  // the crowd's "spoke" (0.1) and the score's audible floor (0.08) sit at it,
  // a lift from the quiet line (0.2) needs a clearly voiced frame above it.
  const vad = voiceLevel(loudnessOfRms(dbfs(-36)));
  assert.ok(vad > 0.08 && vad < 0.12, `${vad}`);
  assert.ok(voiceLevel(loudnessOfRms(dbfs(-33))) > LIFT_AT);
  assert.ok(voiceLevel(loudnessOfRms(dbfs(-36))) < LIFT_AT);
});

test("fastFresh: the lane speaks for the room until it dies mid-voice", () => {
  assert.equal(fastFresh(0, false, 10_000), false, "no frame yet");
  assert.equal(fastFresh(0, true, 10_000), false);
  assert.equal(fastFresh(1000, true, 1000 + FAST_STALE_MS), true);
  assert.equal(fastFresh(1000, true, 1001 + FAST_STALE_MS), false, "stalled mid-voice");
  // Quiet by contract: the all-zero frame holds however long nobody speaks.
  assert.equal(fastFresh(1000, false, 1_000_000), true);
});

const NOW = 100_000;
const ask = (over: Partial<LevelQuery>): number =>
  resolveLevel({
    now: NOW,
    dm: false,
    isMe: false,
    muted: false,
    ptt: false,
    fastAt: NOW - 30,
    fastAudible: true,
    fast: 0.9,
    slow: 0.5,
    ...over,
  });

test("resolveLevel: a DM has no voice, a muted self has none either", () => {
  assert.equal(ask({ dm: true }), 0);
  assert.equal(ask({ dm: true, isMe: true }), 0);
  assert.equal(ask({ isMe: true, muted: true }), 0);
  assert.equal(ask({ isMe: true, muted: true, fastAt: 0 }), 0);
  // Muting is about you; a peer is unaffected by your mute flag.
  assert.equal(ask({ muted: true }), voiceLevel(0.9));
});

test("resolveLevel: the fast lane wins while it is fresh, for you and for peers", () => {
  assert.equal(ask({}), voiceLevel(0.9));
  assert.equal(ask({ isMe: true, fast: 0.8, slow: 0.2 }), voiceLevel(0.8));
  // A quiet lane holds its all-zero frame, whatever the 5 Hz path says below the guard.
  assert.equal(ask({ fastAudible: false, fast: 0, fastAt: NOW - 400, slow: 0.4 }), 0);
});

test("resolveLevel: falls back to 5 Hz before the first frame and when the lane stalls mid-voice", () => {
  assert.equal(ask({ fastAt: 0 }), voiceLevel(0.5));
  assert.equal(ask({ fastAt: NOW - FAST_STALE_MS - 1 }), voiceLevel(0.5));
  // Our own 5 Hz level is mic RMS, mapped through the same dBFS window.
  assert.equal(ask({ isMe: true, fastAt: 0, slow: dbfs(-21) }), voiceLevel(loudnessOfRms(dbfs(-21))));
});

test("resolveLevel: push-to-talk never trusts the raw microphone", () => {
  assert.equal(ask({ isMe: true, ptt: true, fastAt: 0, slow: dbfs(-15) }), 0);
  // The lane's own level is what peers heard — PTT held shows, released doesn't.
  assert.equal(ask({ isMe: true, ptt: true, fast: 0.8 }), voiceLevel(0.8));
  assert.equal(ask({ isMe: true, ptt: true, fast: 0, fastAudible: false, fastAt: NOW - 5000, slow: dbfs(-15) }), 0);
  // Peers are untouched by our voice mode.
  assert.equal(ask({ ptt: true, fastAt: 0 }), voiceLevel(0.5));
});

test("resolveLevel: a lane that died in a quiet room is noticed by a voiced peer", () => {
  const quiet = { fastAudible: false, fast: 0 };
  // Recent quiet frame: trust it.
  assert.equal(ask({ ...quiet, fastAt: NOW - FAST_DEAD_MS + 10, slow: 0.9 }), 0);
  // Long-quiet lane, 5 Hz hears a clear voice: the lane is dead.
  assert.equal(ask({ ...quiet, fastAt: NOW - FAST_DEAD_MS - 10, slow: 0.9 }), voiceLevel(0.9));
  // ...but not on a level that barely clears the gate.
  assert.equal(ask({ ...quiet, fastAt: NOW - FAST_DEAD_MS - 10, slow: 0.35 }), 0);
  // Never for ourselves: our mic can be loud with nothing sent.
  assert.equal(ask({ ...quiet, isMe: true, fastAt: NOW - FAST_DEAD_MS - 10, slow: dbfs(-15) }), 0);
});

/// Steps an envelope at 60 fps over `secs`, fed by `level(t)`; returns values.
function run(env: Envelope, from: number, secs: number, level: (t: number) => number): number[] {
  const out: number[] = [];
  for (let ms = from; ms < from + secs * 1000; ms += 1000 / 60) out.push(env.step(level(ms / 1000), ms));
  return out;
}

test("Envelope: swells steadily through syllables, settles after the phrase", () => {
  const env = new Envelope();
  assert.deepEqual(run(env, 0, 1, () => 0).filter((v) => v !== 0), [], "silence stays still");
  // Speech: syllables at 5 Hz, dropping to near zero between them.
  const syl = (t: number) => (Math.sin(t * 2 * Math.PI * 5) > -0.3 ? 0.7 : 0.02);
  const talk = run(env, 1000, 2, syl);
  const settled = talk.slice(talk.length / 2);
  const lo = Math.min(...settled);
  const hi = Math.max(...settled);
  assert.ok(lo > 0.6, `dips to ${lo} between syllables`);
  assert.ok(hi - lo < 0.06, `ripple ${hi - lo}`);
  // The phrase ends: held briefly, then gone within ~1.5 s.
  const after = run(env, 3000, 2, () => 0);
  assert.ok(after[Math.floor((ENV_HOLD_MS / 1000) * 60) - 2] > 0.6, "the peak is held");
  assert.ok(after[90] < 0.06, `still ${after[90]} after 1.5 s`);
  // Once the hold is over it only ever falls (during the hold it may still
  // finish rising to the peak it holds).
  const held = Math.ceil((ENV_HOLD_MS / 1000) * 60) + 1;
  for (let i = held + 1; i < after.length; i++) assert.ok(after[i] <= after[i - 1] + 1e-12, `${i}`);
});
