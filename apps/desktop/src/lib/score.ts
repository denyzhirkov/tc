// The score: the conversation over time. One staff per person, loudness as
// bars scrolling right→left, a dot where a talkspurt starts, chat lines
// written under the sender's staff at the moment they were sent.
//
// Pure on purpose — no Solid, no DOM, no other app module — so node can test
// it with nothing but type stripping. The recorder is fed by
// `lib/scoreFeed.ts`; the painter is `components/room/Score.tsx`.
// Only syntax node's type stripping understands lives here: no enums,
// namespaces or parameter properties.

/// How much of the conversation is kept, and how far back the wheel can go.
export const SCORE_HISTORY_SECS = 120;
/// Quietest level that counts as a voice. Below it nothing is stored. On the
/// room's gated scale (`lib/loudness.ts`) this is ≈ −36.5 dBFS raw — the
/// sender's VAD threshold, where the old packet-size proxy put it too.
export const SCORE_AUDIBLE = 0.08;
/// Silence longer than this before an audible sample makes it an onset.
export const SCORE_ONSET_GAP = 0.6;
/// The fastest rate a voice is stored at. `level_pump` emits at 5 Hz; the
/// headroom absorbs jitter, and anything faster is thinned so the ring always
/// covers the whole history.
export const SCORE_RECORD_HZ_MAX = 8;
/// Slots per voice: the whole history at the fastest stored rate.
export const SCORE_RING = Math.ceil(SCORE_HISTORY_SECS * SCORE_RECORD_HZ_MAX);
/// Most chat lines kept, whatever their age — a flood must not grow memory.
export const SCORE_LYRICS_MAX = 240;

/// Scroll speed. At 5 Hz this puts a bar every ~7 px, so 3 px bars read as
/// a rhythm rather than a solid block.
export const SCORE_PX_PER_SEC = 36;
/// Gap between the newest sample and the right edge.
export const SCORE_PAD = 24;
/// The "now" cursor, from the right edge.
export const SCORE_CURSOR_INSET = 14;
/// Measure length, in seconds.
export const SCORE_MEASURE_SECS = 10;
/// Measures closer than this to the right edge are skipped, so their label
/// never collides with "now".
export const SCORE_RULER_CLEAR = 70;
/// Height of the clock band over the staves.
export const SCORE_RULER = 26;
/// Row height bounds. Below the minimum a name no longer fits beside its acts.
export const SCORE_ROW_MIN = 34;
export const SCORE_ROW_MAX = 72;
/// A row this tall has room for a line of text under its staff.
export const SCORE_LYRIC_ROW_MIN = 52;
/// Where the staff sits in a row that carries lyrics (the prototype's 30/68).
export const SCORE_BASELINE_FRAC = 0.44;
/// The loudest bar is this share of the row height.
export const SCORE_BAR_SCALE = 0.65;
export const SCORE_BAR_MIN = 2;
export const SCORE_BAR_WIDTH = 3;
export const SCORE_ONSET_DOT = 9;
/// Opacity at the left edge; older content fades toward it.
export const SCORE_FADE_MIN = 0.45;
/// A scroll that lands this close to now is a return to live.
export const SCORE_RESUME_EPS = 0.05;
/// A voice heard this recently still counts as speaking — samples are 200 ms
/// apart, and a name that blinked between them would read as a fault.
export const SCORE_LIVE_HOLD = 0.45;
/// Most staves standing at once; the rest wait in the quiet line.
export const SCORE_STAVES_MAX = 8;
/// Room kept under the staves for the quiet line, when there is one.
export const SCORE_QUIET_ROOM = 34;

const MIN_STEP = 1 / SCORE_RECORD_HZ_MAX;

class Ring {
  times = new Float64Array(SCORE_RING);
  levels = new Float32Array(SCORE_RING);
  onsets = new Uint8Array(SCORE_RING);
  /// Index the next sample is written to.
  head = 0;
  size = 0;
  lastAudible = -Infinity;
}

export type Lyric = { who: string; said: string; t: number };

export class ScoreRecorder {
  private rings = new Map<string, Ring>();
  private lyrics: Lyric[] = [];
  /// When each name last wrote, kept apart from the list so a lookup is O(1).
  private said = new Map<string, number>();
  /// Newest time seen by anything. History is measured back from here, so
  /// what scrolled out of it is gone even while its slot is still unwritten.
  private latest = -Infinity;
  /// Oldest time anything was stored at since the last clear.
  private oldest = Infinity;

  /// Store one level. Silence is never stored — its absence is the silence.
  record(name: string, level: number, t: number): void {
    if (t > this.latest) this.latest = t;
    if (!(level >= SCORE_AUDIBLE)) return;
    let ring = this.rings.get(name);
    if (!ring) {
      ring = new Ring();
      this.rings.set(name, ring);
    }
    // Out of order (a clock step back) or faster than the ring was sized
    // for: dropping it keeps the ring sorted and the history whole.
    if (t - ring.lastAudible < MIN_STEP) return;
    const onset = t - ring.lastAudible > SCORE_ONSET_GAP;
    ring.times[ring.head] = t;
    ring.levels[ring.head] = Math.min(1, level);
    ring.onsets[ring.head] = onset ? 1 : 0;
    ring.head = (ring.head + 1) % SCORE_RING;
    if (ring.size < SCORE_RING) ring.size++;
    ring.lastAudible = t;
    if (t < this.oldest) this.oldest = t;
  }

  /// Visit `name`'s samples with `from <= t <= to`, newest first.
  forEachInWindow(
    name: string,
    from: number,
    to: number,
    fn: (t: number, level: number, onset: boolean) => void,
  ): void {
    const ring = this.rings.get(name);
    if (!ring) return;
    const lo = Math.max(from, this.latest - SCORE_HISTORY_SECS);
    for (let k = 1; k <= ring.size; k++) {
      const i = (ring.head - k + SCORE_RING) % SCORE_RING;
      const t = ring.times[i];
      if (t < lo) return;
      if (t > to) continue;
      fn(t, ring.levels[i], ring.onsets[i] === 1);
    }
  }

  /// When `name` was last heard, or -Infinity.
  lastAudible(name: string): number {
    return this.rings.get(name)?.lastAudible ?? -Infinity;
  }

  /// When `name` was last heard or read — a chat line counts as speaking
  /// up — or -Infinity.
  lastHeard(name: string): number {
    return Math.max(this.lastAudible(name), this.said.get(name) ?? -Infinity);
  }

  /// Oldest stored time still inside the history, or Infinity when empty.
  since(): number {
    if (this.oldest === Infinity) return Infinity;
    return Math.max(this.oldest, this.latest - SCORE_HISTORY_SECS);
  }

  /// A renamed peer keeps what they said under their new name.
  rename(from: string, to: string): void {
    if (from === to) return;
    const ring = this.rings.get(from);
    if (ring) {
      this.rings.delete(from);
      if (!this.rings.has(to)) this.rings.set(to, ring);
    }
    for (const l of this.lyrics) if (l.who === from) l.who = to;
    const said = this.said.get(from);
    if (said !== undefined) {
      this.said.delete(from);
      this.said.set(to, Math.max(said, this.said.get(to) ?? -Infinity));
    }
  }

  clear(): void {
    this.rings.clear();
    this.lyrics = [];
    this.said.clear();
    this.latest = -Infinity;
    this.oldest = Infinity;
  }

  /// A chat line, at the moment it was said. Lines older than the history
  /// (a replay of stored chat) are not kept.
  addLyric(who: string, said: string, t: number, now: number): void {
    if (now > this.latest) this.latest = now;
    const cut = this.latest - SCORE_HISTORY_SECS;
    if (t < cut) return;
    const list = this.lyrics;
    // Usually an append; a replay can arrive out of order.
    let i = list.length;
    while (i > 0 && list[i - 1].t > t) i--;
    list.splice(i, 0, { who, said, t });
    let drop = 0;
    while (drop < list.length && list[drop].t < cut) drop++;
    drop = Math.max(drop, list.length - SCORE_LYRICS_MAX);
    if (drop > 0) list.splice(0, drop);
    if (t < this.oldest) this.oldest = t;
    if (t > (this.said.get(who) ?? -Infinity)) this.said.set(who, t);
  }

  /// Visit lines with `from <= t <= to`, newest first.
  lyricsInWindow(from: number, to: number, fn: (l: Lyric) => void): void {
    const lo = Math.max(from, this.latest - SCORE_HISTORY_SECS);
    for (let i = this.lyrics.length - 1; i >= 0; i--) {
      const l = this.lyrics[i];
      if (l.t < lo) return;
      if (l.t > to) continue;
      fn(l);
    }
  }
}

// ── geometry ────────────────────────────────────────────────────────────

/// Canvas x of time `t` when the right edge shows `view`.
export function xOfTime(
  t: number,
  view: number,
  width: number,
  pxPerSec = SCORE_PX_PER_SEC,
  pad = SCORE_PAD,
): number {
  return width - pad - (view - t) * pxPerSec;
}

/// How many seconds fit between the left edge and the newest sample.
export function spanOf(width: number, pxPerSec = SCORE_PX_PER_SEC, pad = SCORE_PAD): number {
  return Math.max(0, (width - pad) / pxPerSec);
}

/// Whether something at `x` (with `reach` px either side) lands on the canvas.
export function onCanvas(x: number, width: number, reach = 0): boolean {
  return x + reach >= 0 && x - reach <= width;
}

export function barHeight(level: number, rowH: number): number {
  const l = Math.max(0, Math.min(1, level));
  return Math.max(SCORE_BAR_MIN, l * SCORE_BAR_SCALE * rowH);
}

export function rowHeight(available: number, rows: number): number {
  const per = rows > 0 ? available / rows : SCORE_ROW_MAX;
  return Math.max(SCORE_ROW_MIN, Math.min(SCORE_ROW_MAX, per));
}

/// How many staves stand in `available` px for `count` people. Everyone, if
/// they fit at the minimum row height; otherwise as many as fit above the
/// quiet line, which takes the rest. Never fewer than one — yours.
export function stavesThatFit(available: number, count: number): number {
  if (count * SCORE_ROW_MIN <= available) return count;
  return Math.max(1, Math.min(count, Math.floor((available - SCORE_QUIET_ROOM) / SCORE_ROW_MIN)));
}

export function lyricsFit(rowH: number): boolean {
  return rowH >= SCORE_LYRIC_ROW_MIN;
}

/// The staff's y inside its row: centred when the row is only a staff,
/// raised to leave a line under it when it carries lyrics.
export function baselineOf(rowH: number): number {
  return lyricsFit(rowH) ? Math.round(rowH * SCORE_BASELINE_FRAC) : Math.round(rowH / 2);
}

/// Opacity for something `age` seconds behind the right edge.
export function fadeOf(age: number, span: number): number {
  if (span <= 0) return 1;
  return 1 - Math.min(1, Math.max(0, age / span)) * (1 - SCORE_FADE_MIN);
}

/// Visit every measure mark on the canvas, newest first, culled to the
/// width and kept clear of the "now" label at the right.
export function forEachMeasure(
  view: number,
  width: number,
  fn: (t: number, x: number) => void,
  pxPerSec = SCORE_PX_PER_SEC,
  pad = SCORE_PAD,
): void {
  const first = Math.floor(view / SCORE_MEASURE_SECS) * SCORE_MEASURE_SECS;
  for (let t = first; ; t -= SCORE_MEASURE_SECS) {
    const x = xOfTime(t, view, width, pxPerSec, pad);
    if (x < 0) return;
    if (width - x < SCORE_RULER_CLEAR) continue;
    fn(t, x);
  }
}

const two = (n: number) => (n < 10 ? "0" : "") + n;

/// HH:MM:SS of a unix time, in local time.
export function clockLabel(unixSecs: number): string {
  const d = new Date(unixSecs * 1000);
  return `${two(d.getHours())}:${two(d.getMinutes())}:${two(d.getSeconds())}`;
}

/// Where a scroll to `next` lands: a paused view time, or null for live.
/// The left edge never goes past what was recorded.
export function clampView(next: number, now: number, span: number, since: number): number | null {
  if (next >= now - SCORE_RESUME_EPS) return null;
  const floor = Math.max(now - SCORE_HISTORY_SECS, since === Infinity ? now : since) + span;
  if (floor >= now - SCORE_RESUME_EPS) return null;
  return Math.max(floor, next);
}
