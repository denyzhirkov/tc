// Room overlay HUD — a standalone window (overlay.html) shown over fullscreen
// games. It is a pure subscriber to the same backend events the main app uses;
// `AppCore` remains the source of truth. No controls (click-through window),
// just: channel name, roster, and who's speaking.

import { For, Show, createMemo, onCleanup, onMount } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import { cmd, on } from "../lib/tauri";

type Visibility = "always" | "in_call";

type OverlayState = {
  lang: string;
  channel: string | null;
  participants: string[];
  speakers: Record<string, number>; // name → 0..1
  selfName: string | null;
  selfMuted: boolean;
  inputPeak: number;
  conn: "disconnected" | "connecting" | "connected" | "reconnecting";
  visibility: Visibility;
};

const SPEAK_THRESHOLD = 0.04;

// Minimal local i18n — the overlay window has no access to the main store that
// the shared `t()` helper reads from, and it shows only a couple of words.
const STRINGS: Record<string, Record<string, string>> = {
  en: { idle: "not in a call", reconnecting: "reconnecting…", you: "you" },
  ru: { idle: "не в звонке", reconnecting: "переподключение…", you: "вы" },
};

export default function Overlay() {
  const [s, set] = createStore<OverlayState>({
    lang: "en",
    channel: null,
    participants: [],
    speakers: {},
    selfName: null,
    selfMuted: false,
    inputPeak: 0,
    conn: "disconnected",
    visibility: "in_call",
  });

  const tr = (k: string) => STRINGS[s.lang]?.[k] ?? STRINGS.en[k] ?? k;

  const clearCall = () => {
    set("channel", null);
    set("participants", []);
    set("speakers", reconcile({}));
  };

  onMount(async () => {
    // Hydrate from the backend snapshot so a mid-call open shows the full
    // roster immediately (events only carry deltas / currently-speaking peers).
    try {
      const st = await cmd.status();
      set({
        lang: st.language ?? "en",
        channel: st.channel,
        participants: st.participants ?? [],
        selfName: st.name,
        selfMuted: st.muted,
        conn: st.connected ? "connected" : "disconnected",
        visibility: (st.overlay_visibility as Visibility) ?? "in_call",
      });
    } catch {
      /* backend not ready — events will fill in */
    }

    const unlisten = await Promise.all([
      on("connection_state", (c) => {
        if (c.state === "connected") set("conn", "connected");
        else if (c.state === "connecting") set("conn", "connecting");
        else if (c.state === "reconnecting") {
          set("conn", "reconnecting");
          clearCall();
        } else {
          set("conn", "disconnected");
          clearCall();
        }
      }),
      on("joined_channel", (p) => {
        set("channel", p.channel_id);
        set("participants", p.participants);
        set("speakers", reconcile({}));
      }),
      on("left_channel", clearCall),
      on("peer_joined", (p) =>
        set("participants", (list) =>
          list.includes(p.name) ? list : [...list, p.name],
        ),
      ),
      on("peer_left", (p) =>
        set("participants", (list) => list.filter((n) => n !== p.name)),
      ),
      on("name_changed", (p) => {
        set("participants", (list) =>
          list.map((n) => (n === p.old_name ? p.new_name : n)),
        );
        if (s.selfName === p.old_name) set("selfName", p.new_name);
      }),
      on("voice_level", (v) => {
        set("inputPeak", v.input_peak);
        set("selfMuted", v.muted);
        const next: Record<string, number> = {};
        for (const sp of v.speakers) next[sp.name] = sp.level;
        set("speakers", reconcile(next));
      }),
      on("voice_stopped", () => {
        set("inputPeak", 0);
        set("speakers", reconcile({}));
      }),
      on("muted_changed", (p) => set("selfMuted", p.muted)),
      on("overlay_config", (cfg) =>
        set("visibility", cfg.visibility as Visibility),
      ),
    ]);
    onCleanup(() => unlisten.forEach((u) => u()));
  });

  const levelOf = (name: string): number => {
    if (name === s.selfName) {
      if (s.selfMuted) return 0;
      return Math.min(1, s.inputPeak * 4);
    }
    return s.speakers[name] ?? 0;
  };

  const roster = createMemo(() => {
    const me = s.selfName;
    return [...s.participants].sort((a, b) =>
      a === me ? -1 : b === me ? 1 : a.localeCompare(b),
    );
  });

  // Show the full HUD only when in a call; "always" mode keeps a tiny idle pill.
  const inCall = () => !!s.channel;
  const showHud = () => inCall();
  const showIdle = () => !inCall() && s.visibility === "always";

  return (
    <Show when={showHud()} fallback={<Show when={showIdle()}>{idlePill(tr, s)}</Show>}>
      <div class="m-2 rounded-xl bg-black/55 backdrop-blur-md border border-white/10 shadow-lg text-white overflow-hidden">
        <div class="flex items-center gap-2 px-3 py-2 border-b border-white/10">
          <span class="text-emerald-400/90 font-mono text-sm">#</span>
          <span class="text-sm font-medium truncate flex-1">{s.channel}</span>
          <MicBadge muted={s.selfMuted} />
        </div>
        <div class="px-1.5 py-1.5 flex flex-col gap-0.5 max-h-[320px] overflow-hidden">
          <For each={roster()}>
            {(name) => (
              <Row
                name={name}
                self={name === s.selfName}
                level={levelOf(name)}
                youLabel={tr("you")}
              />
            )}
          </For>
          <Show when={roster().length === 0}>
            <div class="text-white/40 text-xs px-2 py-1">—</div>
          </Show>
        </div>
      </div>
    </Show>
  );
}

function idlePill(tr: (k: string) => string, s: OverlayState) {
  return (
    <div class="m-2 inline-flex items-center gap-2 rounded-full bg-black/55 backdrop-blur-md border border-white/10 px-3 py-1.5 text-white/80 text-xs">
      <span class="font-mono text-emerald-400/90">tc_</span>
      <span>{s.conn === "reconnecting" ? tr("reconnecting") : tr("idle")}</span>
    </div>
  );
}

function Row(props: {
  name: string;
  self: boolean;
  level: number;
  youLabel: string;
}) {
  const speaking = () => props.level > SPEAK_THRESHOLD;
  return (
    <div class="flex items-center gap-2 px-2 py-1 rounded-md">
      <Avatar name={props.name} speaking={speaking()} />
      <div class="min-w-0 flex-1">
        <div
          class="text-[13px] truncate transition-colors"
          classList={{
            "text-white": speaking(),
            "text-white/70": !speaking(),
          }}
        >
          {props.name}
          <Show when={props.self}>
            <span class="text-white/35 text-[11px]"> · {props.youLabel}</span>
          </Show>
        </div>
        <div class="h-[3px] mt-0.5 rounded-full bg-white/10 overflow-hidden">
          <div
            class="h-full bg-emerald-400 transition-all duration-75"
            style={{ width: `${Math.round(Math.min(1, props.level) * 100)}%` }}
          />
        </div>
      </div>
    </div>
  );
}

// Deterministic squircle avatar with the name's initial — self-contained so the
// overlay doesn't depend on the main app's themeable Identicon.
function Avatar(props: { name: string; speaking: boolean }) {
  const hue = createMemo(() => {
    let h = 2166136261 >>> 0;
    for (let i = 0; i < props.name.length; i++) {
      h ^= props.name.charCodeAt(i);
      h = Math.imul(h, 16777619);
    }
    return (h >>> 0) % 360;
  });
  const initial = () => props.name.trim().charAt(0).toUpperCase() || "?";
  return (
    <span
      class="inline-flex items-center justify-center w-6 h-6 rounded-lg text-[11px] font-semibold shrink-0 transition-shadow"
      style={{
        background: `hsl(${hue()} 45% 32%)`,
        color: `hsl(${hue()} 70% 85%)`,
        "box-shadow": props.speaking
          ? "0 0 0 2px rgba(52,211,153,0.9)"
          : "0 0 0 1px rgba(255,255,255,0.12)",
      }}
    >
      {initial()}
    </span>
  );
}

function MicBadge(props: { muted: boolean }) {
  return (
    <span
      class="shrink-0"
      classList={{ "text-rose-400": props.muted, "text-white/55": !props.muted }}
      title={props.muted ? "muted" : "live"}
    >
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
        <Show
          when={props.muted}
          fallback={
            <>
              <path d="M12 2a3 3 0 0 0-3 3v6a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3z" />
              <path d="M19 10v1a7 7 0 0 1-14 0v-1" />
              <line x1="12" y1="18" x2="12" y2="22" />
            </>
          }
        >
          <line x1="3" y1="3" x2="21" y2="21" />
          <path d="M9 5a3 3 0 0 1 6 0v5" />
          <path d="M9 11v0a3 3 0 0 0 4.5 2.6" />
          <path d="M19 10v1a7 7 0 0 1-11.3 5.5" />
          <line x1="12" y1="18" x2="12" y2="22" />
        </Show>
      </svg>
    </span>
  );
}
