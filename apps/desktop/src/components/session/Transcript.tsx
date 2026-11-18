// The room, in the session interface: what was said and what was written, in
// order. A turn of speech is a row like any other — who, when, how long — so
// the page reads the same whether people talked or typed.

import { For, Match, Show, Switch, createEffect, on } from "solid-js";
import { state } from "../../lib/store";
import { rows, type Row } from "../../lib/transcript";
import { roomLabel } from "../../lib/rooms";
import { tint } from "../../lib/tint";
import { t } from "../../lib/i18n";

/// Width of a turn's mark per second spoken, and where it stops growing: past
/// that the number beside it carries the length, not the bar.
const PX_PER_SEC = 12;
const WAVE_MAX = 320;
/// Close enough to the bottom that a new row should keep you there.
const STICK = 48;

const hm = (ts: number) => new Date(ts * 1000).toTimeString().slice(0, 5);

function Voice(props: { row: Extract<Row, { kind: "voice" }> }) {
  const secs = () => Math.max(1, Math.round(props.row.end - props.row.t));
  return (
    <div class="ses-voice">
      <span
        class="ses-wave"
        classList={{ "ses-wave-live": props.row.live }}
        style={{
          width: `${Math.min(WAVE_MAX, Math.max(18, secs() * PX_PER_SEC))}px`,
          color: tint(props.row.who),
        }}
        aria-hidden="true"
      />
      <span class="ses-dur">
        {props.row.live ? t("ses.speaking_now") : t("ses.spoke", { secs: secs() })}
      </span>
    </div>
  );
}

export default function Transcript() {
  let box: HTMLDivElement | undefined;

  const title = () =>
    state.dm ? `@${state.dm.name}` : state.channel ? roomLabel(state.channel) : "";

  // Follow the conversation unless the reader has scrolled back into it.
  let stuck = true;
  createEffect(
    on(
      () => rows.length,
      () => {
        if (box && stuck) box.scrollTop = box.scrollHeight;
      },
    ),
  );

  return (
    <div
      class="ses-log"
      ref={box}
      onScroll={() => {
        if (box) stuck = box.scrollHeight - box.scrollTop - box.clientHeight < STICK;
      }}
    >
      <h1 class="ses-title">{title()}</h1>
      <p class="ses-note">
        # {state.dm ? t("ses.note_dm") : t("ses.note_room")}
      </p>

      <Show when={!rows.length}>
        <p class="ses-note"># {t("ses.empty")}</p>
      </Show>

      <For each={rows}>
        {(row) => (
          <div
            class="ses-row"
            classList={{
              "ses-row-event": row.kind === "event",
              "ses-row-bad": row.kind === "event" && row.bad,
              "ses-row-me": row.kind === "text" && row.mine,
            }}
          >
            <time class="ses-time">{hm(row.t)}</time>
            <Switch>
              <Match when={row.kind === "event" && row}>
                {(r) => <p class="ses-text ses-wide"># {r().text}</p>}
              </Match>
              <Match when={row.kind === "text" && row}>
                {(r) => (
                  <>
                    <span class="ses-who" style={{ color: tint(r().who) }}>
                      {r().who}
                    </span>
                    <p class="ses-text">{r().text}</p>
                  </>
                )}
              </Match>
              <Match when={row.kind === "voice" && row}>
                {(r) => (
                  <>
                    <span class="ses-who" style={{ color: tint(r().who) }}>
                      {r().who}
                    </span>
                    <Voice row={r()} />
                  </>
                )}
              </Match>
            </Switch>
          </div>
        )}
      </For>
    </div>
  );
}
