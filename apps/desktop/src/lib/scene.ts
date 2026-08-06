// Which cast the single layout is currently showing. The app has one shape —
// a row of entities, details under them, a line to type into — and these are
// the four things that can stand in that row.

import { createSignal } from "solid-js";
import { state } from "./store";

export type SceneView = "room" | "lobby" | "settings" | "history";

const [view, setView] = createSignal<SceneView>("room");
export { view };

/// What's typed in the one input line while settings are open — with 28
/// controls, searching beats remembering which group something lives in.
const [query, setQuery] = createSignal("");
export { query, setQuery };

/// Text pushed into the input line by something the user clicked (editing a
/// nickname, starting a command). Consumed once by `Line`.
const [prefill, setPrefill] = createSignal<string | null>(null);
export { prefill };
export function fill(text: string) {
  setPrefill(text);
}
export function takeFill(): string | null {
  const v = prefill();
  if (v !== null) setPrefill(null);
  return v;
}

/// Where "back" goes: into the room if we're in one, otherwise the lobby.
export function ground(): SceneView {
  return state.channel || state.dm ? "room" : "lobby";
}

export function goto(v: SceneView) {
  if (v !== "settings") setQuery("");
  setView(v);
}

export function toggle(v: SceneView) {
  goto(view() === v ? ground() : v);
}

/// Called by wire/actions when the room itself changes underneath us, so a
/// join never leaves the user staring at the lobby.
export function followRoom() {
  if (view() === "lobby" && (state.channel || state.dm)) goto("room");
  if (view() === "room" && !state.channel && !state.dm) goto("lobby");
}
