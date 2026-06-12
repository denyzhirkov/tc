// Subscribe to backend events and push them into the Solid store.
import type { UnlistenFn } from "@tauri-apps/api/event";
import { cmd, on, type InvitePayload } from "./tauri";
import { pushLog, state, update } from "./store";
import { t } from "./i18n";

/// Timestamp of the last `voice_level` event. The watchdog below zeroes the
/// voice UI if the stream of events stalls (backend pump stuck or dead) so
/// speaker waves never animate forever from a frozen snapshot.
let lastVoiceLevelAt = 0;
const VOICE_LEVEL_STALL_MS = 1000;

/// Hot-plugged devices the user declined this session ("kind:name").
export const dismissedDevices = new Set<string>();

export async function subscribeAll(): Promise<UnlistenFn[]> {
  setInterval(() => {
    if (state.voice && Date.now() - lastVoiceLevelAt > VOICE_LEVEL_STALL_MS) {
      update.voice(null);
    }
  }, 500);
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
        pushLog(t("conn.connecting"), "system");
      } else if (s.state === "connected") {
        update.conn("connected");
        update.serverAddr(s.server);
        pushLog(t("conn.connected", { server: s.server }), "system");
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
          t("conn.reconnecting", { attempt: s.attempt, delay: s.delay_secs }),
          "system",
        );
      } else {
        update.conn("disconnected");
        update.channel(null);
        update.channelList([]);
        update.speakers({});
        update.voice(null);
        pushLog(t("conn.disconnected"), "system");
      }
    }),
    on("joined_channel", async (p) => {
      update.channel(p.channel_id, p.participants);
      pushLog(
        t("log.joined_channel", {
          channel: p.channel_id,
          count: p.participants.length,
        }),
        "system",
      );
      await loadChannelHistory(state.serverAddr, p.channel_id);
      // Backend auto-mutes on join — pull a fresh status so the mic icon
      // reflects the new muted state without waiting for the next user action.
      try {
        update.status(await cmd.status());
      } catch {}
    }),
    on("left_channel", () => {
      update.channel(null);
      update.voice(null);
      pushLog(t("log.left_channel"), "system");
    }),
    on("peer_joined", (p) => {
      update.addParticipant(p.name);
      pushLog(t("log.peer_joined", { name: p.name }), "system");
    }),
    on("peer_left", (p) => {
      update.removeParticipant(p.name);
      pushLog(t("log.peer_left", { name: p.name }), "system");
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
        pushLog(t("log.dm_from", { name: p.from_name, text: p.text }), "dm", ts);
      }
    }),
    on("channel_list", (p) => {
      update.channelList(p.channels);
    }),
    on("error", (p) => pushLog(t("log.error", { message: p.message }), "error")),
    on("name_changed", (p) => {
      pushLog(t("log.name_changed", { old: p.old_name, new: p.new_name }), "system");
      update.renameParticipant(p.old_name, p.new_name);
    }),
    on("voice_level", (v) => {
      lastVoiceLevelAt = Date.now();
      update.voice(v);
      if (state.status) state.status.muted = v.muted;
    }),
    on("voice_stopped", () => {
      update.voice(null);
    }),
    on("device_added", (d) => {
      // "No" is remembered per session so the same device doesn't nag again.
      if (dismissedDevices.has(`${d.kind}:${d.name}`)) return;
      update.devicePrompt(d);
    }),
    on("muted_changed", (p) => {
      if (state.status) state.status.muted = p.muted;
    }),
    on("log", (p) => pushLog(p.text, "system")),
    on("dev_log", (p) => {
      if (!state.devLogs) return;
      const d = new Date(p.ts_ms);
      const hh = String(d.getHours()).padStart(2, "0");
      const mm = String(d.getMinutes()).padStart(2, "0");
      const ss = String(d.getSeconds()).padStart(2, "0");
      const ms = String(d.getMilliseconds()).padStart(3, "0");
      pushLog(
        `${hh}:${mm}:${ss}.${ms} ${p.level.padEnd(5)} ${p.target} — ${p.message}`,
        "devlog",
      );
    }),
    on("quick_join", () => pushLog("hotkey: quick_join (no target set)", "system")),
    on("invite", handleInvite),
  ]);
}

async function handleInvite(p: InvitePayload) {
  pushLog(`invite: ${p.addr}${p.channel ? " #" + p.channel : ""}`, "system");
  // A link can point anywhere — connecting reveals our IP and pubkey and
  // TOFU-pins the server's cert, so an unknown server needs explicit consent.
  let known = false;
  try {
    known = (await cmd.listServers()).some((s) => s.addr === p.addr);
  } catch {}
  if (!known) {
    update.invitePrompt(p);
    return false;
  }
  await acceptInvite(p);
  return true;
}

/// Connect + join for an invite the user trusts (known server, or confirmed
/// via the invite prompt).
export async function acceptInvite(p: InvitePayload) {
  try {
    // connect() resolves only once the TLS session is up, so the join is
    // ordered behind it on the same stream — no settling delay needed.
    await cmd.connect(p.addr);
    if (p.channel) await cmd.joinChannel(p.channel);
  } catch (e) {
    pushLog(t("log.invite_failed", { error: String(e) }), "error");
  }
}

/// Drain the invite buffered before the event listeners were up (the app was
/// launched by clicking a tc:// link). Call once after `subscribeAll`.
/// Returns true if the invite connected immediately — the caller should then
/// skip auto-connect. A prompt awaiting confirmation returns false so the
/// usual auto-connect still happens underneath it.
export async function drainPendingInvite(): Promise<boolean> {
  try {
    const p = await cmd.takePendingInvite();
    if (p) return await handleInvite(p);
  } catch {}
  return false;
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
