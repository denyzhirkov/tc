// The room as a score: the conversation over time instead of the present.
// One staff per person; loudness runs right→left as bars, a dot marks where
// a talkspurt starts, chat lines sit under the sender's staff at the moment
// they were sent, and a cursor at the right edge is now.
//
// Names are real buttons in a column on the left; everything that moves is
// one canvas. Both are painted from a single frame loop — names by direct
// DOM writes, the staves by drawing — and nothing is written to the store
// per frame. What is drawn comes from the app-wide recorder
// (`lib/scoreFeed.ts`), so the score keeps its past across view switches.

import { For, Show, createEffect, createMemo, createSignal, on, onCleanup, onMount } from "solid-js";
import { state } from "../../lib/store";
import { hueOf, tint } from "../../lib/tint";
import { leaveChannel, setPeerVolume, toggleMute } from "../../lib/actions";
import { closeDm } from "../../lib/dm";
import { tweaks } from "../../lib/theme";
import { t } from "../../lib/i18n";
import { levelNow, score } from "../../lib/scoreFeed";
import {
  SCORE_AUDIBLE,
  SCORE_BAR_WIDTH,
  SCORE_CURSOR_INSET,
  SCORE_HISTORY_SECS,
  SCORE_LIVE_HOLD,
  SCORE_ONSET_DOT,
  SCORE_PX_PER_SEC,
  SCORE_QUIET_ROOM,
  SCORE_RULER,
  SCORE_STAVES_MAX,
  barHeight,
  baselineOf,
  clampView,
  clockLabel,
  fadeOf,
  forEachMeasure,
  lyricsFit,
  rowHeight,
  spanOf,
  stavesThatFit,
  xOfTime,
  type Lyric,
} from "../../lib/score";
import { Meter, Word } from "./words";

/// A swap costs the eye a jump — never more often than this (as in `Cast`).
const SWAP_EVERY = 2.5;
/// Loudest a peer can be made, matching `config::PEER_VOL_MAX_PCT`.
const PEER_VOL_MAX = 200;
/// ~30 fps: the score moves 36 px a second, so more frames buy nothing.
const FRAME_MS = 33;
/// Below this row height your acts sit beside your name instead of under it.
const ROW_TIGHT = 48;
/// How much a speaking name grows. Scale, never font-size: no reflow.
const NAME_SWING = 0.08;
/// Chat lines are prose, so they are set in a sans even in a mono room.
const LYRIC_FONT = "13px 'IBM Plex Sans', ui-sans-serif, system-ui, sans-serif";
/// Half a clock label at 11px mono, with a little air.
const RULER_HALF_LABEL = 32;
/// Someone off a staff who spoke or wrote this recently is brought up —
/// long enough to outlast the swap limit, so a line is never missed.
const LIFT_WITHIN = 10;
/// Space between "back to now" and the paused clock beside it.
const BACK_GAP = 12;
/// Gap kept between a chat line and the newer one to its right.
const LYRIC_GAP = 10;
const DASH = [6, 6];
const SOLID: number[] = [];

type Palette = {
  bg: string;
  text: string;
  text2: string;
  muted: string;
  faint: string;
  line: string;
  line2: string;
  sat: string;
  lum: string;
  rulerFont: string;
  liveFont: string;
  pausedFont: string;
};

export default function Score() {
  const me = () => state.status?.name ?? null;

  /// A DM is a room for two, as in `Cast`.
  const roster = () => {
    if (!state.dm) return state.participants;
    const mine = me();
    return mine ? [mine, state.dm.name] : [state.dm.name];
  };
  const anchor = () => me() ?? (state.dm ? state.dm.name : null);
  const showAll = () => tweaks.crowd === "all";
  const volumeOf = (name: string) => state.peerVolumes[name] ?? 100;
  const tunable = (name: string) => name !== me() && !state.dm;
  const mutedMe = () => !state.dm && !!state.status?.muted;

  const [standing, setStanding] = createSignal<string[]>([]);
  const [height, setHeight] = createSignal(480);
  /// The view time while scrolled back; null is live.
  const [paused, setPaused] = createSignal<number | null>(null);
  const [tuning, setTuning] = createSignal<string | null>(null);
  let lastSwap = 0;

  /// Never heard sorts as oldest, without NaN from `-Infinity - -Infinity`.
  /// A chat line counts: a staff is also where your words are written.
  const heardAt = (name: string) => {
    const at = score.lastHeard(name);
    return at === -Infinity ? 0 : at;
  };

  // Keep the staves in step with the room; free places go to recent voices.
  // Even "show everyone" stops at what fits: past that the rest wait in the
  // quiet line, which is always reachable, instead of under the input line.
  createEffect(() => {
    const all = roster();
    const mine = me();
    const fit = stavesThatFit(height() - SCORE_RULER, all.length);
    const cap = showAll() ? fit : Math.min(SCORE_STAVES_MAX, fit);
    setStanding((prev) => {
      const kept = prev.filter((n) => all.includes(n));
      // You are always on a staff — you can't be crowded out of your own call.
      if (mine && all.includes(mine) && !kept.includes(mine)) kept.unshift(mine);
      const rest = all.filter((n) => !kept.includes(n)).sort((a, b) => heardAt(b) - heardAt(a));
      for (const n of rest) {
        if (kept.length >= cap) break;
        kept.push(n);
      }
      while (kept.length > Math.max(cap, 1)) {
        let victim = -1;
        for (let i = 0; i < kept.length; i++) {
          if (kept[i] === mine) continue;
          if (victim === -1 || heardAt(kept[i]) < heardAt(kept[victim])) victim = i;
        }
        if (victim === -1) break;
        kept.splice(victim, 1);
      }
      return kept;
    });
  });

  createEffect(() => {
    const who = tuning();
    if (who && !standing().includes(who)) setTuning(null);
  });

  /// Someone quiet takes the staff of whoever has been silent longest —
  /// unless that one was heard more recently, or the swap would only churn.
  /// A click always lifts, however recent the last swap; automatic lifts
  /// still wait for the limit, counted from the click too.
  const lift = (name: string, now: number, force = false): boolean => {
    if (!force && now - lastSwap < SWAP_EVERY) return false;
    const current = standing();
    let victim: string | null = null;
    let oldest = Infinity;
    for (const n of current) {
      if (n === me()) continue;
      const at = heardAt(n);
      if (at < oldest) {
        oldest = at;
        victim = n;
      }
    }
    if (!victim || (!force && heardAt(name) <= oldest)) return false;
    lastSwap = now;
    setStanding(current.map((n) => (n === victim ? name : n)));
    return true;
  };

  const quiet = createMemo(() => roster().filter((n) => !standing().includes(n)));
  const rowH = createMemo(() =>
    rowHeight(height() - SCORE_RULER - (quiet().length ? SCORE_QUIET_ROOM : 0), standing().length),
  );
  const below = () => SCORE_RULER + standing().length * rowH() + 4;
  const tight = () => rowH() < ROW_TIGHT;
  const rowOf = createMemo(() => new Map(standing().map((n, i) => [n, i])));
  const nowLabel = createMemo(() => {
    const at = paused();
    return at === null ? t("score.now") : `${clockLabel(at)} · ${t("score.paused")}`;
  });

  const nodes = new Map<string, HTMLElement>();
  /// Whether each name is drawn as speaking, so the class is only touched
  /// when that changes.
  const lit = new Map<string, boolean>();
  const bind = (name: string) => (el: HTMLElement) => {
    nodes.set(name, el);
    // A fresh button (someone rejoining) starts unlit, whatever the old one was.
    lit.delete(name);
    onCleanup(() => {
      if (nodes.get(name) === el) nodes.delete(name);
    });
  };

  let sheet: HTMLDivElement | undefined;
  let canvas: HTMLCanvasElement | undefined;
  let back: HTMLDivElement | undefined;
  /// Paused frames only repaint when something they show has changed.
  let dirty = true;
  let kick = () => {
    dirty = true;
  };

  createEffect(
    on([standing, rowH, nowLabel], () => kick(), { defer: true }),
  );

  onMount(() => {
    if (!sheet || !canvas) return;
    const cv = canvas;
    const ctx = cv.getContext("2d");
    if (!ctx) return;
    const reduced = matchMedia("(prefers-reduced-motion: reduce)").matches;

    let W = 0;
    let H = 0;
    let dpr = 1;
    const fit = () => {
      dpr = window.devicePixelRatio || 1;
      cv.width = Math.max(1, Math.round(W * dpr));
      cv.height = Math.max(1, Math.round(H * dpr));
      dirty = true;
    };
    const ro = new ResizeObserver(([e]) => {
      W = e.contentRect.width;
      H = e.contentRect.height;
      setHeight(H);
      fit();
      if (reduced) paint();
    });
    ro.observe(sheet);
    onCleanup(() => ro.disconnect());

    // Canvas can't read var(): resolve the tokens to concrete colours, and
    // again whenever the theme or the typeface moves them.
    let pal: Palette;
    const tints = new Map<string, string>();
    const readPalette = () => {
      const cs = getComputedStyle(document.documentElement);
      const v = (k: string) => cs.getPropertyValue(k).trim();
      const font = v("--font-ui") || "monospace";
      pal = {
        bg: v("--c-bg"),
        text: v("--c-text"),
        text2: v("--c-text2"),
        muted: v("--c-muted"),
        faint: v("--c-faint"),
        line: v("--c-line"),
        line2: v("--c-line2"),
        sat: v("--sat"),
        lum: v("--lum"),
        rulerFont: `11px ${font}`,
        liveFont: `500 11px ${font}`,
        pausedFont: `700 11px ${font}`,
      };
      tints.clear();
      dirty = true;
    };
    readPalette();
    createEffect(
      on(
        () => [tweaks.theme, tweaks.typeface],
        () =>
          // installTheme writes the attributes in its own effect; read after it.
          queueMicrotask(() => {
            readPalette();
            if (reduced) paint();
          }),
        { defer: true },
      ),
    );
    const colorOf = (name: string) => {
      let c = tints.get(name);
      if (!c) {
        c = `hsl(${hueOf(name)}, ${pal.sat}, ${pal.lum})`;
        tints.set(name, c);
      }
      return c;
    };

    const labels = new Map<number, string>();
    const labelOf = (at: number) => {
      let l = labels.get(at);
      if (!l) {
        if (labels.size > 64) labels.clear();
        l = clockLabel(at);
        labels.set(at, l);
      }
      return l;
    };

    // Text widths are measured once per line; a late web font changes them.
    let widths = new WeakMap<Lyric, number>();
    const fontsDone = () => {
      widths = new WeakMap();
      kick();
    };
    document.fonts?.addEventListener?.("loadingdone", fontsDone);
    onCleanup(() => document.fonts?.removeEventListener?.("loadingdone", fontsDone));

    const levels = new Map<string, number>();
    let rowLimit = new Float64Array(SCORE_STAVES_MAX);

    // Per-frame state shared with the drawing callbacks below, so the loop
    // allocates no closures.
    let view = 0;
    let span = 0;
    let top = 0;
    let base = 0;
    let rh = 0;
    let lyricsOn = false;
    let mid = 0;

    /// Where the right-hand label starts; clocks stop short of it.
    let rulerEnd = 0;
    const drawRuler = (at: number, x: number) => {
      if (x + RULER_HALF_LABEL < rulerEnd) ctx.fillText(labelOf(at), x, SCORE_RULER - 7);
    };
    const drawMeasure = (_at: number, x: number) => ctx.fillRect(Math.round(x), top, 1, rh);
    const drawSample = (at: number, level: number, onset: boolean) => {
      const x = xOfTime(at, view, W);
      if (x < -SCORE_ONSET_DOT) return;
      ctx.globalAlpha = fadeOf(view - at, span);
      if (onset) {
        ctx.beginPath();
        ctx.arc(x, base, SCORE_ONSET_DOT / 2, 0, Math.PI * 2);
        ctx.fill();
      } else {
        const h = barHeight(level, rh);
        ctx.fillRect(x - SCORE_BAR_WIDTH / 2, base - h / 2, SCORE_BAR_WIDTH, h);
      }
    };
    const drawLyric = (l: Lyric) => {
      const row = rowOf().get(l.who);
      if (row === undefined) return;
      const x = xOfTime(l.t, view, W);
      const limit = rowLimit[row];
      if (x <= 0 || limit <= 0) return;
      let w = widths.get(l);
      if (w === undefined) {
        w = ctx.measureText(l.said).width;
        widths.set(l, w);
      }
      const age = view - l.t;
      ctx.globalAlpha = fadeOf(age, span) * Math.min(1, 0.25 + age * 2);
      const y = SCORE_RULER + row * rh + mid + 11;
      // A newer line wins: an older one is cut where the newer one starts.
      ctx.save();
      ctx.beginPath();
      ctx.rect(0, y - 2, limit, rh);
      ctx.clip();
      ctx.fillText(l.said, x, y);
      ctx.restore();
      rowLimit[row] = x - w - LYRIC_GAP;
    };

    const paint = () => {
      if (!W || !H) return;
      if ((window.devicePixelRatio || 1) !== dpr) fit();
      dirty = false;
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.clearRect(0, 0, W, H);
      const now = Date.now() / 1000;
      const at = paused();
      view = at ?? now;
      span = spanOf(W);
      rh = rowH();
      mid = baselineOf(rh);
      lyricsOn = lyricsFit(rh);
      const names = standing();
      const cursor = W - SCORE_CURSOR_INSET;
      const bottom = Math.min(H, SCORE_RULER + names.length * rh);

      // Ruler: now (or where you are) at the right, a clock per measure.
      ctx.globalAlpha = 1;
      ctx.textBaseline = "alphabetic";
      const label = nowLabel();
      ctx.font = at === null ? pal.liveFont : pal.pausedFont;
      // Paused, "back to now" sits at the right of the band (DOM, a real
      // button) and the clock stands just left of it.
      const labelEnd = at !== null && back ? W - back.offsetWidth - BACK_GAP : W;
      rulerEnd = labelEnd - ctx.measureText(label).width - 8;
      ctx.textAlign = "right";
      ctx.fillStyle = at === null ? pal.text2 : pal.text;
      ctx.fillText(label, labelEnd, SCORE_RULER - 7);
      ctx.font = pal.rulerFont;
      ctx.textAlign = "center";
      ctx.fillStyle = pal.muted;
      forEachMeasure(view, W, drawRuler);

      for (let i = 0; i < names.length; i++) {
        const name = names[i];
        top = SCORE_RULER + i * rh;
        base = top + mid + 0.5;
        if (top >= H) break;

        ctx.globalAlpha = 1;
        ctx.fillStyle = pal.line2;
        forEachMeasure(view, W, drawMeasure);

        // The staff; yours is broken while your microphone is off.
        const hushed = name === me() && mutedMe();
        ctx.strokeStyle = hushed ? pal.faint : pal.line;
        ctx.lineWidth = 1;
        ctx.setLineDash(hushed ? DASH : SOLID);
        ctx.beginPath();
        ctx.moveTo(0, base);
        ctx.lineTo(cursor, base);
        ctx.stroke();

        ctx.fillStyle = colorOf(name);
        score.forEachInWindow(name, view - span - 1, view, drawSample);
      }
      ctx.setLineDash(SOLID);

      if (lyricsOn) {
        if (rowLimit.length < names.length) rowLimit = new Float64Array(names.length);
        rowLimit.fill(W);
        ctx.font = LYRIC_FONT;
        ctx.textAlign = "right";
        ctx.textBaseline = "top";
        ctx.fillStyle = pal.text2;
        score.lyricsInWindow(view - span, view, drawLyric);
      }

      ctx.globalAlpha = 1;
      ctx.fillStyle = pal.faint;
      ctx.fillRect(Math.round(cursor), SCORE_RULER, 1, bottom - SCORE_RULER);

      // A note head on the cursor for whoever is speaking now.
      if (at === null) {
        for (let i = 0; i < names.length; i++) {
          const name = names[i];
          if (now - score.lastAudible(name) >= SCORE_LIVE_HOLD) continue;
          const level = levels.get(name) ?? levelNow(name);
          ctx.fillStyle = colorOf(name);
          ctx.beginPath();
          ctx.arc(cursor, SCORE_RULER + i * rh + mid + 0.5, 5.5 * (0.6 + level * 0.6), 0, Math.PI * 2);
          ctx.fill();
        }
      }
    };

    let last = performance.now();
    const step = (frameAt: number) => {
      const dt = Math.min(0.1, (frameAt - last) / 1000);
      last = frameAt;
      const now = Date.now() / 1000;

      for (const [name, el] of nodes) {
        const goal = levelNow(name);
        const cur = levels.get(name) ?? 0;
        const next = reduced ? goal : cur + (goal - cur) * Math.min(1, dt * 8);
        levels.set(name, next);
        if (!reduced) el.style.transform = `scale(${(1 + next * NAME_SWING).toFixed(3)})`;
        const live = goal > SCORE_AUDIBLE || now - score.lastAudible(name) < SCORE_LIVE_HOLD;
        if (lit.get(name) !== live) {
          lit.set(name, live);
          el.classList.toggle("score-live", live);
        }
      }

      for (const name of quiet()) {
        const heard = levelNow(name) > SCORE_AUDIBLE || now - score.lastHeard(name) < LIFT_WITHIN;
        if (heard && lift(name, frameAt / 1000)) break;
      }

      // History moves on under a paused view; don't let it point at nothing.
      const at = paused();
      if (at !== null && at < now - SCORE_HISTORY_SECS + spanOf(W)) {
        setPaused(clampView(at, now, spanOf(W), score.since()));
      }

      if (paused() === null || dirty) paint();
    };

    let raf = 0;
    let lastPaint = 0;
    const frame = (frameAt: number) => {
      raf = 0;
      if (document.hidden) return;
      raf = requestAnimationFrame(frame);
      if (frameAt - lastPaint < FRAME_MS) return;
      lastPaint = frameAt;
      step(frameAt);
    };

    if (reduced) {
      // Still a record of who spoke, just not a moving one.
      kick = () => {
        dirty = true;
        paint();
      };
      const timer = window.setInterval(() => {
        if (!document.hidden) step(performance.now());
      }, 1000);
      onCleanup(() => window.clearInterval(timer));
      queueMicrotask(() => step(performance.now()));
    } else {
      const wake = () => {
        if (!document.hidden && !raf) raf = requestAnimationFrame(frame);
      };
      document.addEventListener("visibilitychange", wake);
      raf = requestAnimationFrame(frame);
      onCleanup(() => {
        document.removeEventListener("visibilitychange", wake);
        if (raf) cancelAnimationFrame(raf);
      });
    }

    // Scrolling back in time: either axis, so a mouse wheel and a trackpad
    // swipe both work. Not passive — the page must not scroll instead.
    const wheel = (e: WheelEvent) => {
      const unit = e.deltaMode === 1 ? 16 : e.deltaMode === 2 ? H : 1;
      const d = (Math.abs(e.deltaX) > Math.abs(e.deltaY) ? -e.deltaX : e.deltaY) * unit;
      if (!d) return;
      e.preventDefault();
      const now = Date.now() / 1000;
      const cur = paused() ?? now;
      setPaused(clampView(cur - d / SCORE_PX_PER_SEC, now, spanOf(W), score.since()));
      kick();
    };
    sheet.addEventListener("wheel", wheel, { passive: false });
    onCleanup(() => sheet?.removeEventListener("wheel", wheel));
  });

  const resume = () => {
    setPaused(null);
    kick();
  };

  return (
    <div class="score">
      {/* The sheet stays mounted with nobody on it: the loop and its observers
          belong to the score, not to whoever happens to be in the room. */}
      <Show when={!roster().length}>
        <div class="note">{t("room.alone")}</div>
      </Show>
      <div class="score-body">
        <div class="score-names" style={{ "padding-top": `${SCORE_RULER}px` }}>
          <For each={standing()}>
            {(name) => (
              <div
                class="score-person"
                classList={{ "score-tight": tight() }}
                style={{
                  height: `${rowH()}px`,
                  "padding-top": `${Math.max(0, baselineOf(rowH()) - 10)}px`,
                }}
              >
                <button
                  ref={bind(name)}
                  type="button"
                  class="score-name"
                  classList={{ "score-hushed": name === me() && mutedMe() }}
                  style={{ color: tint(name) }}
                  title={name === me() ? t("room.you") : undefined}
                  aria-expanded={tunable(name) ? tuning() === name : undefined}
                  onClick={() => {
                    if (!tunable(name)) return;
                    setTuning(tuning() === name ? null : name);
                  }}
                >
                  {name}
                  <Show when={name === me()}>
                    <span class="name-tail">{t("room.you_tail")}</span>
                  </Show>
                  <Show when={name !== me() && volumeOf(name) !== 100}>
                    <span class="name-mark" aria-hidden="true">
                      °
                    </span>
                  </Show>
                </button>
                <Show when={tuning() === name}>
                  <div class="score-tune">
                    <Meter
                      label={t("room.tune")}
                      value={volumeOf(name)}
                      max={PEER_VOL_MAX}
                      unit="%"
                      set={(v) => void setPeerVolume(name, v)}
                    />
                  </div>
                </Show>
                <Show when={name === anchor()}>
                  <div class="score-acts">
                    {/* No microphone in a DM — it's a written room. */}
                    <Show when={!state.dm}>
                      <Word tone="quiet" onClick={() => toggleMute()}>
                        {state.status?.muted ? t("room.speak") : t("room.hush")}
                      </Word>
                    </Show>
                    <Word tone="danger" onClick={() => (state.dm ? closeDm() : leaveChannel())}>
                      {t("room.leave")}
                    </Word>
                  </div>
                </Show>
              </div>
            )}
          </For>
        </div>
        <div class="score-sheet" ref={sheet}>
          <canvas class="score-canvas" ref={canvas} aria-hidden="true" />
          <Show when={paused() !== null}>
            <div class="score-back" ref={back} style={{ height: `${SCORE_RULER}px` }}>
              <Word tone="loud" onClick={resume}>
                {t("score.back")}
              </Word>
            </div>
          </Show>
        </div>
        <Show when={quiet().length}>
          {/* Right under the last staff, wherever that ends. */}
          <div
            class="score-quiet"
            style={{ top: `${below()}px`, "max-height": `${Math.max(0, height() - below())}px` }}
          >
            <span class="score-quiet-label">{t("room.silent", { count: quiet().length })}</span>
            <span class="score-quiet-names">
              <For each={quiet()}>
                {(name) => (
                  <Word
                    tone="quiet"
                    style={{ color: tint(name), opacity: 0.55 }}
                    title={t("room.lift")}
                    onClick={() => lift(name, performance.now() / 1000, true)}
                  >
                    {name}
                  </Word>
                )}
              </For>
            </span>
          </div>
        </Show>
      </div>
    </div>
  );
}
