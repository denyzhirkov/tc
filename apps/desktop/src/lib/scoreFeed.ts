// Feeds the score's recorder from the store. App-wide rather than owned by
// the score view, so a trip to settings and back — or switching the room look
// mid-call — keeps the last two minutes instead of starting blank.
//
// Cost when nobody is looking: one ring write per audible voice per
// `voice_level` payload (5 Hz), and nothing at all for silence.

import { createEffect, on, untrack } from "solid-js";
import { state, type LogLine } from "./store";
import { speakerOf } from "./stage";
import { SCORE_AUDIBLE, ScoreRecorder } from "./score";
import { transcript } from "./transcript";
import { levelOf, slowLevel } from "./live";
import { voiceLevel } from "./loudness";

export const score = new ScoreRecorder();

/// A live line is stamped with whole seconds on arrival; one stamped within
/// this long is placed at the moment it was seen instead, or every line would
/// land up to a second (36 px) left of where it was said.
const FRESH = 1.5;

/// How loud `name` is right now, on the room's scale (`lib/loudness.ts`):
/// your own voice is your microphone unless muted, and a DM has no voice at
/// all. Peers come from the 5 Hz payload, not the fast lane — the score stores
/// one bar per payload, and its hold between samples is sized for that rate.
/// Your own row takes the fast lane when it has one: that is what peers heard,
/// after VAD and push-to-talk, where the 5 Hz `input_peak` is the raw mic.
export function levelNow(name: string): number {
  return name === state.status?.name ? levelOf(name) : slowLevel(name);
}

/// Call once at startup, inside the app's root.
export function installScoreRecorder() {
  // A different room is a different conversation.
  createEffect(
    on(
      () => (state.dm ? `dm:${state.dm.pubkey_hex}` : state.channel ? `ch:${state.channel}` : ""),
      () => {
        score.clear();
        transcript.clear();
      },
    ),
  );

  // A rename is an in-place replacement in the roster (`renameParticipant`);
  // a leave or a join changes its length, so the two cannot be confused.
  let roster: readonly string[] = [];
  createEffect(() => {
    const next = state.participants;
    const prev = roster;
    roster = [...next];
    if (prev.length !== next.length) return;
    let at = -1;
    for (let i = 0; i < next.length; i++) {
      if (prev[i] === next[i]) continue;
      if (at !== -1) return;
      at = i;
    }
    if (at !== -1 && !prev.includes(next[at]) && !next.includes(prev[at])) {
      score.rename(prev[at], next[at]);
      transcript.rename(prev[at], next[at]);
    }
  });

  // Track `speakers`, not `voice`: a store write of an object onto an object
  // merges into the same proxy, so `state.voice` itself never changes between
  // payloads — the array inside it is replaced on every one.
  createEffect(() => {
    const v = state.voice;
    if (!v) {
      // The call stopped reporting (left, stalled, dropped): nobody is still
      // "speaking now" on the strength of a sample from before it did.
      transcript.sweep(Number.POSITIVE_INFINITY);
      return;
    }
    void v.speakers;
    untrack(() => {
      if (state.dm || !state.channel) return;
      const t = Date.now() / 1000;
      const heard = (name: string, level: number) => {
        score.record(name, level, t);
        if (level >= SCORE_AUDIBLE) transcript.hear(name, t);
      };
      for (const s of v.speakers) heard(s.name, voiceLevel(s.level));
      const me = state.status?.name;
      // The payload's own mute flag: `wire` copies it into status only after
      // this effect has run.
      if (me) heard(me, v.muted ? 0 : levelNow(me));
      transcript.sweep(t);
    });
  });

  // Lines already in the log at startup are read too: what was said in the
  // last two minutes belongs on the score at the time it was said.
  let seenLog = -1;
  let seenDm = -1;
  const take = (lines: readonly LogLine[], seen: number, active: boolean): number => {
    if (!lines.length) return seen;
    const now = Date.now() / 1000;
    const me = untrack(() => state.status?.name ?? null);
    // What the app itself says — someone joined, a command answered — belongs
    // to the transcript wherever you are standing, room or private thread.
    const somewhere = untrack(() => !!state.channel || !!state.dm);
    for (const line of lines) {
      if (line.id <= seen) continue;
      seen = line.id;
      if (somewhere && (line.kind === "system" || line.kind === "error")) {
        transcript.event(line.text, now, line.kind === "error");
        continue;
      }
      if (!active || (line.kind !== "chat" && line.kind !== "dm")) continue;
      const parsed = speakerOf(line.text);
      if (!parsed) continue;
      const mine = parsed.who === "you";
      const who = mine && me ? me : parsed.who;
      const at = line.ts === undefined || now - line.ts < FRESH ? now : line.ts;
      score.addLyric(who, parsed.said, at, now);
      transcript.text(who, parsed.said, at, mine);
    }
    return seen;
  };
  createEffect(() => {
    const lines = state.log;
    const active = untrack(() => !state.dm && !!state.channel);
    seenLog = take(lines, seenLog, active);
  });
  createEffect(() => {
    const lines = state.dmLog;
    const active = untrack(() => !!state.dm);
    seenDm = take(lines, seenDm, active);
  });
}
