// Main message area. Renders the active log (channel or DM), with consecutive
// same-author chat lines grouped under a single avatar/name header. Day
// separators (── TODAY ──, ── YESTERDAY ──, ── 5 May ──) split groups when
// the date changes between consecutive timestamped lines.

import { For, Show, createEffect, createMemo, createSignal } from "solid-js";
import { state, type LogLine } from "../lib/store";
import Identicon from "./Identicon";
import Composer from "./Composer";

type Group =
  | { kind: "msg"; author: string; lines: LogLine[]; firstId: number }
  | { kind: "system"; line: LogLine }
  | { kind: "day"; key: string; label: string };

const CHAT_RE = /^([^:]{1,40}):\s(.*)$/s;

function classify(line: LogLine): { author: string | null; text: string } {
  const k = line.kind ?? "info";
  if (k === "chat" || k === "dm") {
    const m = line.text.match(CHAT_RE);
    if (m) return { author: m[1], text: m[2] };
  }
  return { author: null, text: line.text };
}

function dayKey(ts: number): string {
  const d = new Date(ts * 1000);
  return `${d.getFullYear()}-${d.getMonth()}-${d.getDate()}`;
}
function dayLabel(ts: number): string {
  const d = new Date(ts * 1000);
  const today = new Date();
  const t = new Date(today.getFullYear(), today.getMonth(), today.getDate());
  const that = new Date(d.getFullYear(), d.getMonth(), d.getDate());
  const diff = Math.round((t.getTime() - that.getTime()) / 86400000);
  if (diff === 0) return "TODAY";
  if (diff === 1) return "YESTERDAY";
  if (diff > 0 && diff < 7) {
    return d
      .toLocaleDateString(undefined, { weekday: "long" })
      .toUpperCase();
  }
  return d
    .toLocaleDateString(undefined, { day: "numeric", month: "short" })
    .toUpperCase();
}
function hm(ts: number): string {
  return new Date(ts * 1000)
    .toTimeString()
    .slice(0, 5);
}

function groupLines(lines: readonly LogLine[]): Group[] {
  const out: Group[] = [];
  let lastDayKey: string | null = null;
  for (const line of lines) {
    if (line.ts) {
      const k = dayKey(line.ts);
      if (k !== lastDayKey) {
        out.push({ kind: "day", key: k, label: dayLabel(line.ts) });
        lastDayKey = k;
      }
    }
    const c = classify(line);
    if (!c.author) {
      out.push({ kind: "system", line });
      continue;
    }
    const last = out[out.length - 1];
    if (
      last &&
      last.kind === "msg" &&
      last.author === c.author &&
      last.lines.length < 30
    ) {
      last.lines.push({ ...line, text: c.text });
    } else {
      out.push({
        kind: "msg",
        author: c.author,
        lines: [{ ...line, text: c.text }],
        firstId: line.id,
      });
    }
  }
  return out;
}

function authorPubkey(name: string): string | null {
  if (state.status?.name === name) return state.status?.pubkey ?? null;
  return name;
}

export default function Pane() {
  let scroller: HTMLDivElement | undefined;
  const [autoStick, setAutoStick] = createSignal(true);

  const lines = () => (state.dm ? state.dmLog : state.log);
  const groups = createMemo(() => groupLines(lines()));

  createEffect(() => {
    void groups().length;
    void state.dm?.pubkey_hex;
    if (!autoStick()) return;
    queueMicrotask(() => {
      if (scroller) scroller.scrollTop = scroller.scrollHeight;
    });
  });

  const onScroll = () => {
    if (!scroller) return;
    const nearBottom =
      scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 60;
    setAutoStick(nearBottom);
  };

  return (
    <div class="flex-1 min-h-0 flex flex-col">
      <div
        ref={scroller}
        onScroll={onScroll}
        class="flex-1 overflow-y-auto px-6 py-5"
      >
        <For each={groups()}>
          {(g) => {
            if (g.kind === "day") return <DaySep label={g.label} />;
            if (g.kind === "system") return <SystemLine line={g.line} />;
            return <MessageGroup author={g.author} lines={g.lines} />;
          }}
        </For>
      </div>
      <Composer />
    </div>
  );
}

function DaySep(props: { label: string }) {
  return (
    <div class="flex items-center gap-3 my-3 select-none">
      <span class="flex-1 h-px bg-line" />
      <span class="text-[10px] tracking-wider text-muted">{props.label}</span>
      <span class="flex-1 h-px bg-line" />
    </div>
  );
}

function MessageGroup(props: { author: string; lines: LogLine[] }) {
  const isSelf = () => state.status?.name === props.author;
  const firstTs = () => props.lines[0]?.ts;
  return (
    <div class="flex gap-3 mb-4">
      <div class="shrink-0 pt-0.5">
        <Identicon pubkeyHex={authorPubkey(props.author)} />
      </div>
      <div class="flex-1 min-w-0">
        <div class="flex items-baseline gap-2 mb-0.5">
          <span
            class={`text-sm font-medium ${
              isSelf() ? "text-accent" : "text-text"
            }`}
          >
            {props.author}
          </span>
          <Show when={firstTs()}>
            <span class="text-[11px] text-muted tabular-nums">
              {hm(firstTs()!)}
            </span>
          </Show>
        </div>
        <div class="flex flex-col gap-0.5">
          <For each={props.lines}>
            {(l) => (
              <div class="text-sm text-text2 leading-relaxed whitespace-pre-wrap break-words">
                {l.text}
              </div>
            )}
          </For>
        </div>
      </div>
    </div>
  );
}

function SystemLine(props: { line: LogLine }) {
  const cls = () => {
    switch (props.line.kind) {
      case "error":
        return "text-danger";
      case "devlog":
        return "text-faint font-mono whitespace-pre-wrap";
      default:
        return "text-muted";
    }
  };
  return <div class={`text-xs my-1 px-1 ${cls()}`}>{props.line.text}</div>;
}
