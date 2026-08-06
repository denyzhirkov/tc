// The room: people are their names, and volume is the only thing that moves.
//
// Three regimes, chosen by headcount alone — there is no layout setting:
//   ≤6    one rank, big type
//   7–14  staggered ranks (alternating counts, so names never form a grid)
//   15+   only the recent voices stand; the rest wait in a line below and are
//         lifted back the moment they speak.
//
// Voice is painted straight onto the DOM from a rAF loop rather than through
// signals: levels arrive many times a second and a store write per frame per
// peer would re-run every dependent memo for no visual gain.

import { For, Show, createEffect, createMemo, createSignal, onCleanup, onMount } from "solid-js";
import { state } from "../../lib/store";
import { tint, hueOf } from "../../lib/tint";
import { leaveChannel, toggleMute } from "../../lib/actions";
import { closeDm } from "../../lib/dm";
import { tweaks } from "../../lib/theme";
import { t } from "../../lib/i18n";
import { nameNodes } from "../../lib/stage";
import { Word } from "./words";

/// Most names that ever stand at once. Past this the room stops being a room.
const STAND_MAX = 14;
/// A swap costs the eye a jump — never more often than this.
const SWAP_EVERY = 2.5;
/// Crowded rooms recede harder so a small name still reads as speaking.
const CROWD_AT = 8;

export default function Cast() {
  const me = () => state.status?.name ?? null;

  /// A DM is a room for two — you need to be standing in it as well, or your
  /// own words would have nobody to fall from.
  const roster = () => {
    if (!state.dm) return state.participants;
    const mine = me();
    return mine ? [mine, state.dm.name] : [state.dm.name];
  };

  /// Whose name carries the leave/quiet actions.
  const anchor = () => me() ?? (state.dm ? state.dm.name : null);

  const [standing, setStanding] = createSignal<string[]>([]);
  const [width, setWidth] = createSignal(920);
  const spokeAt = new Map<string, number>();
  const nodes = nameNodes;
  let lastSwap = 0;
  let host: HTMLDivElement | undefined;

  const showAll = () => tweaks.crowd === "all";

  const levelOf = (name: string): number => {
    if (state.dm) return 0;
    if (name === me()) {
      if (state.status?.muted) return 0;
      return Math.min(1, (state.voice?.input_peak ?? 0) * 4);
    }
    return state.speakers[name] ?? 0;
  };

  // Keep the standing set in step with who is actually in the room.
  createEffect(() => {
    const all = roster();
    const cap = showAll() ? all.length : Math.min(STAND_MAX, all.length);
    setStanding((prev) => {
      const kept = prev.filter((n) => all.includes(n));
      const mine = me();
      // You are always on stage — you can't be crowded out of your own call.
      if (mine && all.includes(mine) && !kept.includes(mine)) kept.unshift(mine);
      for (const n of all) {
        if (kept.length >= cap) break;
        if (!kept.includes(n)) kept.push(n);
      }
      return kept.slice(0, Math.max(cap, mine ? 1 : 0));
    });
  });

  /// Someone silent starts talking: they take the place of whoever has been
  /// quiet longest.
  const lift = (name: string, now: number) => {
    if (now - lastSwap < SWAP_EVERY) return;
    const current = standing();
    let victim: string | null = null;
    let oldest = Infinity;
    for (const n of current) {
      if (n === me()) continue;
      const at = spokeAt.get(n) ?? 0;
      if (at < oldest) {
        oldest = at;
        victim = n;
      }
    }
    if (!victim) return;
    lastSwap = now;
    setStanding(current.map((n) => (n === victim ? name : n)));
  };

  const size = createMemo(() => {
    const n = standing().length || 1;
    return Math.max(15, Math.min(32, 34 - Math.max(0, n - 5) * 1.15));
  });

  /// Rank sizes alternate (4, 3, 4) so neighbouring rows fall into each
  /// other's gaps instead of lining up into columns.
  const ranks = createMemo<string[][]>(() => {
    const list = standing();
    if (!list.length) return [];
    const perRow = Math.max(3, Math.min(7, Math.floor(width() / (size() * 4.9))));
    const rows = Math.max(1, Math.ceil(list.length / perRow));
    const base = Math.floor(list.length / rows);
    let rem = list.length % rows;
    const out: string[][] = [];
    let k = 0;
    for (let i = 0; i < rows; i++) {
      const extra = rem > 0 && i % 2 === 0 ? 1 : 0;
      if (extra) rem--;
      out.push(list.slice(k, k + base + extra));
      k += base + extra;
    }
    return out;
  });

  const quiet = createMemo(() => roster().filter((n) => !standing().includes(n)));

  onMount(() => {
    if (host && "ResizeObserver" in window) {
      const ro = new ResizeObserver(([e]) => setWidth(e.contentRect.width));
      ro.observe(host);
      onCleanup(() => ro.disconnect());
    }

    // ── the only animation loop in the app ──
    const levels = new Map<string, number>();
    let roomHue = 40;
    let roomLit = 0;
    let paintedHue = -1;
    let paintedLit = -1;
    let raf = 0;
    let last = performance.now();

    const frame = (now: number) => {
      const dt = Math.min(0.05, (now - last) / 1000);
      last = now;
      const t = now / 1000;
      const crowded = standing().length > CROWD_AT;
      const floor = crowded ? 0.18 : 0.26;
      const swing = crowded ? 0.42 : 0.34;

      let loudName: string | null = null;
      let loudLevel = 0;

      for (const [name, el] of nodes) {
        const goal = levelOf(name);
        const cur = levels.get(name) ?? 0;
        const next = cur + (goal - cur) * Math.min(1, dt * 8);
        levels.set(name, next);
        el.style.transform = `scale(${(0.88 + next * swing).toFixed(3)})`;
        el.style.opacity = (floor + next * (1 - floor)).toFixed(3);
        if (next > 0.1) spokeAt.set(name, t);
        if (next > loudLevel) {
          loudLevel = next;
          loudName = name;
        }
      }

      // Somebody off-stage found their voice — bring them up.
      if (!showAll()) {
        for (const name of quiet()) {
          if (levelOf(name) > 0.12) {
            spokeAt.set(name, t);
            lift(name, t);
            break;
          }
        }
      }

      // The room's tint lags the voice on purpose: hue drifts the short way
      // round the wheel over ~4s, brightness over ~3s. You notice it only
      // after it has already happened.
      if (loudName && loudLevel > 0.2) {
        const turn = ((hueOf(loudName) - roomHue + 540) % 360) - 180;
        roomHue = (roomHue + turn * Math.min(1, dt * 0.26) + 360) % 360;
      }
      const litGoal = loudLevel > 0.15 ? Math.min(1, loudLevel * 0.85) : 0;
      roomLit += (litGoal - roomLit) * Math.min(1, dt * 0.34);
      if (Math.abs(roomLit - paintedLit) > 0.004 || Math.abs(roomHue - paintedHue) > 0.8) {
        paintedLit = roomLit;
        paintedHue = roomHue;
        document.documentElement.style.setProperty(
          "--room",
          roomLit < 0.01
            ? "transparent"
            : `hsl(${roomHue.toFixed(0)} 58% 50% / ${(roomLit * 0.055).toFixed(4)})`,
        );
      }
      raf = requestAnimationFrame(frame);
    };

    if (!matchMedia("(prefers-reduced-motion: reduce)").matches) {
      raf = requestAnimationFrame(frame);
      onCleanup(() => cancelAnimationFrame(raf));
    } else {
      // Static rooms still need presence — just not motion.
      queueMicrotask(() => {
        for (const el of nodes.values()) el.style.opacity = "0.72";
      });
    }
    onCleanup(() => document.documentElement.style.removeProperty("--room"));
  });

  const bind = (name: string) => (el: HTMLElement) => {
    nodes.set(name, el);
    onCleanup(() => nodes.delete(name));
  };

  return (
    <div class="cast" ref={host} style={{ "--nameSize": `${size().toFixed(1)}px` }}>
      <Show
        when={roster().length}
        fallback={<div class="note">{t("room.alone")}</div>}
      >
        <div class="ranks">
          <For each={ranks()}>
            {(rank, ri) => (
              <div
                class="rank"
                classList={{
                  shift:
                    ri() > 0 &&
                    rank.length === ranks()[ri() - 1]?.length &&
                    ri() % 2 === 1,
                }}
              >
                <For each={rank}>
                  {(name) => (
                    <div class="person">
                      <button
                        ref={bind(name)}
                        class="name"
                        classList={{
                          hushed: name === me() && !state.dm && !!state.status?.muted,
                        }}
                        style={{ color: tint(name) }}
                        title={name === me() ? t("room.you") : name}
                      >
                        {name}
                        <Show when={name === me()}>
                          <span class="name-tail">{t("room.you_tail")}</span>
                        </Show>
                      </button>
                      <Show when={name === anchor()}>
                        <div class="acts">
                          {/* No microphone in a DM — it's a written room. */}
                          <Show when={!state.dm}>
                            <Word tone="quiet" onClick={() => toggleMute()}>
                              {state.status?.muted ? t("room.speak") : t("room.hush")}
                            </Word>
                          </Show>
                          <Word
                            tone="danger"
                            onClick={() => (state.dm ? closeDm() : leaveChannel())}
                          >
                            {t("room.leave")}
                          </Word>
                        </div>
                      </Show>
                    </div>
                  )}
                </For>
              </div>
            )}
          </For>
        </div>
      </Show>

      <Show when={quiet().length}>
        <div class="quiet">
          <span>{t("room.silent", { count: quiet().length })}</span>
          <For each={quiet()}>
            {(name) => (
              <Word
                tone="quiet"
                style={{ color: tint(name), opacity: 0.45 }}
                title={t("room.lift")}
                onClick={() => lift(name, performance.now() / 1000 + SWAP_EVERY)}
              >
                {name}
              </Word>
            )}
          </For>
        </div>
      </Show>
    </div>
  );
}
