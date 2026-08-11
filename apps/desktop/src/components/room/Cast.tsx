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
// peer would re-run every dependent memo for no visual gain. That same loop
// feeds each name's falling ribbon (`lib/trace.ts`) — one place knows how loud
// anyone is, and it stays this one.

import { For, Show, createEffect, createMemo, createSignal, onCleanup, onMount } from "solid-js";
import { state } from "../../lib/store";
import { tint, hueOf } from "../../lib/tint";
import { leaveChannel, setPeerVolume, toggleMute } from "../../lib/actions";
import { closeDm } from "../../lib/dm";
import { tweaks } from "../../lib/theme";
import { t } from "../../lib/i18n";
import { nameNodes } from "../../lib/stage";
import { Trace } from "../../lib/trace";
import { Lava } from "../../lib/lava";
import { Meter, Word } from "./words";

/// Most names that ever stand at once. Past this the room stops being a room.
const STAND_MAX = 14;
/// A swap costs the eye a jump — never more often than this.
const SWAP_EVERY = 2.5;
/// Crowded rooms recede harder so a small name still reads as speaking.
const CROWD_AT = 8;
/// How long a volume knob stays out after you stop touching it.
const TUNE_LINGER = 6000;
/// Loudest a peer can be made, matching `config::PEER_VOL_MAX_PCT`.
const PEER_VOL_MAX = 200;

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
  /// What paints the voice under each standing name — a string or the drops,
  /// depending on the setting. Either way it is fed from the frame loop below,
  /// so there is still exactly one place that knows how loud anyone is.
  const painters = new Map<string, (level: number, now: number) => void>();
  let lastSwap = 0;
  let host: HTMLDivElement | undefined;

  const showAll = () => tweaks.crowd === "all";

  /// Whose voice you are currently tuning. One at a time: this is an aside in
  /// the middle of a conversation, not a mixing desk.
  const [tuning, setTuning] = createSignal<string | null>(null);
  let tuneTimer: number | undefined;
  /// It closes itself, because nobody thinks to put a volume knob away — but
  /// only after a pause, so a series of nudges keeps it open.
  const holdTuning = (name: string | null) => {
    if (tuneTimer) clearTimeout(tuneTimer);
    setTuning(name);
    if (name) tuneTimer = window.setTimeout(() => setTuning(null), TUNE_LINGER);
  };
  onCleanup(() => tuneTimer && clearTimeout(tuneTimer));

  const volumeOf = (name: string) => state.peerVolumes[name] ?? 100;

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
        painters.get(name)?.(next, now);
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

  /// Swapping the look disposes one branch and creates the other, and the two
  /// are not ordered — so a painter only ever retires its own entry.
  const own = (name: string, paint: (level: number, now: number) => void) => {
    painters.set(name, paint);
    onCleanup(() => {
      if (painters.get(name) === paint) painters.delete(name);
    });
  };

  const bindTrace = (name: string) => (el: SVGPathElement) => {
    const trace = new Trace();
    let lit = false;
    own(name, (level, now) => {
      const d = trace.step(level, now);
      if (d) el.setAttribute("d", d);
      // Showing and hiding the string is CSS's job — this only says when.
      if (trace.audible !== lit) {
        lit = trace.audible;
        el.classList.toggle("live", lit);
      }
    });
  };

  const bindLava = (name: string) => (el: HTMLDivElement) => {
    const lava = new Lava(el);
    own(name, (level, now) => lava.step(level, now));
  };

  return (
    <div
      class="cast"
      ref={host}
      classList={{ crowded: standing().length > CROWD_AT }}
      style={{ "--nameSize": `${size().toFixed(1)}px` }}
    >
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
                        title={name === me() ? t("room.you") : t("room.tune")}
                        onClick={() => {
                          // Your own voice isn't a peer gain, and a DM carries
                          // no voice at all.
                          if (name === me() || state.dm) return;
                          holdTuning(tuning() === name ? null : name);
                        }}
                      >
                        {name}
                        <Show when={name === me()}>
                          <span class="name-tail">{t("room.you_tail")}</span>
                        </Show>
                        {/* Someone you have turned down stays turned down for
                            every future call — that must be visible without
                            opening anything. */}
                        <Show when={name !== me() && volumeOf(name) !== 100}>
                          <span class="name-mark" aria-hidden="true">
                            °
                          </span>
                        </Show>
                      </button>
                      <Show when={tuning() === name}>
                        <div class="tune">
                          <Meter
                            label={t("room.tune")}
                            value={volumeOf(name)}
                            max={PEER_VOL_MAX}
                            unit="%"
                            set={(v) => {
                              holdTuning(name);
                              void setPeerVolume(name, v);
                            }}
                          />
                        </div>
                      </Show>
                      <Show
                        when={tweaks.voiceLook === "lava"}
                        fallback={
                          /* The string the voice runs down. `non-scaling-stroke`
                             is load-bearing: the viewBox is stretched to the
                             element, so a scaled stroke would smear. */
                          <svg
                            class="trace"
                            viewBox="-1 0 2 1"
                            preserveAspectRatio="none"
                            style={{ color: tint(name) }}
                            aria-hidden="true"
                          >
                            <path
                              ref={bindTrace(name)}
                              fill="none"
                              stroke="currentColor"
                              stroke-width="1.25"
                              stroke-linecap="round"
                              stroke-linejoin="round"
                              vector-effect="non-scaling-stroke"
                            />
                          </svg>
                        }
                      >
                        {/* The same slot, filled by drops instead. They are
                            born, animated and removed by CSS — this is only
                            where they live. */}
                        <div
                          class="bubbles"
                          ref={bindLava(name)}
                          style={{ color: tint(name) }}
                          aria-hidden="true"
                        />
                      </Show>
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
