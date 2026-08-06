// What was said, in full — the counterweight to speech that fades.
// Same grouping the old pane had (consecutive lines by one author, day
// separators), set in the room's type instead of chat bubbles.

import { For, Show, createEffect, createMemo } from "solid-js";
import { state, type LogLine } from "../../lib/store";
import { speakerOf } from "../../lib/stage";
import { tint } from "../../lib/tint";
import { t } from "../../lib/i18n";

type Group =
  | { kind: "said"; who: string; lines: LogLine[]; at?: number }
  | { kind: "system"; line: LogLine }
  | { kind: "day"; label: string };

function dayKey(ts: number) {
  const d = new Date(ts * 1000);
  return `${d.getFullYear()}-${d.getMonth()}-${d.getDate()}`;
}

function dayLabel(ts: number) {
  const d = new Date(ts * 1000);
  const now = new Date();
  const a = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  const b = new Date(d.getFullYear(), d.getMonth(), d.getDate());
  const diff = Math.round((a.getTime() - b.getTime()) / 86400000);
  if (diff === 0) return t("hist.today");
  if (diff === 1) return t("hist.yesterday");
  if (diff > 0 && diff < 7)
    return d.toLocaleDateString(undefined, { weekday: "long" }).toUpperCase();
  return d
    .toLocaleDateString(undefined, { day: "numeric", month: "short" })
    .toUpperCase();
}

const hm = (ts: number) => new Date(ts * 1000).toTimeString().slice(0, 5);

function group(lines: readonly LogLine[]): Group[] {
  const out: Group[] = [];
  let lastDay: string | null = null;
  for (const line of lines) {
    if (line.ts) {
      const k = dayKey(line.ts);
      if (k !== lastDay) {
        out.push({ kind: "day", label: dayLabel(line.ts) });
        lastDay = k;
      }
    }
    const parsed =
      line.kind === "chat" || line.kind === "dm" ? speakerOf(line.text) : null;
    if (!parsed) {
      out.push({ kind: "system", line });
      continue;
    }
    const last = out[out.length - 1];
    if (last?.kind === "said" && last.who === parsed.who && last.lines.length < 30) {
      last.lines.push({ ...line, text: parsed.said });
    } else {
      out.push({
        kind: "said",
        who: parsed.who,
        lines: [{ ...line, text: parsed.said }],
        at: line.ts,
      });
    }
  }
  return out;
}

export default function HistoryScene() {
  let scroller: HTMLDivElement | undefined;
  const lines = () => (state.dm ? state.dmLog : state.log);
  const groups = createMemo(() => group(lines()));

  createEffect(() => {
    void groups().length;
    queueMicrotask(() => {
      if (scroller) scroller.scrollTop = scroller.scrollHeight;
    });
  });

  const me = () => state.status?.name ?? null;

  return (
    <div class="history" ref={scroller}>
      <Show when={groups().length} fallback={<div class="note">{t("hist.empty")}</div>}>
        <For each={groups()}>
          {(g) => {
            if (g.kind === "day") return <div class="day">{g.label}</div>;
            if (g.kind === "system")
              return (
                <div class="sys" classList={{ bad: g.line.kind === "error" }}>
                  {g.line.text}
                </div>
              );
            const who = g.who === "you" && me() ? me()! : g.who;
            return (
              <div class="turn">
                <div class="turn-head">
                  <span class="turn-who" style={{ color: tint(who) }}>
                    {who}
                  </span>
                  <Show when={g.at}>
                    <time class="turn-at">{hm(g.at!)}</time>
                  </Show>
                </div>
                <For each={g.lines}>{(l) => <p class="turn-said">{l.text}</p>}</For>
              </div>
            );
          }}
        </For>
      </Show>
    </div>
  );
}
