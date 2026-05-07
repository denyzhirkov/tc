// Left navigation: server picker, channels, DMs, identity strip.
// Click-driven; the bottom command palette (⌘K) layers on top for power users.

import { For, Show, createSignal, onMount } from "solid-js";
import { state } from "../lib/store";
import {
  connectTo,
  disconnect,
  joinChannel,
  reconnect,
} from "../lib/actions";
import { cmd, type DmPeerView, type ServerView } from "../lib/tauri";
import { openDm } from "../lib/dm";
import { openPalette } from "../lib/palette";
import Identicon from "./Identicon";
import VoiceStrip from "./VoiceStrip";
import VoiceWave from "./VoiceWave";
import { Hash, Plus, Chev, Search } from "./Icons";
import { shortcut } from "../lib/platform";

function ConnDot() {
  const cls = () => {
    switch (state.conn) {
      case "connected":
        return "bg-ok";
      case "connecting":
        return "bg-warn";
      default:
        return "bg-faint";
    }
  };
  const title = () =>
    state.conn === "connected" ? "click: disconnect" : "click: connect";
  const onClick = () =>
    state.conn === "connected" ? disconnect() : reconnect();
  return (
    <span
      class={`inline-block w-1.5 h-1.5 rounded-full cursor-pointer ${cls()}`}
      title={title()}
      onClick={onClick}
    />
  );
}

function ServerPicker() {
  const [open, setOpen] = createSignal(false);
  const [servers, setServers] = createSignal<ServerView[]>([]);
  let panelRef: HTMLDivElement | undefined;

  const refresh = async () => {
    try {
      setServers(await cmd.listServers());
    } catch {
      setServers([]);
    }
  };

  onMount(() => {
    const onDoc = (e: MouseEvent) => {
      if (!open()) return;
      if (panelRef && !panelRef.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", onDoc);
    return () => document.removeEventListener("mousedown", onDoc);
  });

  return (
    <div class="relative">
      <button
        class="w-full flex items-center gap-2 px-2 py-1.5 rounded-md hover:bg-hover text-left"
        onClick={() => {
          if (!open()) refresh();
          setOpen((v) => !v);
        }}
      >
        <ConnDot />
        <span class="flex-1 truncate text-sm">{state.serverAddr}</span>
        <span class="text-muted">
          <Chev open={open()} />
        </span>
      </button>
      <Show when={open()}>
        <div
          ref={panelRef}
          class="absolute left-0 right-0 top-full mt-1 z-20 bg-surface border border-line rounded-lg shadow-lg p-1.5 max-h-72 overflow-y-auto"
        >
          <For
            each={[...servers()].sort((a, b) =>
              a.favourite !== b.favourite
                ? a.favourite
                  ? -1
                  : 1
                : b.last_used_unix - a.last_used_unix,
            )}
          >
            {(s) => (
              <button
                class="w-full flex items-center gap-2 px-2 py-1.5 rounded hover:bg-hover text-left text-sm"
                onClick={() => {
                  setOpen(false);
                  connectTo(s.addr);
                }}
              >
                <span class={s.favourite ? "text-accent" : "text-faint"}>
                  {s.favourite ? "★" : "·"}
                </span>
                <span class="flex-1 truncate">{s.addr}</span>
                <Show when={s.label}>
                  <span class="text-muted text-xs truncate">{s.label}</span>
                </Show>
              </button>
            )}
          </For>
          <Show when={servers().length === 0}>
            <div class="text-muted text-xs px-2 py-1.5">no known servers</div>
          </Show>
          <div class="border-t border-line mt-1 pt-1">
            <button
              class="w-full flex items-center gap-2 px-2 py-1.5 rounded hover:bg-hover text-left text-xs text-muted"
              onClick={() => {
                setOpen(false);
                openPalette("/server ");
              }}
            >
              <Plus size={12} /> add server…
            </button>
          </div>
        </div>
      </Show>
    </div>
  );
}

function SectionHead(props: { label: string; action?: () => void }) {
  return (
    <div class="flex items-center px-2 mb-1 text-[11px] uppercase tracking-wider text-muted select-none">
      <span class="flex-1">{props.label}</span>
      <Show when={props.action}>
        <button
          class="p-0.5 rounded hover:bg-hover text-faint hover:text-text"
          onClick={props.action}
          title="add"
        >
          <Plus size={12} />
        </button>
      </Show>
    </div>
  );
}

const PRIVATE_STRIPE_ACTIVE =
  "repeating-linear-gradient(135deg, rgba(127,127,127,0.10) 0 6px, transparent 6px 12px)";
const PRIVATE_STRIPE_IDLE =
  "repeating-linear-gradient(135deg, rgba(127,127,127,0.06) 0 6px, transparent 6px 12px)";

function ChannelRow(props: {
  id: string;
  count: number;
  active: boolean;
  speaking: boolean;
  level: number;
  joined: boolean;
  priv: boolean;
}) {
  // For private rows we paint the background as diagonal stripes via inline
  // style — that overrides the regular Tailwind bg classes, so we only set
  // the text-colour utilities here.
  const baseCls = () =>
    props.priv
      ? props.active
        ? "text-accent"
        : "text-text2 hover:text-text"
      : props.active
        ? "bg-accent-bg text-accent"
        : "text-text2 hover:bg-hover hover:text-text";
  const bg = () =>
    props.priv
      ? props.active
        ? PRIVATE_STRIPE_ACTIVE
        : PRIVATE_STRIPE_IDLE
      : undefined;
  return (
    <button
      class={`relative w-full flex items-center gap-2 px-2 py-1 rounded text-sm text-left ${baseCls()} ${
        props.joined ? "cursor-default" : "cursor-pointer"
      }`}
      style={bg() ? { background: bg() } : undefined}
      disabled={props.joined}
      onClick={() => joinChannel(props.id)}
    >
      <span class="text-faint">
        <Hash size={14} />
      </span>
      <span class="flex-1 truncate">{props.id}</span>
      <Show
        when={props.joined}
        fallback={
          <Show when={props.count > 0}>
            <span class="text-xs text-muted tabular-nums">{props.count}</span>
          </Show>
        }
      >
        <span class="text-[10px] tracking-wider text-accent font-semibold">
          LIVE
        </span>
      </Show>
      {/* voice-activity wave under the row */}
      <Show when={props.speaking}>
        <VoiceWave level={props.level} />
      </Show>
    </button>
  );
}

function DmRow(props: { peer: DmPeerView; active: boolean }) {
  return (
    <button
      class={`w-full flex items-center gap-2 px-2 py-1 rounded text-sm text-left ${
        props.active
          ? "bg-accent-bg text-accent"
          : "text-text2 hover:bg-hover hover:text-text"
      }`}
      onClick={() => openDm(props.peer.pubkey_hex, props.peer.name)}
    >
      <span class="flex-1 truncate">{props.peer.name}</span>
      <span class="text-[10px] font-mono text-faint tabular-nums">
        {props.peer.pubkey_hex.slice(0, 6)}
      </span>
    </button>
  );
}

export default function Sidebar() {
  const [dms, setDms] = createSignal<DmPeerView[]>([]);
  const refreshDms = async () => {
    try {
      setDms(await cmd.listDmPeers());
    } catch {
      setDms([]);
    }
  };
  onMount(refreshDms);

  // Refresh on every direct_message event isn't wired, so refresh when DM opens
  // and every few seconds while sidebar is mounted.
  onMount(() => {
    const id = setInterval(refreshDms, 4000);
    return () => clearInterval(id);
  });

  const isPrivate = (id: string) => !id.startsWith("pub-");

  // Channel speaking state — true if any speaker level > threshold.
  // Includes own mic peak (input_peak is RMS-ish, much smaller than the
  // perceived speaker levels — boost it heavily). A gamma curve (sqrt) on
  // top makes quiet speech read clearly without saturating loud speech.
  const channelLevel = (id: string) => {
    if (state.channel !== id) return 0;
    let max = (state.voice?.input_peak ?? 0) * 8;
    for (const k in state.speakers) max = Math.max(max, state.speakers[k]);
    return Math.min(1, Math.sqrt(Math.max(0, max)));
  };

  return (
    <aside class="w-60 shrink-0 h-full flex flex-col bg-surface border-r border-line">
      {/* server + jump-to */}
      <div class="p-2 border-b border-line flex flex-col gap-1.5">
        <ServerPicker />
        <button
          class="w-full flex items-center gap-2 px-2 py-1.5 rounded-md text-sm text-muted bg-bg hover:bg-hover hover:text-text border border-line"
          onClick={() => openPalette()}
          title={shortcut("k")}
        >
          <Search size={14} />
          <span class="flex-1 text-left">jump to…</span>
          <span class="text-xs text-faint">{shortcut("k")}</span>
        </button>
      </div>

      {/* channels + dms */}
      <div class="flex-1 overflow-y-auto py-2">
        <SectionHead
          label="channels"
          action={() => openPalette("/create ")}
        />
        <div class="px-1.5 mb-3 flex flex-col gap-0.5">
          <For each={state.channelList}>
            {(ch) => (
              <ChannelRow
                id={ch.channel_id}
                count={ch.participant_count}
                active={state.channel === ch.channel_id}
                joined={state.channel === ch.channel_id}
                speaking={channelLevel(ch.channel_id) > 0.02}
                level={channelLevel(ch.channel_id)}
                priv={isPrivate(ch.channel_id)}
              />
            )}
          </For>
          <Show
            when={state.channel && !state.channelList.some((c) => c.channel_id === state.channel)}
          >
            <ChannelRow
              id={state.channel!}
              count={state.participants.length}
              active={true}
              joined={true}
              speaking={channelLevel(state.channel!) > 0.02}
              level={channelLevel(state.channel!)}
              priv={isPrivate(state.channel!)}
            />
          </Show>
          <Show when={state.channelList.length === 0 && !state.channel}>
            <div class="text-muted text-xs px-2 py-1">
              no channels — try /list
            </div>
          </Show>
        </div>

        <SectionHead label="direct messages" />
        <div class="px-1.5 flex flex-col gap-0.5">
          <For each={dms()}>
            {(p) => (
              <DmRow peer={p} active={state.dm?.pubkey_hex === p.pubkey_hex} />
            )}
          </For>
          <Show when={dms().length === 0}>
            <div class="text-muted text-xs px-2 py-1">no DMs yet</div>
          </Show>
        </div>
      </div>

      {/* identity */}
      <div class="border-t border-line p-2">
        <Identity />
      </div>
      <VoiceStrip />
    </aside>
  );
}

function Identity() {
  return (
    <div class="flex items-center gap-2 px-1">
      <Identicon pubkeyHex={state.status?.pubkey} />
      <div class="flex-1 min-w-0">
        <div class="text-sm truncate">
          {state.status?.name ?? "anonymous"}
        </div>
        <div class="text-[10px] font-mono text-muted truncate">
          {state.status?.fingerprint ?? "—"}
        </div>
      </div>
    </div>
  );
}
