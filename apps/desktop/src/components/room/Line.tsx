// The one place text is entered. It says things, runs commands, searches
// settings and renames you — the prompt in front of it says which.

import { For, Show, createEffect, createSignal, onCleanup, onMount } from "solid-js";
import { pushLog, state } from "../../lib/store";
import { runCommand } from "../../lib/commands";
import { openDm } from "../../lib/dm";
import { cmd } from "../../lib/tauri";
import { tweaks } from "../../lib/theme";
import { formatAccel, shortcut } from "../../lib/platform";
import { setQuery, takeFill, view } from "../../lib/scene";
import { roomLabel } from "../../lib/rooms";
import { t } from "../../lib/i18n";

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

/// Look back from the caret: are we inside an `@mention`?
function detectMention(value: string, caret: number): string | null {
  const m = value.slice(0, caret).match(/(?:^|\s)@([^\s]*)$/);
  return m ? m[1] : null;
}

const GLOBAL_LABELS: Record<string, string> = {
  ptt: "talk",
  mute: "mute",
  quick_join: "join",
};

export default function Line() {
  const [value, setValue] = createSignal("");
  const [mention, setMention] = createSignal<string | null>(null);
  const [mentionIdx, setMentionIdx] = createSignal(0);
  const [globalKeys, setGlobalKeys] = createSignal<[string, string][]>([]);
  let history = loadHistory();
  let cursor = history.length;
  let input: HTMLInputElement | undefined;

  onMount(() => {
    // No autofocus: space belongs to the microphone until you start writing
    // (Shell hands focus over on the first printable key).
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key === "l") {
        e.preventDefault();
        input?.focus();
      }
    };
    window.addEventListener("keydown", onKey);
    onCleanup(() => window.removeEventListener("keydown", onKey));
  });

  // Re-read bound global hotkeys whenever we leave settings, since that's
  // where they change.
  createEffect(() => {
    if (view() !== "settings") cmd.listHotkeys().then(setGlobalKeys).catch(() => {});
  });

  // Something clicked elsewhere wants to put text here (a nickname, a command).
  createEffect(() => {
    const pre = takeFill();
    if (pre === null) return;
    setValue(pre);
    input?.focus();
    queueMicrotask(() => input?.setSelectionRange(pre.length, pre.length));
  });

  const isSearching = () => view() === "settings";

  // Crossing in or out of settings empties the box: a half-typed message
  // shouldn't become a search, and a search shouldn't become a message.
  let wasSearching = false;
  createEffect(() => {
    const now = isSearching();
    if (now === wasSearching) return;
    wasSearching = now;
    setValue("");
    setQuery("");
  });

  const prompt = () => {
    if (view() === "settings") return t("line.settings");
    if (view() === "history") return t("line.history");
    if (state.dm) return `@${state.dm.name}`;
    return state.channel ? roomLabel(state.channel) : "";
  };

  const placeholder = () => {
    if (isSearching()) return t("line.find_setting");
    if (state.dm) return t("line.say_to", { name: state.dm.name });
    if (state.channel) return t("line.say");
    return t("line.command");
  };

  const candidates = () => {
    const q = (mention() ?? "").toLowerCase();
    const me = state.status?.name;
    return state.participants
      .filter((p) => p !== me && p.toLowerCase().includes(q))
      .slice(0, 8);
  };

  const pickMention = async (name: string) => {
    setMention(null);
    setValue("");
    try {
      const pk = await cmd.resolvePeer(name);
      if (pk) openDm(pk, name);
      else pushLog(t("line.no_such_peer", { name }), "error");
    } catch (e) {
      pushLog(`${e}`, "error");
    }
  };

  const onInput = (e: InputEvent & { currentTarget: HTMLInputElement }) => {
    const el = e.currentTarget;
    setValue(el.value);
    if (isSearching()) {
      setQuery(el.value);
      return;
    }
    setMention(
      state.dm ? null : detectMention(el.value, el.selectionStart ?? el.value.length),
    );
    setMentionIdx(0);
  };

  const onKeyDown = async (e: KeyboardEvent & { currentTarget: HTMLInputElement }) => {
    if (mention() !== null) {
      const list = candidates();
      if (list.length) {
        if (e.key === "ArrowDown") {
          e.preventDefault();
          return setMentionIdx((i) => Math.min(list.length - 1, i + 1));
        }
        if (e.key === "ArrowUp") {
          e.preventDefault();
          return setMentionIdx((i) => Math.max(0, i - 1));
        }
        if (e.key === "Tab" || e.key === "Enter") {
          e.preventDefault();
          return pickMention(list[mentionIdx()]);
        }
      }
      if (e.key === "Escape") {
        e.preventDefault();
        return setMention(null);
      }
    }

    if (e.key === "Enter") {
      e.preventDefault();
      const text = value().trim();
      if (!text || isSearching()) return;
      history = [...history.filter((x) => x !== text), text];
      try {
        localStorage.setItem(HISTORY_KEY, JSON.stringify(history.slice(-HISTORY_MAX)));
      } catch {}
      cursor = history.length;
      setValue("");
      setMention(null);
      await runCommand(text);
      return;
    }

    if (isSearching()) return;

    if (e.key === "ArrowUp") {
      if (value() && cursor === history.length) return;
      e.preventDefault();
      if (cursor > 0) setValue(history[--cursor] ?? "");
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      if (cursor < history.length - 1) setValue(history[++cursor] ?? "");
      else {
        cursor = history.length;
        setValue("");
      }
    }
  };

  const hints = () => {
    const out = [
      // Only true in push-to-talk; in the other modes the mic isn't yours to hold.
      ...(state.status?.voice_mode === "ptt"
        ? [{ keys: "space", label: t("keys.talk") }]
        : []),
      { keys: "m", label: t("keys.mic") },
      { keys: shortcut("k"), label: t("keys.jump") },
      { keys: shortcut("j"), label: t("keys.history") },
      { keys: "/", label: t("keys.commands") },
      { keys: "@", label: t("keys.dm") },
    ];
    for (const [action, accel] of globalKeys()) {
      const label = GLOBAL_LABELS[action];
      if (label) out.push({ keys: formatAccel(accel), label });
    }
    return out;
  };

  return (
    <div class="line-wrap">
      <Show when={mention() !== null && candidates().length}>
        <div class="mentions">
          <For each={candidates()}>
            {(name, i) => (
              <button
                class="word"
                classList={{ "word-on": i() === mentionIdx() }}
                onMouseEnter={() => setMentionIdx(i())}
                onMouseDown={(e) => {
                  e.preventDefault();
                  pickMention(name);
                }}
              >
                @{name}
              </button>
            )}
          </For>
        </div>
      </Show>

      <div class="line">
        <span class="caret">
          <Show when={prompt()}>
            <b>{prompt()}</b>{" "}
          </Show>
          ❯
        </span>
        <input
          ref={input}
          value={value()}
          placeholder={placeholder()}
          onInput={onInput}
          onKeyDown={onKeyDown}
          onBlur={() => setMention(null)}
          autocomplete="off"
          spellcheck={false}
          aria-label={placeholder()}
        />
      </div>

      <Show when={tweaks.composerHints && !value()}>
        <div class="keys">
          <For each={hints()}>
            {(h) => (
              <span>
                <b>{h.keys}</b> {h.label}
              </span>
            )}
          </For>
        </div>
      </Show>
    </div>
  );
}
