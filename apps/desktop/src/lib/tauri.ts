// Typed wrappers around Tauri commands and events.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type VoiceMode = "open" | "vad" | "ptt";

export type AppStatus = {
  version: string;
  fingerprint: string | null;
  pubkey: string | null;
  connected: boolean;
  server_addr: string | null;
  channel: string | null;
  muted: boolean;
  name: string | null;
  voice_mode: VoiceMode;
  input_device: string | null;
  output_device: string | null;
  input_gain_pct: number;
  output_vol_pct: number;
  vad_level_pct: number;
  notifications: boolean;
  close_to_tray: boolean;
  autostart: boolean;
  language: string;
};

export type ConnState =
  | { state: "disconnected" }
  | { state: "connecting" }
  | { state: "connected"; server: string }
  | { state: "reconnecting"; attempt: number; delay_secs: number };

export type JoinedPayload = { channel_id: string; participants: string[] };
export type ChannelListEntry = { channel_id: string; participant_count: number };
export type ChannelListPayload = { channels: ChannelListEntry[] };
export type ChatPayload = { from: string; text: string };
export type DmPayload = { from_pubkey: string; from_name: string; text: string };
export type PeerEvent = { name: string };
export type ErrorPayload = { message: string };

export type DeviceInfo = { index: number; name: string };

export const cmd = {
  status: () => invoke<AppStatus>("app_status"),
  connect: (addr: string) => invoke<void>("connect", { addr }),
  takePendingInvite: () => invoke<InvitePayload | null>("take_pending_invite"),
  inviteLink: () => invoke<string>("invite_link"),
  disconnect: () => invoke<void>("disconnect"),
  listChannels: () => invoke<void>("list_channels"),
  createChannel: (name: string | null) => invoke<void>("create_channel", { name }),
  joinChannel: (channelId: string) => invoke<void>("join_channel", { channelId }),
  leaveChannel: () => invoke<void>("leave_channel"),
  sendChat: (text: string) => invoke<void>("send_chat", { text }),
  sendDm: (toPubkeyHex: string, text: string) =>
    invoke<void>("send_dm", { toPubkeyHex, text }),
  setName: (name: string) => invoke<void>("set_name", { name }),
  setMute: (muted: boolean) => invoke<void>("set_mute", { muted }),
  setVoiceMode: (mode: VoiceMode) => invoke<void>("set_voice_mode", { mode }),
  pttPress: () => invoke<void>("ptt_press"),
  pttRelease: () => invoke<void>("ptt_release"),
  setHotkey: (action: string, accel: string) =>
    invoke<void>("set_hotkey", { action, accel }),
  unsetHotkey: (action: string) => invoke<void>("unset_hotkey", { action }),
  listHotkeys: () => invoke<[string, string][]>("list_hotkeys"),
  // audio
  listInputDevices: () => invoke<DeviceInfo[]>("list_input_devices"),
  listOutputDevices: () => invoke<DeviceInfo[]>("list_output_devices"),
  setInputDevice: (name: string | null) => invoke<void>("set_input_device", { name }),
  setOutputDevice: (name: string | null) => invoke<void>("set_output_device", { name }),
  setInputGain: (pct: number) => invoke<void>("set_input_gain", { pct }),
  setOutputVolume: (pct: number) => invoke<void>("set_output_volume", { pct }),
  setVadLevel: (pct: number) => invoke<void>("set_vad_level", { pct }),
  playTestSignal: (durationMs?: number) =>
    invoke<void>("play_test_signal", { durationMs: durationMs ?? 500 }),
  // platform integration
  setNotifications: (enabled: boolean) => invoke<void>("set_notifications", { enabled }),
  setAutostart: (enabled: boolean) => invoke<void>("set_autostart", { enabled }),
  setCloseToTray: (enabled: boolean) => invoke<void>("set_close_to_tray", { enabled }),
  setLanguage: (lang: string) => invoke<void>("set_language", { lang }),
  // server registry
  listServers: () => invoke<ServerView[]>("list_servers"),
  forgetServer: (addr: string) => invoke<void>("forget_server", { addr }),
  setServerFavourite: (addr: string, favourite: boolean) =>
    invoke<void>("set_server_favourite", { addr, favourite }),
  labelServer: (addr: string, label: string | null) =>
    invoke<void>("label_server", { addr, label }),
  // history
  getHistory: (server: string, channel: string, limit?: number) =>
    invoke<ChatLine[]>("get_history", { server, channel, limit }),
  clearHistory: (server: string, channel: string) =>
    invoke<void>("clear_history", { server, channel }),
  // DM
  listDmPeers: () => invoke<DmPeerView[]>("list_dm_peers"),
  forgetDmPeer: (pubkeyHex: string) => invoke<void>("forget_dm_peer", { pubkeyHex }),
  getDmHistory: (pubkeyHex: string, limit?: number) =>
    invoke<ChatLine[]>("get_dm_history", { pubkeyHex, limit }),
  sendDmTo: (pubkeyHex: string, text: string) =>
    invoke<void>("send_dm_to", { pubkeyHex, text }),
  resolvePeer: (query: string) => invoke<string | null>("resolve_peer", { query }),
  // backup / restore
  exportSettings: () => invoke<string>("export_settings"),
  importSettings: (json: string) => invoke<void>("import_settings", { json }),
  // dev logs
  setDevLogs: (enabled: boolean) => invoke<boolean>("set_dev_logs", { enabled }),
};

export type DmPeerView = {
  pubkey_hex: string;
  name: string;
  last_seen_unix: number;
};

export type ServerView = {
  addr: string;
  label: string | null;
  last_channel: string | null;
  favourite: boolean;
  last_used_unix: number;
};

export type ChatLine = {
  ts_unix: number;
  kind: "chat" | "dm";
  from: string;
  text: string;
};

export type SpeakerLevel = { name: string; level: number };

export type VoiceLevelPayload = {
  input_peak: number;
  speakers: SpeakerLevel[];
  loss_percent: number;
  tier: string;
  tx_kbps: number;
  rx_kbps: number;
  muted: boolean;
  /** false while the server has not confirmed UDP registration (no inbound voice) */
  registered: boolean;
  /** microphone half is live (false = listen-only call) */
  capture_ok: boolean;
  /** output half is live (false = speak-only call) */
  playback_ok: boolean;
};

export type DeviceAddedPayload = {
  kind: "input" | "output";
  name: string;
};

export type InvitePayload = {
  addr: string;
  channel: string | null;
};

export type DevLogPayload = {
  level: string;
  target: string;
  message: string;
  ts_ms: number;
};

export type EventMap = {
  connection_state: ConnState;
  joined_channel: JoinedPayload;
  left_channel: Record<string, never>;
  peer_joined: PeerEvent;
  peer_left: PeerEvent;
  chat_message: ChatPayload;
  direct_message: DmPayload;
  channel_list: ChannelListPayload;
  name_changed: { old_name: string; new_name: string };
  error: ErrorPayload;
  voice_level: VoiceLevelPayload;
  voice_stopped: Record<string, never>;
  device_added: DeviceAddedPayload;
  quick_join: Record<string, never>;
  log: { text: string };
  muted_changed: { muted: boolean };
  invite: InvitePayload;
  dev_log: DevLogPayload;
};

export function on<K extends keyof EventMap>(
  event: K,
  handler: (payload: EventMap[K]) => void,
): Promise<UnlistenFn> {
  return listen<EventMap[K]>(event, (e) => handler(e.payload));
}
