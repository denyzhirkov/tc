// Subscribe to backend events and push them into the Solid store.
import type { UnlistenFn } from "@tauri-apps/api/event";
import { cmd, on } from "./tauri";
import { pushLog, state, update } from "./store";

export async function subscribeAll(): Promise<UnlistenFn[]> {
  return await Promise.all([
    on("connection_state", (s) => {
      if (s.state === "connecting") {
        update.conn("connecting");
        // Switching servers — wipe the previous server's channels/peers so
        // the sidebar doesn't show stale rows from the old session while we
        // wait for the new server's `channel_list` to arrive.
        update.channel(null);
        update.channelList([]);
        update.speakers({});
        update.voice(null);
        pushLog("connecting...", "system");
      } else if (s.state === "connected") {
        update.conn("connected");
        update.serverAddr(s.server);
        pushLog(`connected to ${s.server}`, "system");
        cmd.listChannels().catch(() => {});
      } else if (s.state === "reconnecting") {
        // Connection dropped; backend is retrying. Keep server addr but clear
        // channel/voice state so the UI reflects we're not actually live.
        update.conn("reconnecting");
        update.channel(null);
        update.channelList([]);
        update.speakers({});
        update.voice(null);
        pushLog(
          `reconnecting (attempt ${s.attempt}, in ${s.delay_secs}s)…`,
          "system",
        );
      } else {
        update.conn("disconnected");
        update.channel(null);
        update.channelList([]);
        update.speakers({});
        update.voice(null);
        pushLog("disconnected", "system");
      }
    }),
    on("joined_channel", async (p) => {
      update.channel(p.channel_id, p.participants);
      pushLog(`joined #${p.channel_id} (${p.participants.length} peers)`, "system");
      await loadChannelHistory(state.serverAddr, p.channel_id);
    }),
    on("left_channel", () => {
      update.channel(null);
      pushLog("left channel", "system");
    }),
    on("peer_joined", (p) => {
      update.addParticipant(p.name);
      pushLog(`* ${p.name} joined`, "system");
    }),
    on("peer_left", (p) => {
      update.removeParticipant(p.name);
      pushLog(`* ${p.name} left`, "system");
    }),
    on("chat_message", (p) =>
      pushLog(`${p.from}: ${p.text}`, "chat", Math.floor(Date.now() / 1000)),
    ),
    on("direct_message", (p) => {
      // If the user is currently viewing this DM thread, append to the thread.
      // Otherwise drop a notice into the main log so they can see something arrived.
      const ts = Math.floor(Date.now() / 1000);
      if (state.dm && state.dm.pubkey_hex === p.from_pubkey) {
        update.pushDmLog(`${p.from_name}: ${p.text}`, "dm", ts);
      } else {
        pushLog(`(dm from ${p.from_name}) ${p.text}`, "dm", ts);
      }
    }),
    on("channel_list", (p) => {
      update.channelList(p.channels);
    }),
    on("error", (p) => pushLog(`error: ${p.message}`, "error")),
    on("name_changed", (p) => {
      pushLog(`* ${p.old_name} → ${p.new_name}`, "system");
      update.renameParticipant(p.old_name, p.new_name);
    }),
    on("voice_level", (v) => {
      update.voice(v);
      if (state.status) state.status.muted = v.muted;
    }),
    on("muted_changed", (p) => {
      if (state.status) state.status.muted = p.muted;
    }),
    on("log", (p) => pushLog(p.text, "system")),
    on("quick_join", () => pushLog("hotkey: quick_join (no target set)", "system")),
    on("invite", async (p) => {
      pushLog(`invite: ${p.addr}${p.channel ? " #" + p.channel : ""}`, "system");
      try {
        await cmd.connect(p.addr);
        if (p.channel) {
          // Wait briefly for the connection to settle, then join.
          setTimeout(() => {
            cmd.joinChannel(p.channel!).catch(() => {});
          }, 400);
        }
      } catch (e) {
        pushLog(`invite failed: ${e}`, "error");
      }
    }),
  ]);
}

/// Pull the current backend snapshot — call once after `subscribeAll`.
export async function fetchInitialStatus() {
  try {
    const s = await cmd.status();
    update.status(s);
    if (s.connected) update.conn("connected");
    if (s.server_addr) update.serverAddr(s.server_addr);
    if (s.channel) update.channel(s.channel, []);
  } catch (e) {
    console.warn("status fetch failed", e);
  }
}

/// Load saved chat history for the current channel and dump it into the log
/// as a header followed by chat lines.
export async function loadChannelHistory(server: string, channel: string) {
  try {
    const lines = await cmd.getHistory(server, channel, 50);
    if (lines.length === 0) return;
    for (const l of lines) {
      pushLog(
        `${l.from}: ${l.text}`,
        l.kind === "dm" ? "dm" : "chat",
        l.ts_unix,
      );
    }
  } catch (e) {
    console.warn("history load failed", e);
  }
}
