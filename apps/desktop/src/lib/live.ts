// The latest voice levels, as plain values — not a Solid store. The fast lane
// (`voice_levels`, ~30 Hz) writes here and the room's frame loop reads from
// here; a store write that often would re-run every dependent memo for
// nothing. `wire.ts` installs the one writer and clears it when voice stops.
//
// `levelOf` is the single answer to "how loud is this person right now" for
// the names and heads looks; the decision itself is `resolveLevel` in
// `lib/loudness.ts`, where node can test it. This file only gathers its inputs.

import type { VoiceLevelsPayload } from "./tauri";
import { state } from "./store";
import { resolveLevel, type LevelQuery } from "./loudness";

const peers = new Map<string, number>();
let me = 0;
/// `performance.now()` of the last frame; 0 = no frame since voice started.
let at = 0;
/// Whether the last frame had anyone above zero in it.
let audible = false;

export function takeFastLevels(p: VoiceLevelsPayload) {
  peers.clear();
  audible = p.me > 0;
  for (const s of p.peers) {
    peers.set(s.name, s.level);
    if (s.level > 0) audible = true;
  }
  me = p.me;
  at = performance.now();
}

export function clearFastLevels() {
  peers.clear();
  me = 0;
  at = 0;
  audible = false;
}

/// Refilled per call: the frame loop asks once per name per frame.
const q: LevelQuery = {
  now: 0,
  dm: false,
  isMe: false,
  muted: false,
  ptt: false,
  fastAt: 0,
  fastAudible: false,
  fast: 0,
  slow: 0,
};

function ask(name: string, useFast: boolean): number {
  const isMe = name === state.status?.name;
  q.now = performance.now();
  q.dm = !!state.dm;
  q.isMe = isMe;
  q.muted = !!state.status?.muted;
  q.ptt = state.status?.voice_mode === "ptt";
  q.fastAt = useFast ? at : 0;
  q.fastAudible = audible;
  q.fast = isMe ? me : (peers.get(name) ?? 0);
  q.slow = isMe ? (state.voice?.input_peak ?? 0) : (state.speakers[name] ?? 0);
  return resolveLevel(q);
}

/// The 5 Hz path alone, on the room's scale. The score records from this: its
/// geometry (a bar per payload, its hold between samples) is built on 5 Hz.
export function slowLevel(name: string): number {
  return ask(name, false);
}

/// How loud `name` is right now, on the room's scale (`lib/loudness.ts`).
export function levelOf(name: string): number {
  return ask(name, true);
}
