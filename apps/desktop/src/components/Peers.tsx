// Right-hand participants panel. Voice activity is shown by an animated
// "running highlight" through the peer's fingerprint chars (instead of
// dedicated VU bars) — feels organic and ties identity to liveness.

import { Index, Show, createSignal, onCleanup, onMount } from "solid-js";
import { state, update } from "../lib/store";
import { fpFor } from "../lib/fp";
import { cmd } from "../lib/tauri";
import { Speaker, SpeakerOff } from "./Icons";
import Identicon from "./Identicon";

const SPEAK_THRESHOLD = 0.04;
// Chars per second the highlight advances at. Scales with level so loud
// speech runs faster than quiet speech.
const BASE_SPEED = 7;
const LEVEL_SPEED = 18;

function PeerRow(props: { name: string; level: number; self: boolean }) {
  const speaking = () => props.level > SPEAK_THRESHOLD;
  const fp = () =>
    props.self
      ? state.status?.fingerprint ?? fpFor(props.name)
      : fpFor(props.name);

  // Head position (in chars) of the running highlight. Advances over time
  // whenever the peer is above the speaking threshold; freezes otherwise.
  const [head, setHead] = createSignal(0);
  let raf = 0;
  let lastT = 0;

  const tick = (t: number) => {
    if (lastT === 0) lastT = t;
    const dt = (t - lastT) / 1000;
    lastT = t;
    const lvl = props.level;
    if (lvl > SPEAK_THRESHOLD) {
      const speed = BASE_SPEED + lvl * LEVEL_SPEED;
      const span = fp().length + 1;
      setHead((h) => (h + dt * speed) % span);
    }
    raf = requestAnimationFrame(tick);
  };

  onMount(() => {
    raf = requestAnimationFrame(tick);
  });
  onCleanup(() => cancelAnimationFrame(raf));

  // Local per-peer playback volume (percent, 100 = unchanged). Not shown for
  // self — you can't turn your own volume down for yourself.
  const vol = () => state.peerVolumes[props.name] ?? 100;
  const setVol = (pct: number) => {
    update.peerVolume(props.name, pct);
    cmd.setPeerVolume(props.name, pct).catch(() => {});
  };
  // Mute toggle: 0 ↔ 100 (boost levels are set via the slider).
  const toggleMute = () => setVol(vol() === 0 ? 100 : 0);
  // Show the control on hover, or always when it's been adjusted off 100%.
  const adjusted = () => vol() !== 100;

  return (
    <div class="group flex flex-col px-2 py-1.5 rounded hover:bg-hover">
      <div class="flex items-center gap-2">
        <span class={speaking() ? "text-accent" : "text-text2"}>
          <Identicon
            pubkeyHex={props.self ? state.status?.pubkey : props.name}
          />
        </span>
        <div class="flex-1 min-w-0 leading-tight">
          <div
            class={`text-sm truncate ${
              speaking() ? "text-text" : "text-text2"
            }`}
          >
            {props.name}
            <Show when={props.self}>
              <span class="text-faint text-xs"> · you</span>
            </Show>
          </div>
          <div class="text-[10px] font-mono tracking-wider flex">
            <Index each={[...fp()]}>
              {(ch, i) => {
                const lit = () => speaking() && i < head();
                return (
                  <span
                    class={`transition-colors duration-75 ${
                      lit() ? "text-accent" : "text-faint"
                    }`}
                  >
                    {ch()}
                  </span>
                );
              }}
            </Index>
          </div>
        </div>
        <Show when={!props.self && adjusted()}>
          <span class="text-[10px] tabular-nums text-muted shrink-0">
            {vol()}%
          </span>
        </Show>
      </div>
      <Show when={!props.self}>
        <div
          class="flex items-center gap-1.5 mt-1 px-0.5"
          classList={{
            "opacity-0 group-hover:opacity-100 transition-opacity": !adjusted(),
          }}
        >
          <button
            class="text-muted hover:text-accent shrink-0"
            title={vol() === 0 ? "unmute (local)" : "mute (local)"}
            onClick={toggleMute}
          >
            {vol() === 0 ? <SpeakerOff size={13} /> : <Speaker size={13} />}
          </button>
          <input
            type="range"
            min={0}
            max={200}
            step={5}
            value={vol()}
            class="flex-1 h-1 accent-accent cursor-pointer"
            title={`local volume: ${vol()}%`}
            onInput={(e) => setVol(parseInt(e.currentTarget.value, 10))}
          />
        </div>
      </Show>
    </div>
  );
}

export default function Peers() {
  const list = () => {
    const me = state.status?.name;
    return [...state.participants].sort((a, b) =>
      a === me ? -1 : b === me ? 1 : a.localeCompare(b),
    );
  };

  const levelOf = (name: string) => {
    if (state.status?.name === name) {
      return Math.min(1, (state.voice?.input_peak ?? 0) * 4);
    }
    return state.speakers[name] ?? 0;
  };

  return (
    <aside class="w-56 shrink-0 h-full border-l border-line bg-surface flex flex-col">
      <div class="px-3 h-14 flex items-center border-b border-line">
        <span class="text-[11px] uppercase tracking-wider text-muted">
          in call · {list().length}
        </span>
      </div>
      <div class="flex-1 overflow-y-auto p-1.5 flex flex-col gap-0.5">
        <Index each={list()}>
          {(p) => (
            <PeerRow
              name={p()}
              level={levelOf(p())}
              self={state.status?.name === p()}
            />
          )}
        </Index>
        <Show when={list().length === 0}>
          <div class="text-muted text-xs px-2 py-1">no peers</div>
        </Show>
      </div>
    </aside>
  );
}
