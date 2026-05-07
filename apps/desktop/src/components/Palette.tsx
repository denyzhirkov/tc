// ⌘K palette. Replaces the standalone command bar — surfaces channels, DMs,
// settings entries, and acts as a fallback for free-form slash commands.

import { For, Show, createEffect, createMemo, createSignal, onCleanup, onMount } from "solid-js";
import { state, update } from "../lib/store";
import { joinChannel } from "../lib/actions";
import { runCommand } from "../lib/commands";
import { openDm } from "../lib/dm";
import { cmd, type DmPeerView, type ServerView } from "../lib/tauri";
import { closePalette, openPalette, paletteOpen } from "../lib/palette";
import { Hash, Search, Gear } from "./Icons";

type Item =
  | { kind: "channel"; id: string; count: number; label: string }
  | { kind: "dm"; pubkey: string; name: string; label: string }
  | { kind: "server"; addr: string; label: string }
  | { kind: "command"; cmd: string; label: string; hint?: string }
  | { kind: "raw"; cmd: string; label: string };

const COMMANDS: { cmd: string; label: string; hint?: string }[] = [
  { cmd: "/list", label: "list channels" },
  { cmd: "/create", label: "create channel" },
  { cmd: "/leave", label: "leave channel" },
  { cmd: "/disconnect", label: "disconnect" },
  { cmd: "/reconnect", label: "reconnect" },
  { cmd: "/devices", label: "list audio devices" },
  { cmd: "/test", label: "play test tone" },
  { cmd: "/help", label: "help" },
];

export default function Palette() {
  const [query, setQuery] = createSignal("");
  const [active, setActive] = createSignal(0);
  const [servers, setServers] = createSignal<ServerView[]>([]);
  const [dms, setDms] = createSignal<DmPeerView[]>([]);
  let inputRef: HTMLInputElement | undefined;

  // Global hotkey
  onMount(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        openPalette();
      }
    };
    window.addEventListener("keydown", onKey);
    onCleanup(() => window.removeEventListener("keydown", onKey));
  });

  // When the palette opens, sync initial query and refresh data.
  createEffect(async () => {
    const o = paletteOpen();
    if (!o) return;
    setQuery(o.initial ?? "");
    setActive(0);
    queueMicrotask(() => inputRef?.focus());
    try {
      const [s, d] = await Promise.all([cmd.listServers(), cmd.listDmPeers()]);
      setServers(s);
      setDms(d);
    } catch {}
  });

  const items = createMemo<Item[]>(() => {
    const q = query().trim().toLowerCase();
    const out: Item[] = [];

    // If query starts with /, top suggestion is "run as command"
    if (q.startsWith("/")) {
      out.push({ kind: "raw", cmd: query().trim(), label: `run: ${query().trim()}` });
    }

    for (const ch of state.channelList) {
      const lbl = `# ${ch.channel_id}`;
      if (!q || lbl.toLowerCase().includes(q) || ch.channel_id.toLowerCase().includes(q)) {
        out.push({
          kind: "channel",
          id: ch.channel_id,
          count: ch.participant_count,
          label: lbl,
        });
      }
    }
    for (const p of dms()) {
      if (!q || p.name.toLowerCase().includes(q)) {
        out.push({ kind: "dm", pubkey: p.pubkey_hex, name: p.name, label: `DM ${p.name}` });
      }
    }
    for (const s of servers()) {
      if (!q || s.addr.toLowerCase().includes(q)) {
        out.push({ kind: "server", addr: s.addr, label: `connect ${s.addr}` });
      }
    }
    if (!q || "settings".includes(q)) {
      out.push({ kind: "command", cmd: "/settings", label: "open settings" });
    }
    for (const c of COMMANDS) {
      if (!q || c.cmd.includes(q) || c.label.toLowerCase().includes(q)) {
        out.push({ kind: "command", cmd: c.cmd, label: c.label, hint: c.cmd });
      }
    }
    return out.slice(0, 50);
  });

  createEffect(() => {
    void items().length;
    setActive(0);
  });

  const choose = async (item: Item) => {
    closePalette();
    switch (item.kind) {
      case "channel":
        return joinChannel(item.id);
      case "dm":
        return openDm(item.pubkey, item.name);
      case "server":
        return runCommand(`/server ${item.addr}`);
      case "command":
        if (item.cmd === "/settings") return update.showSettings(true);
        return runCommand(item.cmd);
      case "raw":
        return runCommand(item.cmd);
    }
  };

  const onKeyDown = async (e: KeyboardEvent) => {
    const list = items();
    if (e.key === "Escape") {
      e.preventDefault();
      closePalette();
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      setActive((i) => Math.min(i + 1, Math.max(0, list.length - 1)));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActive((i) => Math.max(i - 1, 0));
    } else if (e.key === "Enter") {
      e.preventDefault();
      const item = list[active()];
      if (item) {
        await choose(item);
      } else {
        const q = query().trim();
        if (q.startsWith("/")) {
          closePalette();
          await runCommand(q);
        }
      }
    }
  };

  return (
    <Show when={paletteOpen()}>
      <div
        class="fixed inset-0 z-50 bg-black/40 flex items-start justify-center pt-[15vh]"
        onMouseDown={(e) => {
          if (e.currentTarget === e.target) closePalette();
        }}
      >
        <div class="w-[560px] max-w-[90vw] bg-surface border border-line rounded-xl shadow-2xl overflow-hidden">
          <div class="flex items-center gap-2 px-3 h-11 border-b border-line">
            <span class="text-muted">
              <Search size={16} />
            </span>
            <input
              ref={inputRef}
              class="flex-1 text-sm placeholder:text-faint"
              placeholder="jump to channel, DM, command…"
              value={query()}
              onInput={(e) => setQuery(e.currentTarget.value)}
              onKeyDown={onKeyDown}
              autocomplete="off"
              spellcheck={false}
            />
          </div>
          <div class="max-h-[50vh] overflow-y-auto py-1">
            <For each={items()}>
              {(item, i) => (
                <button
                  class={`w-full flex items-center gap-2 px-3 py-2 text-left text-sm ${
                    i() === active()
                      ? "bg-accent-bg text-text"
                      : "text-text2 hover:bg-hover"
                  }`}
                  onMouseEnter={() => setActive(i())}
                  onClick={() => choose(item)}
                >
                  <ItemIcon kind={item.kind} />
                  <span class="flex-1 truncate">{item.label}</span>
                  <Show when={(item as any).hint}>
                    <span class="font-mono text-xs text-faint">
                      {(item as any).hint}
                    </span>
                  </Show>
                  <Show when={item.kind === "channel"}>
                    <span class="text-xs text-muted">
                      {(item as any).count}
                    </span>
                  </Show>
                </button>
              )}
            </For>
            <Show when={items().length === 0}>
              <div class="text-muted text-xs px-3 py-3">no matches</div>
            </Show>
          </div>
          <div class="px-3 py-2 border-t border-line text-[10px] text-muted flex gap-3">
            <span>↑↓ navigate</span>
            <span>↵ select</span>
            <span>esc close</span>
          </div>
        </div>
      </div>
    </Show>
  );
}

function ItemIcon(props: { kind: Item["kind"] }) {
  if (props.kind === "channel")
    return (
      <span class="text-faint">
        <Hash size={14} />
      </span>
    );
  if (props.kind === "command" || props.kind === "raw")
    return (
      <span class="text-faint">
        <Gear size={14} />
      </span>
    );
  return (
    <span class="text-faint">
      <Search size={14} />
    </span>
  );
}
