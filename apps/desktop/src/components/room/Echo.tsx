// The last thing the app said to you, over the input line, for a few seconds.
//
// Most commands answer in one line — "mic gain 120%", "unknown command",
// "forgot 10.0.0.2" — and those lines used to land in a log nobody was
// looking at. Switching the whole scene for a confirmation is too violent,
// so they surface here and fade. Click it to open the full log.

import { Show, createEffect, createSignal, onCleanup, onMount } from "solid-js";
import { state, type LogLine } from "../../lib/store";
import { goto, view } from "../../lib/scene";
import { t } from "../../lib/i18n";

const LIFE_MS = 6000;

export default function Echo() {
  const [shown, setShown] = createSignal<LogLine | null>(null);
  let seen = -1;
  let timer: number | undefined;

  onMount(() => {
    // Whatever is already in the log is history, not news.
    const l = state.log;
    seen = l.length ? l[l.length - 1].id : -1;
  });

  createEffect(() => {
    const l = state.log;
    if (!l.length) return;
    const last = l[l.length - 1];
    if (last.id <= seen) return;
    seen = last.id;
    // Chat belongs on the stage, dev logs are a firehose — neither echoes.
    if (last.kind !== "system" && last.kind !== "error") return;
    setShown(last);
    window.clearTimeout(timer);
    timer = window.setTimeout(() => setShown(null), LIFE_MS);
  });

  onCleanup(() => window.clearTimeout(timer));

  return (
    <Show when={view() !== "history" && shown()}>
      {(line) => (
        <button
          class="echo"
          classList={{ bad: line().kind === "error" }}
          title={t("echo.show_all")}
          onClick={() => {
            setShown(null);
            goto("history");
          }}
        >
          {line().text}
        </button>
      )}
    </Show>
  );
}
