// Cuts a stream of loudness samples into turns of speech: "anna spoke for
// eleven seconds, starting 21:02". Pure — no Solid, no DOM — so node can test
// it. The session transcript (`lib/transcript.ts`) is its only caller.
//
// A voice arrives as samples only while it is audible; silence is the absence
// of samples. So a turn cannot be closed by its own next sample — it is closed
// by the clock, in `sweep`, once nothing has been heard for `TURN_GAP`.

/// Silence that ends a turn. Long enough to breathe between sentences without
/// the transcript splitting one thought into three rows.
export const TURN_GAP = 1.5;
/// A turn shorter than this is a cough or a key click, not something said.
export const TURN_MIN = 0.6;

export type Turn = { who: string; start: number; end: number };

export class TurnTracker {
  private open = new Map<string, Turn>();

  /// One audible sample. Returns the turn it opened, or null when it extended
  /// one already running. Callers filter silence out before calling.
  hear(who: string, t: number): Turn | null {
    const turn = this.open.get(who);
    if (turn) {
      turn.end = t;
      return null;
    }
    const next = { who, start: t, end: t };
    this.open.set(who, next);
    return next;
  }

  /// The turn `who` is in right now, if any.
  current(who: string): Turn | undefined {
    return this.open.get(who);
  }

  /// Close every turn that has been silent for `TURN_GAP` and hand them back.
  /// `kept` says whether it lasted long enough to be worth a row.
  sweep(now: number, closed: (turn: Turn, kept: boolean) => void): void {
    for (const [who, turn] of this.open) {
      if (now - turn.end < TURN_GAP) continue;
      this.open.delete(who);
      closed(turn, turn.end - turn.start >= TURN_MIN);
    }
  }

  rename(from: string, to: string): void {
    const turn = this.open.get(from);
    if (!turn) return;
    this.open.delete(from);
    turn.who = to;
    this.open.set(to, turn);
  }

  clear(): void {
    this.open.clear();
  }
}
