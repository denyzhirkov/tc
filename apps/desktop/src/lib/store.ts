// Reactive global state for the desktop client.
// Kept intentionally small — Solid stores hold what the UI renders, the
// backend (`AppCore` in Rust) is the source of truth for everything else.

import { createStore } from "solid-js/store";
import type {
  AppStatus,
  ChannelListEntry,
  InvitePayload,
  VoiceLevelPayload,
} from "./tauri";

export type LogLine = {
  id: number;
  text: string;
  kind?: "info" | "chat" | "dm" | "system" | "error" | "devlog";
  /// Unix seconds. Set for chat/dm lines; absent for system/info.
  ts?: number;
};

export type AppState = {
  status: AppStatus | null;
  conn: "disconnected" | "connecting" | "connected" | "reconnecting";
  serverAddr: string;
  channel: string | null;
  participants: string[];
  channelList: ChannelListEntry[];
  log: LogLine[];
  voice: VoiceLevelPayload | null;
  /// Per-peer levels (name → 0..1).
  speakers: Record<string, number>;
  /// Settings overlay visibility.
  showSettings: boolean;
  /// When set, the pane shows a DM thread with this peer instead of the channel log.
  dm: { pubkey_hex: string; name: string } | null;
  /// Lines for the active DM thread. Cleared each time `dm` changes.
  dmLog: LogLine[];
  /// /show_dev_logs toggle — stream DEBUG+ tracing into the main feed.
  devLogs: boolean;
  /// tc:// invite to a server not in the registry, awaiting user confirmation.
  invitePrompt: InvitePayload | null;
};

let nextId = 1;

const [state, setState] = createStore<AppState>({
  status: null,
  conn: "disconnected",
  serverAddr: "127.0.0.1:7100",
  channel: null,
  participants: [],
  channelList: [],
  log: [],
  voice: null,
  speakers: {},
  showSettings: false,
  dm: null,
  dmLog: [],
  devLogs: false,
  invitePrompt: null,
});

export { state };

export function pushLog(
  text: string,
  kind: LogLine["kind"] = "info",
  ts?: number,
) {
  setState("log", (l) => {
    const next: LogLine[] = [...l, { id: nextId++, text, kind, ts }];
    // keep last 500
    return next.length > 500 ? next.slice(next.length - 500) : next;
  });
}

export const update = {
  status: (s: AppStatus) => setState("status", s),
  conn: (c: AppState["conn"]) => setState("conn", c),
  serverAddr: (a: string) => setState("serverAddr", a),
  channel: (c: string | null, participants: string[] = []) => {
    setState("channel", c);
    setState("participants", participants);
  },
  channelList: (l: ChannelListEntry[]) => setState("channelList", l),
  addParticipant: (name: string) =>
    setState("participants", (p) => (p.includes(name) ? p : [...p, name])),
  removeParticipant: (name: string) =>
    setState("participants", (p) => p.filter((x) => x !== name)),
  renameParticipant: (oldName: string, newName: string) => {
    setState("participants", (p) =>
      p.map((n) => (n === oldName ? newName : n)),
    );
    setState("speakers", (s) => {
      if (!(oldName in s)) return s;
      const next: Record<string, number> = { ...s };
      next[newName] = next[oldName];
      delete next[oldName];
      return next;
    });
    if (state.status?.name === oldName) {
      setState("status", "name", newName);
    }
  },
  voice: (v: VoiceLevelPayload | null) => {
    setState("voice", v);
    if (!v) {
      setState("speakers", {});
      return;
    }
    const next: Record<string, number> = {};
    for (const s of v.speakers) next[s.name] = s.level;
    setState("speakers", next);
  },
  speakers: (m: Record<string, number>) => setState("speakers", m),
  showSettings: (v: boolean) => setState("showSettings", v),
  devLogs: (v: boolean) => setState("devLogs", v),
  invitePrompt: (p: InvitePayload | null) => setState("invitePrompt", p),
  openDm: (pubkey_hex: string, name: string) => {
    setState("dm", { pubkey_hex, name });
    setState("dmLog", []);
  },
  closeDm: () => {
    setState("dm", null);
    setState("dmLog", []);
  },
  pushDmLog: (text: string, kind: LogLine["kind"] = "dm", ts?: number) => {
    setState("dmLog", (l) => {
      const next: LogLine[] = [...l, { id: nextId++, text, kind, ts }];
      return next.length > 500 ? next.slice(next.length - 500) : next;
    });
  },
};
