// Frosted-glass system toast — the shared shell for non-blocking notifications
// (update available, hot-plugged audio device, …). Translucent blurred panel
// with a circular accent icon chip and an accent glow that follows the theme.
// Visuals live in `.toast-card` (styles.css); this owns layout + slots.
// Positioning/stacking is owned by the `ToastStack` container in App.tsx.

import { JSX, Show } from "solid-js";

export type ToastAction = {
  label: string;
  onClick: () => void;
  /// Filled accent button (the call-to-action). Otherwise a quiet ghost button.
  primary?: boolean;
};

export default function Toast(props: {
  icon: JSX.Element;
  title: JSX.Element;
  body?: JSX.Element;
  actions: ToastAction[];
}) {
  return (
    <div class="w-[340px] toast-card p-4">
      <div class="flex gap-3">
        <div class="shrink-0 w-9 h-9 rounded-full bg-accent-bg text-accent flex items-center justify-center">
          {props.icon}
        </div>
        <div class="flex-1 min-w-0">
          <div class="text-sm font-medium text-text leading-tight">
            {props.title}
          </div>
          <Show when={props.body}>
            <div class="text-xs text-muted mt-1 leading-snug">{props.body}</div>
          </Show>
          <div class="flex justify-end gap-2 mt-3">
            {props.actions.map((a) => (
              <button
                class={
                  a.primary
                    ? "px-3 py-1 text-sm rounded-lg bg-accent text-white hover:opacity-90 transition-opacity"
                    : "px-3 py-1 text-sm rounded-lg border border-line text-text2 hover:bg-hover transition-colors"
                }
                onClick={a.onClick}
              >
                {a.label}
              </button>
            ))}
          </div>
        </div>
      </div>
    </div>
  );
}
