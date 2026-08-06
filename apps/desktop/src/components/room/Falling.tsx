// Speech falls from the person who said it and fades on the way down.
//
// Distance and life scale with the crowd: alone, a line crosses the whole room
// over ~14s; in a full room it drops a single rank and is gone in ~7, because
// fifteen long-lived lines would overlap into noise. One line per voice at a
// time — a new one replaces the speaker's previous line rather than stacking.

import { createEffect, onCleanup, onMount } from "solid-js";
import { state, type LogLine } from "../../lib/store";
import { nameNodes, speakerOf } from "../../lib/stage";
import { tint } from "../../lib/tint";

const CROWD_AT = 6;

export default function Falling() {
  let host: HTMLDivElement | undefined;
  let seen = -1;
  const live = new Map<string, HTMLElement>();
  // Timers are tracked here, not via onCleanup: drop() runs inside an effect,
  // and an onCleanup registered there fires on the effect's *next* run — which
  // would cancel every pending removal the moment anyone else spoke.
  const timers = new Set<number>();

  const lines = (): readonly LogLine[] => (state.dm ? state.dmLog : state.log);

  onMount(() => {
    // Don't replay history as a shower of words on mount.
    const l = lines();
    seen = l.length ? l[l.length - 1].id : -1;
  });

  const drop = (who: string, said: string, mine: boolean) => {
    const node = nameNodes.get(who);
    if (!node || !host) return;
    const box = node.getBoundingClientRect();
    const room = host.getBoundingClientRect();
    if (!room.height) return;

    live.get(who)?.remove();

    const crowded = nameNodes.size > CROWD_AT;
    const el = document.createElement("div");
    el.className = crowded ? "said tight" : "said";
    el.textContent = said;
    el.style.color = tint(who);
    if (mine) el.style.fontWeight = "500";
    el.style.left = `${Math.max(12, Math.min(room.width - 12, box.left + box.width / 2 - room.left))}px`;
    el.style.top = `${Math.max(0, box.bottom - room.top + (crowded ? 8 : 20))}px`;

    const size = parseFloat(
      getComputedStyle(node).getPropertyValue("font-size") || "28",
    );
    const far = Math.max(60, room.height - (box.bottom - room.top) - 60);
    const life = crowded ? 6.5 + Math.random() * 1.5 : 13 + Math.random() * 3;
    el.style.setProperty("--life", `${life}s`);
    el.style.setProperty("--drop", `${crowded ? Math.min(size * 1.8, far) : far}px`);

    host.appendChild(el);
    live.set(who, el);
    const timer = window.setTimeout(() => {
      el.remove();
      timers.delete(timer);
      if (live.get(who) === el) live.delete(who);
    }, life * 1000 + 200);
    timers.add(timer);
  };

  createEffect(() => {
    const l = lines();
    if (!l.length) return;
    const me = state.status?.name ?? null;
    for (const line of l) {
      if (line.id <= seen) continue;
      seen = line.id;
      if (line.kind !== "chat" && line.kind !== "dm") continue;
      const parsed = speakerOf(line.text);
      if (!parsed) continue;
      // Your own sends are logged as "you: …" — resolve that to your name so
      // the words fall from where you actually stand.
      const mine = parsed.who === "you";
      const who = mine && me ? me : parsed.who;
      drop(who, parsed.said, mine);
    }
  });

  onCleanup(() => {
    for (const timer of timers) window.clearTimeout(timer);
    timers.clear();
    for (const el of live.values()) el.remove();
    live.clear();
  });

  return <div class="fall" ref={host} aria-hidden="true" />;
}
