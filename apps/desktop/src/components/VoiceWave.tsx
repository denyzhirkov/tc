// Voice-activity wave for the sidebar — a strip of thin vertical bars whose
// heights ride a slowly-shifting sine envelope. Glued to the bottom edge of
// the channel pill. Amplitude scales with the speaker level.

import { For, createSignal, onCleanup, onMount } from "solid-js";

const BARS = 36;
const MAX_H = 10; // max bar height (px)
const MIN_H = 1;

export default function VoiceWave(props: { level: number }) {
  const [phase, setPhase] = createSignal(0);

  onMount(() => {
    const id = setInterval(() => setPhase((p) => p + 0.35), 80);
    onCleanup(() => clearInterval(id));
  });

  // Two superimposed sines with different frequencies feel less mechanical
  // than a single one. Bar i: amplitude in [0..1].
  const amp = (i: number) => {
    const p = phase();
    const a = Math.sin(i * 0.55 + p) * 0.55 + Math.sin(i * 0.18 - p * 0.7) * 0.45;
    return (a + 1) / 2; // → 0..1
  };

  const heightOf = (i: number) => {
    // Compress: lift quiet input off the floor so it still reads as a wave.
    const lvl = Math.pow(Math.max(0, Math.min(1, props.level)), 0.6);
    const h = MIN_H + amp(i) * lvl * (MAX_H - MIN_H);
    return Math.max(MIN_H, h);
  };

  return (
    <span class="absolute left-2 right-2 bottom-0 pointer-events-none flex items-end justify-between gap-[1px]"
      style={{ height: `${MAX_H}px` }}
    >
      <For each={Array.from({ length: BARS }, (_, i) => i)}>
        {(i) => (
          <span
            class="block flex-1 rounded-[1px]"
            style={{
              height: `${heightOf(i)}px`,
              background: "var(--c-accent)",
              opacity: 0.55 + Math.pow(props.level, 0.5) * 0.45,
              transition: "height 80ms linear",
            }}
          />
        )}
      </For>
    </span>
  );
}
