// Inline message composer at the bottom of the message pane.
// Plain text → chat / DM (depending on view), slash commands → runCommand.
// Typing `@` triggers a mention popover that filters channel participants;
// picking one opens a DM thread with them.

import { For, Show, createSignal, onCleanup, onMount } from "solid-js";
import { pushLog, state } from "../lib/store";
import { classifyInput, runCommand } from "../lib/commands";
import { openDm } from "../lib/dm";
import { cmd } from "../lib/tauri";
import { Send } from "./Icons";

const HISTORY_KEY = "tc.cmdhistory";
const HISTORY_MAX = 100;

function loadHistory(): string[] {
  try {
    const raw = localStorage.getItem(HISTORY_KEY);
    return raw ? (JSON.parse(raw) as string[]) : [];
  } catch {
    return [];
  }
}
function saveHistory(h: string[]) {
  try {
    localStorage.setItem(HISTORY_KEY, JSON.stringify(h.slice(-HISTORY_MAX)));
  } catch {}
}

// Look back from the caret to see if we're in an @-mention context — i.e.
// the most recent `@` has no whitespace between it and the caret.
function detectMention(value: string, caret: number): string | null {
  const before = value.slice(0, caret);
  const m = before.match(/(?:^|\s)@([^\s]*)$/);
  return m ? m[1] : null;
}

export default function Composer() {
  const [value, setValue] = createSignal("");
  const [mentionQuery, setMentionQuery] = createSignal<string | null>(null);
  const [mentionIdx, setMentionIdx] = createSignal(0);
  let history = loadHistory();
  let historyIndex = history.length;
  let inputRef: HTMLInputElement | undefined;

  onMount(() => {
    inputRef?.focus();
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key === "l") {
        e.preventDefault();
        inputRef?.focus();
      }
    };
    window.addEventListener("keydown", onKey);
    onCleanup(() => window.removeEventListener("keydown", onKey));
  });

  const placeholder = () => {
    if (state.dm) return `message ${state.dm.name}`;
    if (state.channel) return `message #${state.channel} — type @ to DM a peer`;
    return "type / for commands";
  };

  const candidates = () => {
    const q = (mentionQuery() ?? "").toLowerCase();
    const me = state.status?.name;
    return state.participants
      .filter((p) => p !== me && p.toLowerCase().includes(q))
      .slice(0, 8);
  };

  const refreshMention = (val: string, caret: number) => {
    if (state.dm) {
      // Mentions only make sense in a channel.
      setMentionQuery(null);
      return;
    }
    const q = detectMention(val, caret);
    setMentionQuery(q);
    setMentionIdx(0);
  };

  const onInput = (e: InputEvent & { currentTarget: HTMLInputElement }) => {
    const t = e.currentTarget;
    setValue(t.value);
    refreshMention(t.value, t.selectionStart ?? t.value.length);
  };

  const pickMention = async (name: string) => {
    setMentionQuery(null);
    setValue("");
    try {
      const pk = await cmd.resolvePeer(name);
      if (pk) {
        openDm(pk, name);
      } else {
        pushLog(`couldn't resolve @${name} — try /dm <pk-hex> <text>`, "error");
      }
    } catch (e) {
      pushLog(`error: ${e}`, "error");
    }
  };

  const onKeyDown = async (
    e: KeyboardEvent & { currentTarget: HTMLInputElement },
  ) => {
    // Mention navigation takes priority while popover is open.
    if (mentionQuery() !== null) {
      const list = candidates();
      if (list.length > 0) {
        if (e.key === "ArrowDown") {
          e.preventDefault();
          setMentionIdx((i) => Math.min(list.length - 1, i + 1));
          return;
        }
        if (e.key === "ArrowUp") {
          e.preventDefault();
          setMentionIdx((i) => Math.max(0, i - 1));
          return;
        }
        if (e.key === "Tab" || e.key === "Enter") {
          e.preventDefault();
          await pickMention(list[mentionIdx()]);
          return;
        }
      }
      if (e.key === "Escape") {
        e.preventDefault();
        setMentionQuery(null);
        return;
      }
    }

    if (e.key === "Enter") {
      e.preventDefault();
      const text = value().trim();
      if (!text) return;
      history = [...history.filter((x) => x !== text), text];
      saveHistory(history);
      historyIndex = history.length;
      setValue("");
      setMentionQuery(null);
      await runCommand(text);
    } else if (e.key === "ArrowUp") {
      if (value() && historyIndex === history.length) return;
      e.preventDefault();
      if (historyIndex > 0) {
        historyIndex--;
        setValue(history[historyIndex] ?? "");
      }
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      if (historyIndex < history.length - 1) {
        historyIndex++;
        setValue(history[historyIndex] ?? "");
      } else {
        historyIndex = history.length;
        setValue("");
      }
    }
  };

  const onSelectionChange = (e: Event) => {
    const t = e.currentTarget as HTMLInputElement;
    refreshMention(t.value, t.selectionStart ?? t.value.length);
  };

  const disabled = () =>
    !state.dm && !state.channel && !value().startsWith("/");

  // Command-mode signal: recognised /cmd (and #join) → accent, unrecognised
  // /word → warn, plain text → no signal.
  const kind = () => classifyInput(value());

  const wrapCls = () => {
    if (disabled()) return "border-line bg-surface2 text-muted";
    switch (kind()) {
      case "command":
        return "border-accent bg-surface ring-1 ring-accent";
      case "unknown-command":
        return "border-warn bg-surface ring-1 ring-warn";
      default:
        return "border-line bg-surface focus-within:border-accent";
    }
  };

  return (
    <div class="px-4 pb-4 pt-2 shrink-0 relative">
      <Show when={mentionQuery() !== null && candidates().length > 0}>
        <div class="absolute left-4 right-4 bottom-full mb-1 z-30 bg-surface border border-line rounded-lg shadow-lg p-1 max-h-56 overflow-y-auto">
          <div class="text-[10px] uppercase tracking-wider text-muted px-2 py-1">
            mention → open DM
          </div>
          <For each={candidates()}>
            {(name, i) => (
              <button
                class={`w-full flex items-center gap-2 px-2 py-1.5 rounded text-sm text-left ${
                  i() === mentionIdx()
                    ? "bg-accent-bg text-accent"
                    : "text-text2 hover:bg-hover hover:text-text"
                }`}
                onMouseEnter={() => setMentionIdx(i())}
                onMouseDown={(e) => {
                  // mousedown so blur doesn't kill the popover before click fires
                  e.preventDefault();
                  pickMention(name);
                }}
              >
                <span class="text-faint">@</span>
                <span class="flex-1 truncate">{name}</span>
              </button>
            )}
          </For>
        </div>
      </Show>
      <div
        class={`relative flex items-center gap-2 px-3 py-2 rounded-lg border transition-colors ${wrapCls()}`}
      >
        <Show when={kind() !== "chat" && !disabled()}>
          <span
            class={`absolute left-0 top-1.5 bottom-1.5 w-[3px] rounded-full ${
              kind() === "command" ? "bg-accent" : "bg-warn"
            }`}
          />
        </Show>
        <input
          ref={inputRef}
          class="flex-1 bg-transparent placeholder:text-faint text-sm"
          value={value()}
          placeholder={placeholder()}
          onInput={onInput}
          onKeyDown={onKeyDown}
          onSelect={onSelectionChange}
          onClick={onSelectionChange}
          onBlur={() => setMentionQuery(null)}
          autocomplete="off"
          spellcheck={false}
        />
        <Show when={kind() !== "chat" && !disabled()}>
          <span
            class={`text-[10px] font-mono uppercase tracking-wider px-1.5 py-0.5 rounded select-none ${
              kind() === "command"
                ? "text-accent bg-accent-bg"
                : "text-warn bg-surface2"
            }`}
            title={
              kind() === "command"
                ? "command mode — recognised"
                : "command mode — unknown command"
            }
          >
            {kind() === "command" ? "cmd" : "cmd?"}
          </span>
        </Show>
        <Show when={value()}>
          <button
            class="text-muted hover:text-accent"
            onMouseDown={async (e) => {
              e.preventDefault();
              const text = value().trim();
              if (!text) return;
              setValue("");
              await runCommand(text);
            }}
            title="send"
          >
            <Send size={14} />
          </button>
        </Show>
      </div>
    </div>
  );
}
