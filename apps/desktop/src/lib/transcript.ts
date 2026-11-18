// What the session interface shows in a room: what was said and what was
// written, in the order it happened. A reactive list, fed by the same effects
// that feed the score (`lib/scoreFeed.ts`) — there is one reading of the voice
// state in this app, not one per view.
//
// Nothing is recorded unless the session interface is the one in use: a store
// write per speaker per payload is not free, and the other interface never
// reads it.

import { createStore, produce } from "solid-js/store";
import { tweaks } from "./theme";
import { TurnTracker } from "./turns";

export type Row =
  | { id: number; kind: "text"; who: string; text: string; t: number; mine: boolean }
  | { id: number; kind: "voice"; who: string; t: number; end: number; live: boolean }
  | { id: number; kind: "event"; text: string; t: number; bad: boolean };

/// Rows kept. The log itself keeps 500 lines; voice adds its own on top.
const ROWS_MAX = 600;

const [rows, setRows] = createStore<Row[]>([]);
export { rows };

const turns = new TurnTracker();
/// Open turn → the row drawing it.
const live = new Map<string, number>();
let nextId = 1;

const on = () => tweaks.layout === "session";

function push(row: Row) {
  setRows(
    produce((l) => {
      l.push(row);
      if (l.length > ROWS_MAX) l.splice(0, l.length - ROWS_MAX);
    }),
  );
}

const indexOf = (id: number) => {
  for (let i = rows.length - 1; i >= 0; i--) if (rows[i].id === id) return i;
  return -1;
};

export const transcript = {
  /// One audible sample of `who` at `t` (unix seconds).
  hear(who: string, t: number) {
    if (!on()) return;
    const opened = turns.hear(who, t);
    if (opened) {
      const id = nextId++;
      live.set(who, id);
      push({ id, kind: "voice", who, t, end: t, live: true });
      return;
    }
    const id = live.get(who);
    const i = id === undefined ? -1 : indexOf(id);
    if (i !== -1) setRows(i, produce((r) => void (r.kind === "voice" && (r.end = t))));
  },

  /// Close the turns that have gone quiet. Called once per voice payload.
  sweep(now: number) {
    turns.sweep(now, (turn, kept) => {
      const id = live.get(turn.who);
      live.delete(turn.who);
      const i = id === undefined ? -1 : indexOf(id);
      if (i === -1) return;
      if (kept) setRows(i, produce((r) => void (r.kind === "voice" && (r.live = false))));
      else setRows(produce((l) => void l.splice(i, 1)));
    });
  },

  text(who: string, text: string, t: number, mine: boolean) {
    if (on()) push({ id: nextId++, kind: "text", who, text, t, mine });
  },

  event(text: string, t: number, bad: boolean) {
    if (on()) push({ id: nextId++, kind: "event", text, t, bad });
  },

  rename(from: string, to: string) {
    turns.rename(from, to);
    const id = live.get(from);
    if (id !== undefined) {
      live.delete(from);
      live.set(to, id);
    }
    setRows(
      produce((l) => {
        for (const r of l) if (r.kind !== "event" && r.who === from) r.who = to;
      }),
    );
  },

  clear() {
    turns.clear();
    live.clear();
    setRows([]);
  },
};
